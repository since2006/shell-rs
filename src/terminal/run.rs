//! 在已开的终端里执行: what `shellrs exec --terminal` types into an open
//! remote terminal, and how the result is read back out of its output.
//!
//! A bastion host that opened ShellRS with a link serves one login, and
//! ends the session when another channel opens on it (see
//! `HostLogin::shell_only`). An agent's command can then only reach the
//! host the way the user's own do: typed into the shell. A run goes:
//!
//! 1. ^C, which ends what runs in the foreground and empties the input
//!    line, in bash, dash and busybox ash alike. Nothing checks first what
//!    the terminal is doing: whoever hands a tab to an agent knows.
//! 2. A moment later, a probe, whose answer (marker P) says the shell
//!    reads lines again and which shell it is.
//! 3. Once P comes, the command line. It prints marker B with where it
//!    runs, then the command's output through a pipe, then marker E with
//!    the exit code.
//!
//! The markers are an OSC sequence alacritty ignores, with the run's
//! nonce an argument of its own. The echo of a typed line has `\033` as
//! four characters, never an escape, so only the run's own `printf` makes
//! them.

use std::io;
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::time::{Duration, Instant};

use rand::{Rng as _, distr::Alphanumeric};

use super::TerminalTransportCommand;

/// What every marker starts with: an OSC number no terminal uses.
const MARKER: &[u8] = b"\x1b]6973;";
/// The longest marker waited for: room for a context whose directory is
/// as long as paths get, in hex.
const MARKER_LIMIT: usize = 16 * 1024;
const NONCE_LENGTH: usize = 12;

/// Prints P with the nonce and the program the interactive shell runs:
/// `sh -c`'s parent is that shell. Not `/proc/<pid>/comm`, which says `sh`
/// for a bash started as sh.
const PROBE_SCRIPT: &str =
    r#"printf "\033]6973;P;%s;%s\007" "$1" "$(readlink /proc/$PPID/exe 2>/dev/null)""#;

/// Runs the command (`$2`, as `printf %b` reads it) with bash where there
/// is one and sh where not, its output through a pipe so that programs
/// see no terminal: no colors, no pager. B comes first with the user, the
/// host, the directory and the interpreter, in hex so that no path can
/// break the marker; E comes last, printed by this shell after the pipe.
/// It survives a ^C (`trap : INT`) while the command, in a subshell whose
/// trap is reset, does not; the status comes out on fd 3, and none means
/// the command was interrupted. No `'`, `!` or `\\` in it: it is typed in
/// single quotes.
const COMMAND_SCRIPT: &str = concat!(
    r#"trap : INT; i=$(command -v bash || echo sh); "#,
    r#"c=$(printf "%s\n%s\n%s\n%s" "$(id -un)" "$(uname -n)" "$PWD" "${i##*/}" "#,
    r#"| od -An -tx1 2>/dev/null | tr -d " \n"); "#,
    r#"printf "\033]6973;B;%s;%s\007" "$1" "$c"; exec 4>&1; "#,
    r#"s=$( { { "$i" -c "$(printf %b "$2")" </dev/null 2>&1 3>&- 4>&-; printf %d $? >&3; } "#,
    r#"| cat >&4; } 3>&1 ); "#,
    r#"printf "\033]6973;E;%s;%s\007" "$1" "${s:-130}""#,
);

/// The interactive shells a command can be typed into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunShell {
    Bash,
    Dash,
    Busybox,
}

impl RunShell {
    /// The shell whose program is `exe`, as `/proc/<pid>/exe` reads.
    pub fn of(exe: &str) -> Option<Self> {
        match program_name(exe) {
            "bash" => Some(Self::Bash),
            "dash" => Some(Self::Dash),
            "busybox" => Some(Self::Busybox),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Dash => "dash",
            Self::Busybox => "busybox",
        }
    }

    /// The longest command line it takes, Enter included. The line is
    /// typed ahead while the probe still runs, when the terminal holds a
    /// line of 4095 bytes at most; busybox's own line editor holds 1024.
    pub fn line_limit(self) -> usize {
        match self {
            Self::Busybox => 1000,
            Self::Bash | Self::Dash => 4000,
        }
    }
}

/// The file name of `exe`, without the mark Linux adds once the program
/// was upgraded under a running shell.
fn program_name(exe: &str) -> &str {
    let exe = exe.trim();
    let exe = exe.strip_suffix(" (deleted)").unwrap_or(exe);
    exe.rsplit('/').next().unwrap_or(exe)
}

