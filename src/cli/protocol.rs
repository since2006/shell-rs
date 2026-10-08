//! What travels over the CLI socket. Every message is a frame: one byte
//! saying what it holds, four bytes of big-endian length, then the payload.
//! Control messages are JSON; a command's output is passed on as raw bytes,
//! so binary output needs no encoding.
//!
//! One connection carries one request: the client sends it as the first
//! frame, the app answers with any number of frames and closes.

use std::{
    io::{self, Read, Write},
    path::PathBuf,
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};
use zeroize::Zeroizing;

use super::link::OpenLink;

/// Bumped when a request or reply changes shape. The command and the app
/// come from the same build, so they differ only while an older copy still
/// runs, or on Windows while the copy on the PATH has not been updated.
///
/// New requests and replies are added without bumping it, so a command
/// and an app of different builds still agree on what both know; an app
/// asked for a request it does not know answers `bad_request`, which the
/// command explains.
pub const PROTOCOL_VERSION: u32 = 2;

/// Frames larger than this are refused, so a confused peer cannot make the
/// other side allocate without bound.
const MAX_FRAME: u32 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind {
    Json,
    Stdout,
    Stderr,
}

impl FrameKind {
    fn tag(self) -> u8 {
        match self {
            FrameKind::Json => b'J',
            FrameKind::Stdout => b'1',
            FrameKind::Stderr => b'2',
        }
    }

    fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            b'J' => Some(FrameKind::Json),
            b'1' => Some(FrameKind::Stdout),
            b'2' => Some(FrameKind::Stderr),
            _ => None,
        }
    }
}

pub fn write_frame(writer: &mut impl Write, kind: FrameKind, payload: &[u8]) -> io::Result<()> {
    let length = u32::try_from(payload.len())
        .ok()
        .filter(|length| *length <= MAX_FRAME)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "frame too large"))?;
    let mut header = [0; 5];
    header[0] = kind.tag();
    header[1..].copy_from_slice(&length.to_be_bytes());
    writer.write_all(&header)?;
    writer.write_all(payload)?;
    writer.flush()
}

/// The next frame, or `None` when the peer closed the connection cleanly
/// between frames.
pub fn read_frame(reader: &mut impl Read) -> io::Result<Option<(FrameKind, Vec<u8>)>> {
    let mut tag = [0; 1];
    match reader.read_exact(&mut tag) {
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        result => result?,
    }
    let kind = FrameKind::from_tag(tag[0])
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unknown frame"))?;
    let mut length = [0; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length);
    if length > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut payload = vec![0; length as usize];
    reader.read_exact(&mut payload)?;
    Ok(Some((kind, payload)))
}

pub fn write_json(writer: &mut impl Write, message: &impl Serialize) -> io::Result<()> {
    let payload = serde_json::to_vec(message).map_err(io::Error::other)?;
    write_frame(writer, FrameKind::Json, &payload)
}

pub fn parse_json<T: DeserializeOwned>(payload: &[u8]) -> io::Result<T> {
    serde_json::from_slice(payload)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// The first frame of every connection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub version: u32,
    pub request: Request,
}

