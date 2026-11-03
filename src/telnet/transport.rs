//! A Telnet host's terminal: the route to it, taken as an SSH host's is,
//! the Telnet protocol on top, 自动登录 watching what comes back, and the
//! line edited here while the server does not echo.

use std::sync::{Arc, mpsc};
use std::time::Duration;

use anyhow::{Result, anyhow};
use async_channel::Sender;
use tokio::io::{AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::sync::mpsc as tokio_mpsc;
use tokio::time::Instant;

use super::auto_login::AutoLogin;
use super::local_line::LocalLine;
use super::protocol::TelnetSession;
use crate::host::TelnetLogin;
use crate::i18n::t;
use crate::ssh::{SshConnector, SshPrompts, describe_login_error, run_terminal_worker};
use crate::terminal::{
    SharedTerminalTransportFactory, TelnetTerminalTransportProvider, TerminalSize,
    TerminalTransport, TerminalTransportCommand, TerminalTransportEvent, TerminalTransportFactory,
};

/// How long the device has to stop printing before 自动登录 answers a
/// prompt: one is the last thing on its line while the device waits.
const SETTLE: Duration = Duration::from_millis(100);

/// How long closing the connection may take before it is dropped instead.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// Telnet hosts' terminals, through the SSH connector for the route: its
/// jump hosts, proxy and keychain.
pub struct TelnetTransportProvider {
    connector: SshConnector,
}

impl TelnetTransportProvider {
    pub fn new(connector: SshConnector) -> Self {
        Self { connector }
    }
}

impl TelnetTerminalTransportProvider for TelnetTransportProvider {
    fn factory_for(&self, login: &TelnetLogin) -> SharedTerminalTransportFactory {
        Arc::new(TelnetTransport {
            login: login.clone(),
            connector: self.connector.clone(),
        })
    }
}

/// One host's connection. It is its own factory: every launch and restart
/// runs a fresh copy.
#[derive(Clone)]
struct TelnetTransport {
    login: TelnetLogin,
    connector: SshConnector,
}

impl TerminalTransportFactory for TelnetTransport {
    fn create(&self) -> Box<dyn TerminalTransport> {
        Box::new(self.clone())
    }
}

impl TerminalTransport for TelnetTransport {
    fn run(
        self: Box<Self>,
        initial_size: TerminalSize,
        commands: mpsc::Receiver<TerminalTransportCommand>,
        events: Sender<TerminalTransportEvent>,
    ) -> Result<()> {
        // The keychain blocks, so the password is read here, on the
        // transport's thread, before anything connects. One that cannot
        // be read is one the user types.
        let auto_login = self.login.auto_login.as_ref().map(|auto_login| {
            let password = auto_login
                .password
                .as_ref()
                .and_then(|secret| self.connector.secrets().secret(secret).ok().flatten());
            AutoLogin::new(
                &auto_login.user,
                password,
                &auto_login.user_prompt,
                &auto_login.password_prompt,
            )
        });
        run_terminal_worker(commands, events.clone(), |broker, commands| {
            self.run_async(initial_size, auto_login, broker, commands, events)
        })
    }
}

impl TelnetTransport {
    async fn run_async(
        &self,
        size: TerminalSize,
        mut auto_login: Option<AutoLogin>,
        broker: Arc<SshPrompts>,
        mut commands: tokio_mpsc::UnboundedReceiver<TerminalTransportCommand>,
        events: Sender<TerminalTransportEvent>,
    ) -> Result<()> {
        let login = &self.login;
        // Said in the terminal: down to the cause, which says what to fix.
        let stream = self
            .connector
            .open_route(
                &login.route,
                &login.host,
                login.port,
                &broker,
                self.connector.secrets(),
            )
            .await
            .map_err(|error| anyhow!(describe_login_error(&error)))?;
        let (mut reader, mut writer) = tokio::io::split(stream);
        let mut session = TelnetSession::new(dimension(size.columns()), dimension(size.rows()));
        let mut local_line = LocalLine::default();
        events
            .send(TerminalTransportEvent::Started)
            .await
            .map_err(|_| anyhow!(t!("ssh.terminal.tab_closed")))?;

        let mut buffer = vec![0; 8192];
        // When 自动登录 answers the prompt the output ends with, unless more
        // output comes first.
        let mut settle: Option<Instant> = None;
        loop {
            tokio::select! {
                read = reader.read(&mut buffer) => match read {
                    Ok(0) => break,
                    Ok(count) => {
                        let received = session.receive(&buffer[..count]);
                        send(&mut writer, &received.reply).await?;
                        if session.server_echoes() {
                            send(&mut writer, &session.send(&local_line.take())).await?;
                        }
                        if received.data.is_empty() {
                            continue;
                        }
                        if let Some(auto_login) = &mut auto_login {
                            auto_login.output(&received.data);
                            settle = auto_login.waiting().then(|| Instant::now() + SETTLE);
                        }
                        let output = TerminalTransportEvent::Output(received.data);
                        if events.send(output).await.is_err() {
                            break;
                        }
                    }
                    Err(error) => return Err(anyhow!(t!("telnet.lost", error = error))),
                },
                () = tokio::time::sleep_until(settle.unwrap_or_else(Instant::now)), if settle.is_some() => {
                    settle = None;
                    if let Some(answer) = auto_login.as_mut().and_then(AutoLogin::answer) {
                        send(&mut writer, &session.send(&answer)).await?;
                    }
                    if auto_login.as_ref().is_some_and(AutoLogin::is_done) {
                        auto_login = None;
                    }
                },
                command = commands.recv() => match command {
                    Some(TerminalTransportCommand::Write(bytes)) => {
                        if let Some(watching) = &mut auto_login {
                            watching.typed(&bytes);
                            if watching.is_done() {
                                auto_login = None;
                                settle = None;
                            }
                        }
                        if session.server_echoes() {
                            send(&mut writer, &session.send(&bytes)).await?;
                            continue;
                        }
                        let edited = local_line.edit(&bytes);
                        send(&mut writer, &session.send(&edited.send)).await?;
                        if !edited.echo.is_empty()
                            && events.send(TerminalTransportEvent::Output(edited.echo)).await.is_err()
                        {
                            break;
                        }
                    }
                    // Neither auto login nor the line edited here: the
                    // bytes are a file transfer's, not keys.
                    Some(TerminalTransportCommand::WriteData(bytes)) => {
                        send(&mut writer, &session.send(&bytes)).await?;
                    }
                    Some(TerminalTransportCommand::Resize(size)) => {
                        let bytes = session.resize(dimension(size.columns()), dimension(size.rows()));
                        send(&mut writer, &bytes).await?;
                    }
                    Some(TerminalTransportCommand::SendBreak) => {
                        send(&mut writer, &session.brk()).await?;
                    }
                    Some(TerminalTransportCommand::Shutdown) | None => {
                        let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, writer.shutdown()).await;
                        return Ok(());
                    }
                    // Nothing runs beside a Telnet terminal: a request
                    // dropped here closes its answer.
                    Some(
                        TerminalTransportCommand::Exec(_)
                        | TerminalTransportCommand::OpenSftp(_)
                        | TerminalTransportCommand::PromptReply { .. },
                    ) => {}
                },
            }
        }
        // The device closed the connection, as a shell's `exit` does.
        let _ = events
            .send(TerminalTransportEvent::Exited {
                code: 0,
                signal: None,
            })
            .await;
        Ok(())
    }
}

