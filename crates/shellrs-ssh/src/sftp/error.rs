//! Why browsing or a transfer failed, and what a transfer asks or notes:
//! values the app turns into its own words. The `Display` texts here are
//! English for logs, never shown as they are.
//!
//! Like the SSH layer's errors they are the leaves and the layers of an
//! `anyhow` chain, so a dropped connection under any of them still reads as
//! one and is retried.

use std::error::Error;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use crate::wrap_shared;

/// What the engine says, kept whole for the app to word: a typed leaf,
/// maybe over the error that caused it. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Reason(Arc<anyhow::Error>);

impl Reason {
    /// `error` alone, over nothing.
    pub fn new<E: Error + Send + Sync + 'static>(error: E) -> Reason {
        Reason(Arc::new(anyhow::Error::new(error)))
    }

    pub fn error(&self) -> &anyhow::Error {
        &self.0
    }

    /// `error` over this reason, which stays shared.
    pub fn under<E: Error + Send + Sync + 'static>(&self, error: E) -> Reason {
        Reason(Arc::new(wrap_shared(error, self.0.clone())))
    }
}

impl From<anyhow::Error> for Reason {
    fn from(error: anyhow::Error) -> Self {
        Self(Arc::new(error))
    }
}

impl From<Arc<anyhow::Error>> for Reason {
    fn from(error: Arc<anyhow::Error>) -> Self {
        Self(error)
    }
}

/// Equal when they say the same.
impl PartialEq for Reason {
    fn eq(&self, other: &Self) -> bool {
        format!("{:#}", self.0) == format!("{:#}", other.0)
    }
}

impl Eq for Reason {}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&*self.0, f)
    }
}

/// A browsing, connection or batch failure, and what a batch notes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SftpError {
    StartTimeout,
    NoV3,
    NoPath,
    NoLinkTarget,
    /// A remote name decoded lossily: using it could mean another file.
    AmbiguousName,
    RemotePathInvalid,
    FileNameInvalid {
        name: String,
    },
    NothingToUpload,
    NothingToDownload,
    DownloadTargetRelative,
    FileNameNotUtf8,
    Exists {
        name: String,
    },
    /// A resumed transfer met a server whose key changed.
    HostKeyChanged,
    /// A request came while there was no connection.
    NotConnected,
    /// A batch went on while there was no connection.
    NoConnection,
    /// Over the error reading the folder failed with.
    ListFailed,
    /// A batch came while another ran.
    Busy,
    LocalFolderMissing {
        path: PathBuf,
    },
    RemoteFolderMissing {
        path: String,
    },
    NotAFolder {
        path: String,
    },
    SyncToRoot,
    /// A dropped batch waits `seconds` before try `attempt` of 3.
    ReconnectIn {
        seconds: u64,
        attempt: usize,
    },
    /// Over the error a sync's delete failed with.
    DeleteFailed,
}

impl fmt::Display for SftpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StartTimeout => f.write_str("SFTP didn't start in time"),
            Self::NoV3 => f.write_str("the server doesn't offer SFTP version 3"),
            Self::NoPath => f.write_str("the server returned no path"),
            Self::NoLinkTarget => f.write_str("couldn't read the link's target"),
            Self::AmbiguousName => f.write_str("a remote name can't be represented reliably"),
            Self::RemotePathInvalid => f.write_str("a remote path can't be empty or hold NUL"),
            Self::FileNameInvalid { name } => write!(f, "invalid file name: {name}"),
            Self::NothingToUpload => f.write_str("nothing to upload"),
            Self::NothingToDownload => f.write_str("nothing to download"),
            Self::DownloadTargetRelative => f.write_str("a download target must be absolute"),
            Self::FileNameNotUtf8 => f.write_str("a file name isn't UTF-8"),
            Self::Exists { name } => write!(f, "{name} already exists"),
            Self::HostKeyChanged => f.write_str("the server's host key changed"),
            Self::NotConnected => f.write_str("SFTP isn't connected"),
            Self::NoConnection => f.write_str("no SFTP connection"),
            Self::ListFailed => f.write_str("couldn't read the folder"),
            Self::Busy => f.write_str("a transfer batch is already running"),
            Self::LocalFolderMissing { path } => {
                write!(f, "local folder {} doesn't exist", path.display())
            }
            Self::RemoteFolderMissing { path } => write!(f, "remote folder {path} doesn't exist"),
            Self::NotAFolder { path } => write!(f, "remote path {path} isn't a folder"),
            Self::SyncToRoot => f.write_str("can't sync to the root folder"),
            Self::ReconnectIn { seconds, attempt } => {
                write!(f, "reconnecting in {seconds} s ({attempt}/3)")
            }
            Self::DeleteFailed => f.write_str("couldn't delete"),
        }
    }
}