/// Where the command ran.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunContext {
    pub user: String,
    pub host: String,
    pub cwd: String,
    /// The interactive shell it was typed into.
    pub shell: String,
    /// What ran it: `bash`, or `sh` where there is no bash.
    pub interp: String,
}

impl RunContext {
    /// The context from marker B's hex: user, host, directory and
    /// interpreter, a line each. A directory may hold newlines itself, so
    /// it is what is left between the first two and the last.
    fn read(hex: &[u8], shell: &str) -> Self {
        let bytes: Vec<u8> = hex
            .as_chunks::<2>()
            .0
            .iter()
            .filter_map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
            .collect();
        let text = String::from_utf8_lossy(&bytes);
        let mut context = Self {
            shell: shell.to_owned(),
            ..Self::default()
        };
        let mut parts = text.splitn(3, '\n');
        context.user = parts.next().unwrap_or_default().to_owned();
        context.host = parts.next().unwrap_or_default().to_owned();
        if let Some((cwd, interp)) = parts.next().unwrap_or_default().rsplit_once('\n') {
            context.cwd = cwd.to_owned();
            context.interp = interp.to_owned();
        }
        context
    }
}

/// What a run tells the request it serves, in order.
#[derive(Debug, PartialEq, Eq)]
pub enum RunEvent {
    /// A terminal took the run up and sent its ^C.
    Accepted,
    /// The shell answered the probe and the command line was typed.
    Typed,
    /// The command started.
    Begun(RunContext),
    Output(Vec<u8>),
    /// The command ended with this exit code.
    Exited(i32),
    Failed(RunFailure),
}

/// Why a run did not run, or did not finish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunFailure {
    HostNotFound,
    /// The host has no terminal open.
    NoTerminal,
    /// Its terminals are not connected.
    NotConnected,
    /// The program of the interactive shell; empty when it could not be
    /// read (no `/proc`).
    UnsupportedShell(String),
    TooLong {
        limit: usize,
    },
    Busy(BusyReason),
    /// The terminal went away after the command was typed: it may have
    /// run, in part.
    Disconnected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusyReason {
    /// A full-screen program has the screen, which ^C does not end.
    FullScreen,
    /// Another run has the terminal.
    AnotherRun,
    /// Nothing answered the probe.
    NoAnswer,
    /// The shell did not start the command.
    NotStarted,
}

/// How long each step of a run may take.
#[derive(Clone, Copy, Debug)]
pub struct RunTiming {
    /// After the ^C, for the shell to show its prompt again: bytes coming
    /// with the ^C may go with it.
    pub settle: Duration,
    pub probe: Duration,
    /// From typing the command to its marker B. A bastion host's command
    /// filter or review may hold it back.
    pub start: Duration,
    /// How often the caller hears of a run that prints nothing.
    pub tick: Duration,
}

impl RunTiming {
    pub const STANDARD: Self = Self {
        settle: Duration::from_millis(500),
        probe: Duration::from_secs(5),
        start: Duration::from_secs(15),
        tick: Duration::from_secs(1),
    };
}

/// What a run hands its caller while it runs.
#[derive(Debug, PartialEq, Eq)]
pub enum RunProgress {
    Context(RunContext),
    Output(Vec<u8>),
    /// Nothing happened for a tick.
    Idle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Waiting for a terminal to take it up.
    Queued,
    /// ^C sent.
    Installed,
    Probing,
    Typed,
    Begun,
    Done,
    Abandoned,
}

impl Phase {
    fn is_active(self) -> bool {
        matches!(
            self,
            Phase::Installed | Phase::Probing | Phase::Typed | Phase::Begun
        )
    }
}

struct RunState {
    nonce: String,
    command: String,
    phase: Phase,
    shell: Option<RunShell>,
    scanner: RunScanner,
    /// The terminal's input, once one took the run up.
    input: Option<mpsc::Sender<TerminalTransportCommand>>,
    events: mpsc::Sender<RunEvent>,
}

impl RunState {
    fn type_bytes(&self, bytes: impl Into<Vec<u8>>) {
        if let Some(input) = &self.input {
            let _ = input.send(TerminalTransportCommand::Write(bytes.into()));
        }
    }

    fn tell(&self, event: RunEvent) {
        let _ = self.events.send(event);
    }

    fn fail(&mut self, failure: RunFailure) {
        self.phase = Phase::Abandoned;
        self.tell(RunEvent::Failed(failure));
    }