/// The version alone, read before the request: a request this build does
/// not know is then a version mismatch rather than a puzzle.
#[derive(Deserialize)]
pub struct VersionOnly {
    pub version: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    List {
        query: Option<String>,
    },
    Exec {
        host: String,
        command: String,
    },
    /// `exec --terminal`: type `command` into the host's open terminal
    /// instead of logging in again, for a bastion host that allows one
    /// login. Its output comes as stdout, then [`Reply::Context`], then
    /// [`Reply::Exit`].
    ExecInTerminal {
        host: String,
        command: String,
    },
    /// `source` is absolute: the app's working directory is not the
    /// caller's.
    Upload {
        host: String,
        source: PathBuf,
        destination: String,
    },
    Download {
        host: String,
        source: String,
        destination: PathBuf,
    },
    /// Copy what the local folder `source` holds into `destination` on the
    /// host, skipping what has not changed; with `delete`, remove what is
    /// there and not here first.
    Sync {
        host: String,
        source: PathBuf,
        destination: String,
        #[serde(default)]
        delete: bool,
    },
    ShowHost {
        host: String,
    },
    CreateHost {
        fields: HostFields,
    },
    UpdateHost {
        host: String,
        fields: HostFields,
    },
    /// `force` closes the host's tabs; without it, a host with tabs open
    /// is refused.
    DeleteHost {
        host: String,
        #[serde(default)]
        force: bool,
    },
    ListCredentials {
        query: Option<String>,
    },
    ShowCredential {
        credential: String,
    },
    CreateCredential {
        fields: CredentialFields,
    },
    UpdateCredential {
        credential: String,
        fields: CredentialFields,
    },
    DeleteCredential {
        credential: String,
    },
    /// Bring the app's window forward, and open `open` in it. Not the
    /// `shellrs` command's: ShellRS sends it when it is opened while it is
    /// already running, so it is answered whatever 启用外部 CLI says.
    ///
    /// Without a link it is `{"type":"activate"}`, as it always was. A
    /// ShellRS from before links ignores `open` and only comes forward.
    Activate {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        open: Option<OpenLink>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    Hosts {
        hosts: Vec<HostInfo>,
    },
    /// How far a transfer has come.
    Progress(TransferCounters),
    TransferDone(TransferSummary),
    /// The remote command ended with this exit code.
    Exit {
        code: i32,
    },
    /// Where an `exec --terminal` command runs, once it started.
    Context(TerminalContext),
    /// A host as it is now: shown, created or changed.
    Host(HostDetails),
    HostDeleted(HostDeleted),
    Credentials {
        credentials: Vec<CredentialDetails>,
    },
    Credential(CredentialDetails),
    CredentialDeleted(CredentialDeleted),
    /// The app heard [`Request::Activate`].
    Activated,
    Error {
        code: ErrorCode,
        message: String,
    },
}

/// A host as the CLI lists it: a saved one, or a temporary one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInfo {
    /// The host's public ID, the one 复制 ID copies.
    pub id: String,
    pub name: String,
    /// The full group path, such as `生产/数据库`; `None` at the root.
    pub group: Option<String>,
    pub user: String,
    pub host: String,
    pub port: u16,
    /// The detected operating system, as stored (`ubuntu`, `macos`, …).
    pub os: Option<String>,
    /// Connected to without being saved (a 临时连接, or an 外部连接 a bastion
    /// host's link opened): listed while its tab is open, under one ID for
    /// as long; opened again, it is another host with another ID. Missing
    /// from an older app's answer, which listed saved hosts only.
    #[serde(default)]
    pub temporary: bool,
}

impl HostInfo {
    pub fn address(&self) -> String {
        format!("{}@{}:{}", self.user, self.host, self.port)
    }
}

/// Where an `exec --terminal` command ran: the terminal may have gone to
/// another user, folder or machine since it was opened.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalContext {
    pub user: String,
    pub host: String,
    pub cwd: String,
    /// The interactive shell the command was typed into.
    pub shell: String,
    /// What ran the command: `bash`, or `sh` where there is none.
    pub interp: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferCounters {
    pub files: u64,
    pub total_files: u64,
    pub bytes: u64,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferSummary {
    pub files: u64,
    pub bytes: u64,
    /// For a sync, the files left alone because they had not changed.
    pub skipped: u64,
    pub failed: u64,
    /// What a sync with `--delete` removed from the host. Missing from an
    /// older app's answer.
    #[serde(default)]
    pub deleted: u64,
    /// Why each failed item failed.
    pub failures: Vec<String>,
}

/// Stable names for what went wrong, for an agent to act on whatever
/// language the message is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The app is not running. Only the command itself reports this.
    NotRunning,
    /// 启用外部 CLI is off.
    NotEnabled,
    VersionMismatch,
    HostNotFound,
    HostKeyUnknown,
    HostKeyChanged,
    MissingCredential,
    ConnectFailed,
    TransferFailed,
    BadRequest,
    CredentialNotFound,
    /// A host with tabs open, asked to be deleted without `--force`.
    HostInUse,
    /// A change was made, but could not all be written to disk or the
    /// keychain.
    SaveFailed,
    /// `exec --terminal`: the host has no connected terminal.
    NoTerminal,
    /// `exec --terminal`: the terminal could not take the command.
    TerminalBusy,
    /// `exec --terminal`: the shell is not bash, dash or busybox ash.
    UnsupportedShell,
    /// `exec --terminal`: the command line is longer than the shell takes.
    TooLong,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::NotRunning => "not_running",
            ErrorCode::NotEnabled => "not_enabled",
            ErrorCode::VersionMismatch => "version_mismatch",
            ErrorCode::HostNotFound => "host_not_found",
            ErrorCode::HostKeyUnknown => "host_key_unknown",
            ErrorCode::HostKeyChanged => "host_key_changed",
            ErrorCode::MissingCredential => "missing_credential",
            ErrorCode::ConnectFailed => "connect_failed",
            ErrorCode::TransferFailed => "transfer_failed",
            ErrorCode::BadRequest => "bad_request",
            ErrorCode::CredentialNotFound => "credential_not_found",
            ErrorCode::HostInUse => "host_in_use",
            ErrorCode::SaveFailed => "save_failed",
            ErrorCode::NoTerminal => "no_terminal",
            ErrorCode::TerminalBusy => "terminal_busy",
            ErrorCode::UnsupportedShell => "unsupported_shell",
            ErrorCode::TooLong => "too_long",
        }
    }
}