impl Error for SftpError {}

/// An upload's failures, and what it asks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadError {
    SourceNameNotUtf8,
    /// Another transfer writes the same target.
    TargetBusy,
    ParentMissing,
    /// Over the error the item failed with.
    Failed,
    FolderInTheWay,
    /// Asked before replacing what is there; `modified` in Unix seconds.
    Exists {
        size: u64,
        modified: Option<u64>,
    },
    /// Asked again: the target changed while the item went up.
    ExistsChanged {
        size: u64,
        modified: Option<u64>,
    },
    TargetGone,
    SourceNowFolder,
    TargetNotFolder,
    SpecialFile,
    ResumeFound,
    LinkTargetNotUtf8,
    RecordWithoutLink,
    TemporaryLinkChanged,
    RecordWithoutSource,
    TemporaryKindChanged,
    PartialTooLarge,
    PipelineEnded,
    PublishUnconfirmed,
    TemporaryChanged,
    TargetCreated,
    BackupChanged,
    BackupChangedAfterPublish,
    BackupAndTarget,
}

impl fmt::Display for UploadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceNameNotUtf8 => f.write_str("the source's name isn't UTF-8"),
            Self::TargetBusy => f.write_str("another transfer is uploading to the same target"),
            Self::ParentMissing => f.write_str("the parent folder wasn't created"),
            Self::Failed => f.write_str("couldn't upload"),
            Self::FolderInTheWay => f.write_str("a remote folder is in the way"),
            Self::Exists { size, modified } => {
                write!(f, "the target exists ({size} bytes, modified {modified:?})")
            }
            Self::ExistsChanged { size, modified } => write!(
                f,
                "the target changed and exists ({size} bytes, modified {modified:?})"
            ),
            Self::TargetGone => f.write_str("the target went away during the upload"),
            Self::SourceNowFolder => f.write_str("the source is now a folder"),
            Self::TargetNotFolder => f.write_str("the target exists and isn't a folder"),
            Self::SpecialFile => f.write_str("special files can't be uploaded"),
            Self::ResumeFound => f.write_str("an unfinished .filepart was found"),
            Self::LinkTargetNotUtf8 => f.write_str("the link's target isn't UTF-8"),
            Self::RecordWithoutLink => f.write_str("the resume record has no link target"),
            Self::TemporaryLinkChanged => f.write_str("the temporary link changed"),
            Self::RecordWithoutSource => f.write_str("the resume record has no source"),
            Self::TemporaryKindChanged => f.write_str("the remote temporary file changed kind"),
            Self::PartialTooLarge => f.write_str("the remote part is larger than the source"),
            Self::PipelineEnded => f.write_str("the upload pipeline ended unexpectedly"),
            Self::PublishUnconfirmed => f.write_str("couldn't confirm the file was published"),
            Self::TemporaryChanged => f.write_str("the temporary file changed"),
            Self::TargetCreated => f.write_str("the target was created while replacing it"),
            Self::BackupChanged => f.write_str("the backup changed"),
            Self::BackupChangedAfterPublish => f.write_str("the backup changed after publishing"),
            Self::BackupAndTarget => f.write_str("both a backup and the target exist"),
        }
    }
}