    /// The shell answered the probe: type the command, if it is one the
    /// line can be typed into and the line fits.
    fn probed(&mut self, exe: &str) {
        let Some(shell) = RunShell::of(exe) else {
            self.fail(RunFailure::UnsupportedShell(program_name(exe).to_owned()));
            return;
        };
        let mut line = command_line(&self.nonce, &self.command);
        line.push('\r');
        if line.len() > shell.line_limit() {
            self.fail(RunFailure::TooLong {
                limit: shell.line_limit(),
            });
            return;
        }
        self.type_bytes(line);
        self.shell = Some(shell);
        self.phase = Phase::Typed;
        self.tell(RunEvent::Typed);
    }
}

/// One `exec --terminal` run, as the request it serves holds it. Dropping
/// it gives the run up, with a ^C when its command was typed.
pub struct TerminalRun {
    state: Arc<Mutex<RunState>>,
}

/// The same run, as the terminal it runs in holds it.
#[derive(Clone)]
pub struct RunHandle {
    state: Arc<Mutex<RunState>>,
}

fn lock(state: &Mutex<RunState>) -> MutexGuard<'_, RunState> {
    state.lock().unwrap_or_else(|error| error.into_inner())
}

impl TerminalRun {
    pub fn new(command: String) -> (Self, mpsc::Receiver<RunEvent>) {
        let nonce: String = rand::rng()
            .sample_iter(Alphanumeric)
            .take(NONCE_LENGTH)
            .map(char::from)
            .collect();
        let (events, receiver) = mpsc::channel();
        let state = RunState {
            scanner: RunScanner::new(&nonce),
            nonce,
            command,
            phase: Phase::Queued,
            shell: None,
            input: None,
            events,
        };
        (
            Self {
                state: Arc::new(Mutex::new(state)),
            },
            receiver,
        )
    }

    pub fn handle(&self) -> RunHandle {
        RunHandle {
            state: self.state.clone(),
        }
    }

    /// Type the probe, once the terminal has sent its ^C.
    pub fn type_probe(&self) {
        let mut state = lock(&self.state);
        if state.phase == Phase::Installed {
            let mut line = probe_line(&state.nonce);
            line.push('\r');
            state.type_bytes(line);
            state.phase = Phase::Probing;
        }
    }

    /// Give the run up for `reason`, unless it got past that meanwhile:
    /// whether it was given up. A command typed and not started gets a ^C,
    /// which also withdraws one a bastion host holds for review.
    pub fn give_up(&self, reason: BusyReason) -> bool {
        let mut state = lock(&self.state);
        let given_up = match reason {
            BusyReason::NotStarted => state.phase == Phase::Typed,
            _ => matches!(
                state.phase,
                Phase::Queued | Phase::Installed | Phase::Probing
            ),
        };
        if given_up {
            if state.phase == Phase::Typed {
                state.type_bytes(b"\x03".to_vec());
            }
            state.phase = Phase::Abandoned;
        }
        given_up
    }

    /// Give the run up whatever it does: ^C to a command typed and not
    /// finished.
    pub fn cancel(&self) {
        let mut state = lock(&self.state);
        match state.phase {
            Phase::Done | Phase::Abandoned => return,
            Phase::Typed | Phase::Begun => state.type_bytes(b"\x03".to_vec()),
            Phase::Queued | Phase::Installed | Phase::Probing => {}
        }
        state.phase = Phase::Abandoned;
    }

    /// See the run through once a terminal took it up (after
    /// [`RunEvent::Accepted`]): probe, wait for the command, and hand what
    /// it prints to `progress`, with [`RunProgress::Idle`] every quiet
    /// tick. The exit code, or why there is none. When `progress` fails,
    /// its reader is gone: the run is cancelled, and given up as
    /// [`RunFailure::Disconnected`], which nobody hears.
    pub fn drive(
        &self,
        events: &mpsc::Receiver<RunEvent>,
        timing: RunTiming,
        progress: &mut dyn FnMut(RunProgress) -> io::Result<()>,
    ) -> Result<i32, RunFailure> {
        std::thread::sleep(timing.settle);
        self.type_probe();
        let mut deadline = Some((Instant::now() + timing.probe, BusyReason::NoAnswer));
        loop {
            let next = match events.recv_timeout(timing.tick) {
                Ok(RunEvent::Accepted) => continue,
                Ok(RunEvent::Typed) => {
                    deadline = Some((Instant::now() + timing.start, BusyReason::NotStarted));
                    continue;
                }
                Ok(RunEvent::Begun(context)) => {
                    deadline = None;
                    RunProgress::Context(context)
                }
                Ok(RunEvent::Output(bytes)) => RunProgress::Output(bytes),
                Ok(RunEvent::Exited(code)) => return Ok(code),
                Ok(RunEvent::Failed(failure)) => return Err(failure),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Some((at, reason)) = deadline
                        && Instant::now() >= at
                    {
                        if self.give_up(reason) {
                            return Err(RunFailure::Busy(reason));
                        }
                        // It got further just now: its event is on the way.
                        deadline = None;
                    }
                    RunProgress::Idle
                }
                // The run itself holds the sender.
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(RunFailure::Disconnected),
            };
            if progress(next).is_err() {
                self.cancel();
                return Err(RunFailure::Disconnected);
            }
        }
    }
}