/// A failure with its code, as the backend reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliError {
    pub code: ErrorCode,
    pub message: String,
}

impl CliError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// A password, passphrase or private key on its way to the app, never
/// the other way. `Debug` leaves it out, so a request can be printed or
/// compared in a test without it showing.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Secret(***)")
    }
}

impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::new)
    }
}

/// For a field that can be left out, set or set to `null`, three different
/// things: `None` when left out, `Some(None)` for `null`.
fn present<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// How a host logs in, as the CLI spells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthChoice {
    Password,
    Credential,
    NoPassword,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyChoice {
    #[default]
    Http,
    Socks5,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKindChoice {
    Password,
    Key,
    Agent,
}

/// A saved host in full, as `hosts show` prints it. `hosts create` and
/// `hosts update` read the same names back, so what one prints the other
/// takes. Never a secret: only whether one is saved, and only when shown.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostDetails {
    pub id: String,
    pub name: String,
    /// The full group path, such as `生产/数据库`; `None` at the root.
    pub group: Option<String>,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: AuthChoice,
    /// The credential's ID, for a host that logs in with one.
    pub credential: Option<String>,
    /// For a host with its own password, whether one is saved. Looked up
    /// only for `hosts show`, and for the host a change returns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_saved: Option<bool>,
    pub route: RouteDetails,
    pub notes: String,
    pub os: Option<String>,
    #[serde(default)]
    pub temporary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RouteDetails {
    Direct,
    /// The jump hosts' IDs in order; `None` for one that was deleted.
    Jump {
        hosts: Vec<Option<String>>,
    },
    Proxy {
        kind: ProxyChoice,
        host: String,
        port: u16,
        user: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password_saved: Option<bool>,
    },
}

/// What `hosts create` sets and `hosts update` changes: every field may be
/// left out (`update` leaves it as it is), and those that can be empty may
/// be `null`. What `hosts show` prints and nobody sets is taken out before
/// these are read.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostFields {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, alias = "address", skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// `null` for the root.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub group: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthChoice>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub credential: Option<Option<String>>,
    /// `null` deletes the saved one.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub password: Option<Option<Secret>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<RouteFields>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// A route as `hosts create` and `hosts update` take it: always whole.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RouteFields {
    Direct,
    Jump {
        hosts: Vec<Option<String>>,
    },
    Proxy {
        #[serde(default)]
        kind: ProxyChoice,
        host: String,
        port: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user: Option<String>,
        /// `null` deletes the saved one.
        #[serde(
            default,
            deserialize_with = "present",
            skip_serializing_if = "Option::is_none"
        )]
        password: Option<Option<Secret>>,
    },
}

/// Deleted, with what went with it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostDeleted {
    pub host: HostDetails,
    /// Its port forwarding rules, deleted with it.
    pub forwards: u64,
    /// Other hosts that went through it, which now have a deleted jump
    /// host in its place.
    pub jump_users: u64,
}

