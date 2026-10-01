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

use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// Bumped when a request or reply changes shape. The command and the app
/// come from the same build, so they differ only while an older copy still
/// runs, or on Windows while the copy on the PATH has not been updated.
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
    /// Bring the app's window forward. Not the `shellrs` command's: ShellRS
    /// sends it when it is opened while it is already running, so it is
    /// answered whatever 启用外部 CLI says.
    Activate,
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
    /// The app heard [`Request::Activate`].
    Activated,
    Error {
        code: ErrorCode,
        message: String,
    },
}

/// A saved host as the CLI lists it.
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
}

impl HostInfo {
    pub fn address(&self) -> String {
        format!("{}@{}:{}", self.user, self.host, self.port)
    }
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
    pub skipped: u64,
    pub failed: u64,
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
        for code in [ErrorCode::NotEnabled, ErrorCode::HostKeyUnknown] {
            assert_eq!(
                serde_json::to_string(&code).unwrap(),
                format!("\"{}\"", code.as_str())
            );
        }
    }
}