impl Drop for TerminalRun {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl RunHandle {
    /// Take the run up in a terminal whose input is `input`; the ^C is the
    /// terminal's to send. False when the request gave it up already.
    pub fn accept(&self, input: mpsc::Sender<TerminalTransportCommand>) -> bool {
        let mut state = lock(&self.state);
        if state.phase != Phase::Queued {
            return false;
        }
        state.input = Some(input);
        state.phase = Phase::Installed;
        state.tell(RunEvent::Accepted);
        true
    }

    /// Say why no terminal can take the run up.
    pub fn fail(&self, failure: RunFailure) {
        let mut state = lock(&self.state);
        if state.phase == Phase::Queued {
            state.fail(failure);
        }
    }

    /// Whether the run still has its terminal: one given up does not,
    /// whatever its command still does.
    pub(super) fn is_active(&self) -> bool {
        lock(&self.state).phase.is_active()
    }

    /// Read the terminal's output for the run's markers and output;
    /// whether the run goes on.
    pub fn feed(&self, bytes: &[u8]) -> bool {
        let mut state = lock(&self.state);
        if !state.phase.is_active() {
            return false;
        }
        for scanned in state.scanner.scan(bytes) {
            match (scanned, state.phase) {
                (Scanned::Probe(exe), Phase::Probing) => state.probed(&exe),
                (Scanned::Begin(hex), Phase::Typed) => {
                    let shell = state.shell.map(RunShell::name).unwrap_or_default();
                    state.phase = Phase::Begun;
                    state.tell(RunEvent::Begun(RunContext::read(&hex, shell)));
                }
                (Scanned::Output(bytes), Phase::Begun) => state.tell(RunEvent::Output(bytes)),
                (Scanned::End(code), Phase::Begun) => {
                    state.phase = Phase::Done;
                    state.tell(RunEvent::Exited(code));
                }
                _ => {}
            }
        }
        state.phase.is_active()
    }

    /// The terminal's connection is gone.
    pub(super) fn disconnected(&self) {
        let mut state = lock(&self.state);
        match state.phase {
            Phase::Installed | Phase::Probing => state.fail(RunFailure::NotConnected),
            Phase::Typed | Phase::Begun => state.fail(RunFailure::Disconnected),
            Phase::Queued | Phase::Done | Phase::Abandoned => {}
        }
    }
}

/// The probe line, with a leading space so that bash with `ignorespace`
/// keeps it out of the history.
fn probe_line(nonce: &str) -> String {
    format!(" sh -c '{PROBE_SCRIPT}' sh {nonce}")
}

/// The command line: `command` as [`encode_command`] writes it.
fn command_line(nonce: &str, command: &str) -> String {
    format!(
        " sh -c '{COMMAND_SCRIPT}' sh {nonce} '{}'",
        encode_command(command.as_bytes())
    )
}

/// `command` as `printf %b` reads it back, with nothing in it a shell's
/// line editor or single quotes would take for their own: control
/// characters, quotes, `\`, `!` (csh-style history) and every byte above
/// ASCII (Meta keys to readline) as `\0NNN`. The rest reads as it is, for
/// the user and a bastion host's command audit alike.
pub(super) fn encode_command(command: &[u8]) -> String {
    let mut encoded = String::with_capacity(command.len());
    for &byte in command {
        if !(0x20..0x7f).contains(&byte) || matches!(byte, b'\'' | b'\\' | b'!' | b'"') {
            encoded.push_str(&format!("\\0{byte:03o}"));
        } else {
            encoded.push(char::from(byte));
        }
    }
    encoded
}

/// What the scanner found in the output, in order.
#[derive(Debug, PartialEq, Eq)]
enum Scanned {
    /// Marker P, with the interactive shell's program.
    Probe(String),
    /// Marker B, with the context in hex.
    Begin(Vec<u8>),
    /// What the command printed, between B and E, with CRLF as LF.
    Output(Vec<u8>),
    End(i32),
}

/// Finds a run's markers in terminal output, however it is split into
/// chunks, and what the command printed between B and E.
struct RunScanner {
    nonce: Vec<u8>,
    /// What may still turn out to be (part of) a marker.
    pending: Vec<u8>,
    capturing: bool,
    /// A CR the next chunk may make CRLF.
    carriage_return: bool,
}

impl RunScanner {
    fn new(nonce: &str) -> Self {
        Self {
            nonce: nonce.as_bytes().to_vec(),
            pending: Vec::new(),
            capturing: false,
            carriage_return: false,
        }
    }