/// A saved credential, as `credentials show` prints it. Never a secret,
/// nor the path of a key ShellRS keeps: only whether one is saved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialDetails {
    pub id: String,
    pub name: String,
    pub kind: CredentialKindChoice,
    pub user: String,
    /// The user's own key file; `None` for a key ShellRS keeps (`kept`).
    pub key_path: Option<String>,
    /// The key was pasted or generated, and ShellRS keeps it itself.
    #[serde(default)]
    pub kept: bool,
    /// The IDs of the saved hosts that log in with it.
    #[serde(default)]
    pub hosts: Vec<String>,
    /// Looked up only for `credentials show`, and for the credential a
    /// change returns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_saved: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passphrase_saved: Option<bool>,
}

/// What `credentials create` sets and `credentials update` changes, as
/// for [`HostFields`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialFields {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<CredentialKindChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// A password credential's; `null` deletes the saved one.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub password: Option<Option<Secret>>,
    /// A key file on this machine, absolute by the time it reaches the app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_path: Option<PathBuf>,
    /// The key's text, for ShellRS to keep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_key: Option<Secret>,
    /// The key's; `null` deletes the saved one.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub passphrase: Option<Option<Secret>>,
}

/// Deleted; the hosts that logged in with it now log in on their own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialDeleted {
    pub credential: CredentialDetails,
    pub released: Vec<HostInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_including_binary_output() {
        let mut wire = Vec::new();
        write_json(
            &mut wire,
            &Envelope {
                version: PROTOCOL_VERSION,
                request: Request::Exec {
                    host: "VmLkf1snMOuPKJ07".into(),
                    command: "uname -a".into(),
                },
            },
        )
        .unwrap();
        write_frame(&mut wire, FrameKind::Stderr, &[0, 159, 146, 150, b'\n']).unwrap();

        let mut reader = wire.as_slice();
        let (kind, payload) = read_frame(&mut reader).unwrap().unwrap();
        assert_eq!(kind, FrameKind::Json);
        let envelope: Envelope = parse_json(&payload).unwrap();
        assert_eq!(
            envelope.request,
            Request::Exec {
                host: "VmLkf1snMOuPKJ07".into(),
                command: "uname -a".into(),
            }
        );
        assert_eq!(
            read_frame(&mut reader).unwrap(),
            Some((FrameKind::Stderr, vec![0, 159, 146, 150, b'\n']))
        );
        // A clean close between frames.
        assert_eq!(read_frame(&mut reader).unwrap(), None);
    }

    #[test]
    fn a_truncated_or_foreign_frame_is_an_error() {
        let mut wire = Vec::new();
        write_frame(&mut wire, FrameKind::Stdout, b"hello").unwrap();
        wire.truncate(wire.len() - 1);
        assert!(read_frame(&mut wire.as_slice()).is_err());
        assert!(read_frame(&mut &b"X\0\0\0\0"[..]).is_err());
        assert!(read_frame(&mut &b"J\xff\xff\xff\xff"[..]).is_err());
    }

    #[test]
    fn error_codes_are_spelled_as_documented() {
        for code in [
            ErrorCode::NotEnabled,
            ErrorCode::HostKeyUnknown,
            ErrorCode::NoTerminal,
            ErrorCode::TerminalBusy,
            ErrorCode::UnsupportedShell,
            ErrorCode::TooLong,
        ] {
            assert_eq!(
                serde_json::to_string(&code).unwrap(),
                format!("\"{}\"", code.as_str())
            );
        }
    }

    #[test]
    fn activating_without_a_link_is_what_it_always_was() {
        let plain = Envelope {
            version: PROTOCOL_VERSION,
            request: Request::Activate { open: None },
        };
        assert_eq!(
            serde_json::to_string(&plain).unwrap(),
            r#"{"version":2,"request":{"type":"activate"}}"#
        );
        let read: Envelope = parse_json(br#"{"version":2,"request":{"type":"activate"}}"#).unwrap();
        assert_eq!(read, plain);

        let with_link = Request::Activate {
            open: Some(OpenLink {
                url: "ssh://token@10.0.0.9:2222".into(),
                tab: Some("堡垒机".into()),
            }),
        };
        let wire = serde_json::to_vec(&with_link).unwrap();
        assert_eq!(parse_json::<Request>(&wire).unwrap(), with_link);

        // How a ShellRS from before links read it: still a plain Activate.
        #[derive(Debug, PartialEq, Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum Before {
            Activate,
        }
        assert_eq!(parse_json::<Before>(&wire).unwrap(), Before::Activate);
    }
}