async fn send(writer: &mut (impl AsyncWrite + Unpin), bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    let written = async {
        writer.write_all(bytes).await?;
        writer.flush().await
    };
    written
        .await
        .map_err(|error| anyhow!(t!("telnet.write_failed", error = error)))
}

fn dimension(value: usize) -> u16 {
    value.min(usize::from(u16::MAX)) as u16
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::time::Instant;

    use super::*;
    use crate::host::{AuthKind, Host, HostDraft, HostId, TelnetSettings};
    use crate::secrets::{InMemorySecretStore, SecretRef, SecretStore as _};

    const IAC: u8 = 255;
    const SB: u8 = 250;
    const SE: u8 = 240;
    const DO: u8 = 253;
    const WILL: u8 = 251;
    const NAWS: u8 = 31;
    const ECHO: u8 = 1;

    /// Read from `socket` until `expected` has come, and keep what came
    /// after it for the next.
    fn read_until(socket: &mut std::net::TcpStream, seen: &mut Vec<u8>, expected: &[u8]) {
        let mut buffer = [0; 1024];
        loop {
            if let Some(at) = seen
                .windows(expected.len())
                .position(|window| window == expected)
            {
                seen.drain(..at + expected.len());
                return;
            }
            let count = socket
                .read(&mut buffer)
                .unwrap_or_else(|error| panic!("waiting for {expected:?}, got {seen:?}: {error}"));
            assert!(count > 0, "closed before {expected:?}; got {seen:?}");
            seen.extend_from_slice(&buffer[..count]);
        }
    }

    /// A device to connect to, or none where loopback sockets are refused.
    fn listen() -> Option<TcpListener> {
        match TcpListener::bind(("127.0.0.1", 0)) {
            Ok(listener) => Some(listener),
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("loopback sockets are unavailable in this sandbox; skipping");
                None
            }
            Err(error) => panic!("failed to bind: {error}"),
        }
    }

    type Launched = (
        mpsc::Sender<TerminalTransportCommand>,
        async_channel::Receiver<TerminalTransportEvent>,
        std::thread::JoinHandle<Result<()>>,
    );

    /// The terminal's transport for `draft`, running.
    fn launch(draft: HostDraft, keychain: Arc<InMemorySecretStore>) -> Launched {
        let directory = tempfile::tempdir().unwrap();
        let connector = SshConnector::new(directory.path().join("known_hosts"), keychain);
        let login = TelnetLogin::of(
            &Host::new(HostId(1), draft),
            &TelnetSettings::default(),
            None,
        );
        let transport = TelnetTransportProvider::new(connector)
            .factory_for(&login)
            .create();
        let (commands, receiver) = mpsc::channel();
        let (events, received) = async_channel::unbounded();
        let runner =
            std::thread::spawn(move || transport.run(TerminalSize::DEFAULT, receiver, events));
        (commands, received, runner)
    }

    #[test]
    fn it_negotiates_logs_in_and_passes_the_terminal_through() {
        let Some(listener) = listen() else { return };
        let port = listener.local_addr().unwrap().port();
        let device = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut seen = Vec::new();
            // A device negotiates first; the requests come after.
            socket.write_all(&[IAC, WILL, ECHO]).unwrap();
            read_until(&mut socket, &mut seen, &[IAC, DO, ECHO]);
            read_until(&mut socket, &mut seen, &[IAC, WILL, NAWS]);
            socket.write_all(&[IAC, DO, NAWS]).unwrap();
            read_until(
                &mut socket,
                &mut seen,
                &[IAC, SB, NAWS, 0, 80, 0, 24, IAC, SE],
            );
            socket.write_all(b"\r\nswitch login: ").unwrap();
            read_until(&mut socket, &mut seen, b"admin\r\n");
            socket.write_all(b"admin\r\nPassword: ").unwrap();
            read_until(&mut socket, &mut seen, b"hunter2\r\n");
            socket.write_all(b"\r\nWelcome\r\n$ ").unwrap();
            read_until(&mut socket, &mut seen, b"ls\r\n");
            read_until(
                &mut socket,
                &mut seen,
                &[IAC, SB, NAWS, 0, 100, 0, 30, IAC, SE],
            );
            read_until(&mut socket, &mut seen, &[IAC, 243]);
            // The device ends the session, as `exit` does.
        });

        let keychain = Arc::new(InMemorySecretStore::default());
        keychain
            .set(&SecretRef::password("admin", "127.0.0.1", port), "hunter2")
            .unwrap();
        let mut draft = HostDraft::telnet("switch", "127.0.0.1", port, None);
        draft.auth = AuthKind::Password;
        draft.user = "admin".into();
        let (commands, received, runner) = launch(draft, keychain);

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut output = Vec::new();
        let mut exited = false;
        let mut typed = false;
        while !exited {
            assert!(
                Instant::now() < deadline,
                "got {:?}",
                String::from_utf8_lossy(&output)
            );
            match received.try_recv() {
                Ok(TerminalTransportEvent::Output(bytes)) => output.extend(bytes),
                Ok(TerminalTransportEvent::Exited { code, .. }) => {
                    assert_eq!(code, 0);
                    exited = true;
                }
                Ok(TerminalTransportEvent::Failed(why)) => panic!("{why}"),
                Ok(_) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
            if !typed && output.ends_with(b"$ ") {
                typed = true;
                commands
                    .send(TerminalTransportCommand::Write(b"ls\r".to_vec()))
                    .unwrap();
                commands
                    .send(TerminalTransportCommand::Resize(TerminalSize::new(
                        100, 30, 8, 16,
                    )))
                    .unwrap();
                commands.send(TerminalTransportCommand::SendBreak).unwrap();
            }
        }
        device.join().unwrap();
        assert!(String::from_utf8_lossy(&output).contains("Welcome"));
        drop(commands);
        runner.join().unwrap().unwrap();
    }

    #[test]
    fn without_the_servers_echo_the_line_is_edited_here() {
        let Some(listener) = listen() else { return };
        let port = listener.local_addr().unwrap().port();
        let device = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut seen = Vec::new();
            // A chess server: no negotiation, and no echo.
            socket.write_all(b"login: ").unwrap();
            // Typed with a mistake, taken back: only the line comes.
            read_until(&mut socket, &mut seen, b"guest\r\n");
            // Then it echoes, as for a password.
            socket.write_all(&[IAC, WILL, ECHO]).unwrap();
            read_until(&mut socket, &mut seen, &[IAC, DO, ECHO]);
            read_until(&mut socket, &mut seen, b"x");
        });

        let draft = HostDraft::telnet("chess", "127.0.0.1", port, None);
        let (commands, received, runner) = launch(draft, Arc::default());
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut output = Vec::new();
        let mut typed = false;
        while !(typed && output.ends_with(b"\r\n")) {
            assert!(
                Instant::now() < deadline,
                "got {:?}",
                String::from_utf8_lossy(&output)
            );
            match received.try_recv() {
                Ok(TerminalTransportEvent::Output(bytes)) => output.extend(bytes),
                Ok(TerminalTransportEvent::Failed(why)) => panic!("{why}"),
                Ok(_) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
            if !typed && output.ends_with(b"login: ") {
                typed = true;
                for keys in [&b"gues"[..], b"z", &[0x7f], b"t\r"] {
                    commands
                        .send(TerminalTransportCommand::Write(keys.to_vec()))
                        .unwrap();
                }
            }
        }
        assert_eq!(output, b"login: guesz\x08 \x08t\r\n");
        // The server echoes now: typing goes as it is, and is not shown here.
        std::thread::sleep(Duration::from_millis(200));
        commands
            .send(TerminalTransportCommand::Write(b"x".to_vec()))
            .unwrap();
        device.join().unwrap();
        drop(commands);
        runner.join().unwrap().unwrap();
        assert!(
            std::iter::from_fn(|| received.try_recv().ok()).all(|event| !matches!(
                event,
                TerminalTransportEvent::Output(bytes) if bytes.contains(&b'x')
            )),
            "no local echo once the server echoes"
        );
    }

    #[test]
    fn a_transfers_bytes_go_past_the_line_edited_here() {
        let Some(listener) = listen() else { return };
        let port = listener.local_addr().unwrap().port();
        // What ZMODEM sends, with what the line editing would take: a
        // backspace, DEL, ^U, and IAC, which Telnet doubles.
        let data = [0x18, b'B', 0x08, 0x7f, 0x15, 0xff, b'*'];
        let device = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut seen = Vec::new();
            // No negotiation and no echo: the line is edited here.
            socket.write_all(b"$ ").unwrap();
            let mut buffer = [0; 1024];
            while !seen.contains(&b'*') {
                let count = socket.read(&mut buffer).unwrap();
                assert!(count > 0, "closed; got {seen:?}");
                seen.extend_from_slice(&buffer[..count]);
            }
            seen
        });

        let draft = HostDraft::telnet("device", "127.0.0.1", port, None);
        let (commands, received, runner) = launch(draft, Arc::default());
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut output = Vec::new();
        while !output.ends_with(b"$ ") {
            assert!(Instant::now() < deadline);
            match received.try_recv() {
                Ok(TerminalTransportEvent::Output(bytes)) => output.extend(bytes),
                Ok(_) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        commands
            .send(TerminalTransportCommand::WriteData(data.to_vec()))
            .unwrap();
        let seen = device.join().unwrap();
        assert!(
            seen.ends_with(&[0x18, b'B', 0x08, 0x7f, 0x15, 0xff, 0xff, b'*']),
            "{seen:?}"
        );
        drop(commands);
        runner.join().unwrap().unwrap();
        // Nothing of it shown here.
        assert!(
            std::iter::from_fn(|| received.try_recv().ok())
                .all(|event| !matches!(event, TerminalTransportEvent::Output(_))),
        );
    }
}