    fn scan(&mut self, bytes: &[u8]) -> Vec<Scanned> {
        let mut text = std::mem::take(&mut self.pending);
        text.extend_from_slice(bytes);
        let mut found = Vec::new();
        let mut output = Vec::new();
        let mut rest = text.as_slice();
        loop {
            let Some(at) = find(rest, MARKER) else {
                let kept = partial_marker(rest);
                self.capture(&rest[..rest.len() - kept], &mut output);
                self.pending = rest[rest.len() - kept..].to_vec();
                break;
            };
            self.capture(&rest[..at], &mut output);
            let body = &rest[at + MARKER.len()..];
            match terminator(body) {
                Some((end, length)) => {
                    if let Some(marker) = self.marker(&body[..end], &mut output) {
                        if !output.is_empty() {
                            found.push(Scanned::Output(std::mem::take(&mut output)));
                        }
                        found.push(marker);
                    }
                    rest = &body[end + length..];
                }
                // Too long to be one: not a marker after all.
                None if body.len() > MARKER_LIMIT => {
                    self.capture(&rest[at..at + MARKER.len()], &mut output);
                    rest = body;
                }
                None => {
                    self.pending = rest[at..].to_vec();
                    break;
                }
            }
        }
        if !output.is_empty() {
            found.push(Scanned::Output(output));
        }
        found
    }

    /// Keep `bytes` when they are the command's, with CRLF as LF.
    fn capture(&mut self, bytes: &[u8], output: &mut Vec<u8>) {
        if !self.capturing {
            return;
        }
        for &byte in bytes {
            if std::mem::take(&mut self.carriage_return) && byte != b'\n' {
                output.push(b'\r');
            }
            if byte == b'\r' {
                self.carriage_return = true;
            } else {
                output.push(byte);
            }
        }
    }

    /// The marker `body` (after `ESC ] 6973 ;`) is, if it is this run's.
    fn marker(&mut self, body: &[u8], output: &mut Vec<u8>) -> Option<Scanned> {
        let mut fields = body.splitn(3, |byte| *byte == b';');
        let (kind, nonce, payload) = (fields.next()?, fields.next()?, fields.next()?);
        if nonce != self.nonce.as_slice() {
            return None;
        }
        match kind {
            b"P" => Some(Scanned::Probe(
                String::from_utf8_lossy(payload).into_owned(),
            )),
            b"B" => {
                self.capturing = true;
                Some(Scanned::Begin(payload.to_vec()))
            }
            b"E" => {
                if std::mem::take(&mut self.carriage_return) {
                    output.push(b'\r');
                }
                self.capturing = false;
                // Never anything but digits; the run ends either way.
                let code = std::str::from_utf8(payload)
                    .ok()
                    .and_then(|code| code.trim().parse().ok())
                    .unwrap_or(255);
                Some(Scanned::End(code))
            }
            _ => None,
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// How many bytes at the end of `bytes` begin a marker.
fn partial_marker(bytes: &[u8]) -> usize {
    (1..MARKER.len())
        .rev()
        .find(|&length| length <= bytes.len() && bytes.ends_with(&MARKER[..length]))
        .unwrap_or(0)
}

/// Where an OSC body ends, and how long its terminator is: BEL or ST.
fn terminator(body: &[u8]) -> Option<(usize, usize)> {
    body.iter()
        .enumerate()
        .find_map(|(index, byte)| match byte {
            0x07 => Some((index, 1)),
            0x1b if body.get(index + 1) == Some(&b'\\') => Some((index, 2)),
            _ => None,
        })
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