impl Error for UploadError {}

/// A download's failures, and what it asks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadError {
    NameInvalid {
        name: String,
    },
    SourceMissing,
    /// Another transfer writes the same target.
    TargetBusy,
    ParentMissing,
    /// Over the error the item failed with.
    Failed,
    TargetInvalid,
    FolderInTheWay,
    /// Asked before replacing what is there; `modified` in Unix seconds.
    Exists {
        size: u64,
        modified: Option<u64>,
    },
    /// Asked again: the target changed while the item came down.
    ExistsChanged {
        size: u64,
        modified: Option<u64>,
    },
    FileInTheWay,
    SpecialFile,
    PartialNotFile {
        path: PathBuf,
    },
    PartialTooLarge,
    SourceChanged,
    ResumeFound,
    PipelineEnded,
    SourceShrank,
    LinksUnsupported,
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NameInvalid { name } => write!(f, "{name} isn't a valid name here"),
            Self::SourceMissing => f.write_str("the remote item doesn't exist"),
            Self::TargetBusy => f.write_str("another transfer is downloading to the same place"),
            Self::ParentMissing => f.write_str("the parent folder wasn't created"),
            Self::Failed => f.write_str("couldn't download"),
            Self::TargetInvalid => f.write_str("invalid download target"),
            Self::FolderInTheWay => f.write_str("a local folder is in the way"),
            Self::Exists { size, modified } => {
                write!(f, "the target exists ({size} bytes, modified {modified:?})")
            }
            Self::ExistsChanged { size, modified } => write!(
                f,
                "the target changed and exists ({size} bytes, modified {modified:?})"
            ),
            Self::FileInTheWay => f.write_str("a local file is in the way"),
            Self::SpecialFile => f.write_str("special files can't be downloaded"),
            Self::PartialNotFile { path } => {
                write!(f, "the temporary file {} isn't a file", path.display())
            }
            Self::PartialTooLarge => f.write_str("the local part is larger than the source"),
            Self::SourceChanged => f.write_str("the remote file changed since"),
            Self::ResumeFound => f.write_str("an unfinished .filepart was found"),
            Self::PipelineEnded => f.write_str("the download pipeline ended unexpectedly"),
            Self::SourceShrank => f.write_str("the remote file shrank during the download"),
            Self::LinksUnsupported => f.write_str("symbolic links aren't supported here"),
        }
    }
}

impl Error for DownloadError {}

/// What is wrong with a resume record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JournalError {
    SourceNotFile,
    Mismatch,
    TemporaryInvalid,
    ReplacesFolder,
    /// Over the parse error of an upload's record.
    UploadDamaged,
    /// Over the parse error of a download's record.
    DownloadDamaged,
    /// Over the error reading a record failed with.
    Unreadable,
    /// Over the error saving a record failed with.
    Unsaved,
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceNotFile => f.write_str("the local source is no longer a file"),
            Self::Mismatch => f.write_str("the resume record doesn't match"),
            Self::TemporaryInvalid => f.write_str("invalid temporary path in the resume record"),
            Self::ReplacesFolder => f.write_str("a resume record can't replace a folder"),
            Self::UploadDamaged | Self::DownloadDamaged => {
                f.write_str("the resume record is damaged")
            }
            Self::Unreadable => f.write_str("couldn't read the resume record"),
            Self::Unsaved => f.write_str("couldn't save the resume record"),
        }
    }
}

impl Error for JournalError {}

/// Why the editor's read or save of a remote file failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditError {
    Missing,
    Shrank,
    GoneAfterSave,
    /// Over the error a write failed with after the file was truncated.
    Interrupted,
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("the file doesn't exist"),
            Self::Shrank => f.write_str("the file shrank while it was read"),
            Self::GoneAfterSave => f.write_str("the file is gone after saving"),
            Self::Interrupted => f.write_str("the save was interrupted"),
        }
    }
}

impl Error for EditError {}
