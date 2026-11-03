//! Whole text files for the editor: read into memory, written back in place.
//!
//! Unlike transfers there is no `.filepart` and no rename: a save truncates
//! the file and writes it again, as vim, nano and VS Code Remote do, so the
//! owner, the permissions, links pointing at it and hard links all stay.
//! What keeps a save from clobbering someone else's change is the stamp: the
//! size and modification time the file had when it was read or last saved.

use super::{EditError, EntryKind, FileMetadata, Reason, RemotePath, client::RemoteFs};
use crate::{Wrapped, wrap};
use anyhow::{Result, anyhow, bail};
use futures::{StreamExt as _, stream::FuturesUnordered};
use std::{
    error::Error,
    fmt,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// The largest file the editor opens. The code editor is meant for up to
/// about fifty thousand lines.
pub const EDIT_LIMIT: u64 = 5 * 1024 * 1024;

const CHUNK: u32 = 32 * 1024;
const MAX_IN_FLIGHT: usize = 16;
/// A NUL byte this early means a binary file, as `grep` and git decide.
const BINARY_PROBE: usize = 8 * 1024;
const BOM: &[u8] = b"\xEF\xBB\xBF";

/// What a file looked like when it was read or saved: a save checks it is
/// still so, otherwise someone else changed the file in the meantime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStamp {
    size: u64,
    modified: Option<SystemTime>,
}

impl FileStamp {
    pub fn new(size: u64, modified: Option<SystemTime>) -> Self {
        Self { size, modified }
    }
    /// A remote file's stamp; SFTP gives whole seconds.
    pub fn of(metadata: &FileMetadata) -> Self {
        Self::new(
            metadata.size(),
            metadata
                .modified()
                .map(|seconds| UNIX_EPOCH + Duration::from_secs(u64::from(seconds))),
        )
    }
    /// A local file's stamp, to the precision the file system keeps.
    pub fn local(metadata: &std::fs::Metadata) -> Self {
        Self::new(metadata.len(), metadata.modified().ok())
    }
    pub fn size(&self) -> u64 {
        self.size
    }
}

/// How a text file is laid out on disk, kept so that a save writes it back
/// the same way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextFormat {
    bom: bool,
    crlf: bool,
}

impl TextFormat {
    pub fn new(bom: bool, crlf: bool) -> Self {
        Self { bom, crlf }
    }
    /// The file starts with a UTF-8 byte order mark.
    pub fn bom(&self) -> bool {
        self.bom
    }
    /// Every line ends in CRLF; the editor sees LF.
    pub fn crlf(&self) -> bool {
        self.crlf
    }
    /// The bytes to write for `text` as the editor holds it.
    pub fn encode(&self, text: &str) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(text.len() + 3);
        if self.bom {
            bytes.extend_from_slice(BOM);
        }
        if self.crlf {
            // A CRLF pasted in stays one line break.
            bytes.extend_from_slice(text.replace("\r\n", "\n").replace('\n', "\r\n").as_bytes());
        } else {
            bytes.extend_from_slice(text.as_bytes());
        }
        bytes
    }
}

/// A file read for editing.
#[derive(Clone, PartialEq, Eq)]
pub struct TextFile {
    text: String,
    format: TextFormat,
    stamp: FileStamp,
}

impl TextFile {
    /// UTF-8 text, with or without a byte order mark. A file whose lines all
    /// end in CRLF is shown with LF and written back with CRLF; one with
    /// mixed line ends is kept exactly as it is. Only refuses, so any kind of
    /// failure will do.
    pub fn decode<F>(mut bytes: Vec<u8>, stamp: FileStamp) -> Result<Self, ReadFailure<F>> {
        if bytes[..bytes.len().min(BINARY_PROBE)].contains(&0) {
            return Err(ReadFailure::NotText);
        }
        let bom = bytes.starts_with(BOM);
        if bom {
            bytes.drain(..BOM.len());
        }
        let text = String::from_utf8(bytes).map_err(|_| ReadFailure::NotText)?;
        let crlf_count = text.matches("\r\n").count();
        let crlf = crlf_count > 0 && crlf_count == text.matches('\n').count();
        let text = if crlf {
            text.replace("\r\n", "\n")
        } else {
            text
        };
        Ok(Self {
            text,
            format: TextFormat { bom, crlf },
            stamp,
        })
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn format(&self) -> TextFormat {
        self.format
    }
    pub fn stamp(&self) -> FileStamp {
        self.stamp
    }
    pub fn into_parts(self) -> (String, TextFormat, FileStamp) {
        (self.text, self.format, self.stamp)
    }
}

impl std::fmt::Debug for TextFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextFile")
            .field("bytes", &self.text.len())
            .field("format", &self.format)
            .field("stamp", &self.stamp)
            .finish()
    }
}

/// Why a file was not opened for editing. `F` is how a failure other than
/// a refusal is told: the engine's [`Reason`], or the app's words for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadFailure<F> {
    /// A directory, a device or the like.
    NotFile,
    /// Larger than `EDIT_LIMIT`; the size in bytes.
    TooLarge(u64),
    /// Binary, or text in an encoding other than UTF-8.
    NotText,
    Failed(F),
}

impl<F: fmt::Display> fmt::Display for ReadFailure<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadFailure::NotFile => f.write_str("not a regular file"),
            ReadFailure::TooLarge(size) => write!(f, "file too large ({size} bytes)"),
            ReadFailure::NotText => f.write_str("not a UTF-8 text file"),
            ReadFailure::Failed(failure) => failure.fmt(f),
        }
    }
}

impl<F: fmt::Debug + fmt::Display> Error for ReadFailure<F> {}

impl<F> ReadFailure<F> {
    /// The refusal inside `error`, told either way, or the error itself as
    /// a failure, told by `failed`.
    pub fn from_error(error: anyhow::Error, failed: impl FnOnce(Reason) -> F) -> Self
    where
        Self: Error + Send + Sync + 'static,
    {
        let error = match error.downcast::<Self>() {
            Ok(refusal) => return refusal,
            Err(error) => error,
        };
        match error.downcast::<ReadFailure<Reason>>() {
            Ok(refusal) => refusal.map_failed(failed),
            Err(error) => ReadFailure::Failed(failed(error.into())),
        }
    }

    /// The same failure, told another way.
    pub fn map_failed<G>(self, tell: impl FnOnce(F) -> G) -> ReadFailure<G> {
        match self {
            ReadFailure::NotFile => ReadFailure::NotFile,
            ReadFailure::TooLarge(size) => ReadFailure::TooLarge(size),
            ReadFailure::NotText => ReadFailure::NotText,
            ReadFailure::Failed(failure) => ReadFailure::Failed(tell(failure)),
        }
    }
}

/// Why a save did not happen, or did not finish. `F` as for
/// [`ReadFailure`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveFailure<F> {
    /// The file is no longer what was read or last saved: changed, or gone.
    /// Nothing was written.
    Changed,
    /// The file was truncated and then the write failed: what is on disk
    /// now is incomplete, and no longer what was read.
    Interrupted(F),
    /// Nothing was written.
    Failed(F),
}

impl<F: fmt::Display> fmt::Display for SaveFailure<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SaveFailure::Changed => f.write_str("the file changed after it was opened"),
            SaveFailure::Interrupted(failure) | SaveFailure::Failed(failure) => failure.fmt(f),
        }
    }
}

impl<F: fmt::Debug + fmt::Display> Error for SaveFailure<F> {}

impl<F> SaveFailure<F> {
    /// The refusal inside `error`, told either way, a write cut short (told
    /// by its cause), or the error itself as a failure, told by `failed`.
    pub fn from_error(error: anyhow::Error, failed: impl FnOnce(Reason) -> F) -> Self
    where
        Self: Error + Send + Sync + 'static,
    {
        let error = match error.downcast::<Self>() {
            Ok(refusal) => return refusal,
            Err(error) => error,
        };
        let error = match error.downcast::<SaveFailure<Reason>>() {
            Ok(refusal) => return refusal.map_failed(failed),
            Err(error) => error,
        };
        match error.downcast::<Wrapped<EditError>>() {
            Ok(cut) if *cut.error() == EditError::Interrupted => {
                SaveFailure::Interrupted(failed(cut.into_cause().into()))
            }
            Ok(other) => SaveFailure::Failed(failed(anyhow::Error::new(other).into())),
            Err(error) => SaveFailure::Failed(failed(error.into())),
        }
    }

    /// The same failure, told another way.
    pub fn map_failed<G>(self, tell: impl FnOnce(F) -> G) -> SaveFailure<G> {
        match self {
            SaveFailure::Changed => SaveFailure::Changed,
            SaveFailure::Interrupted(failure) => SaveFailure::Interrupted(tell(failure)),
            SaveFailure::Failed(failure) => SaveFailure::Failed(tell(failure)),
        }
    }
}

/// How the engine refuses a read, inside an `anyhow` error.
type Refusal = ReadFailure<Reason>;

/// A whole file read as it is, for a preview: an image, say.
#[derive(Clone, PartialEq, Eq)]
pub struct FileBytes(Vec<u8>);

impl FileBytes {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn into_inner(self) -> Vec<u8> {
        self.0
    }
}

impl std::fmt::Debug for FileBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FileBytes({} bytes)", self.0.len())
    }
}

/// Read a remote text file, following links. Refusals come back as a
/// `ReadFailure` inside the error, so that the worker can still tell a
/// broken connection from the rest.
pub(crate) async fn read_text<F: RemoteFs>(fs: &F, path: &RemotePath) -> Result<TextFile> {
    let (bytes, stamp) = read_bytes(fs, path, EDIT_LIMIT, true).await?;
    Ok(TextFile::decode::<Reason>(bytes, stamp)?)
}

/// Read a remote file whole, following links, binary or not; refused like
/// `read_text` when it is not a file or larger than `limit`.
pub(crate) async fn read_whole<F: RemoteFs>(
    fs: &F,
    path: &RemotePath,
    limit: u64,
) -> Result<FileBytes> {
    let (bytes, _) = read_bytes(fs, path, limit, false).await?;
    Ok(FileBytes(bytes))
}

/// `stat`, refuse what is not a file or too large, then read it all. For
/// `text`, a binary file is given up on after its first chunk.
async fn read_bytes<F: RemoteFs>(
    fs: &F,
    path: &RemotePath,
    limit: u64,
    text: bool,
) -> Result<(Vec<u8>, FileStamp)> {
    let Some(metadata) = fs.stat(path).await? else {
        bail!(EditError::Missing);
    };
    if metadata.kind() != EntryKind::File {
        return Err(Refusal::NotFile.into());
    }
    if metadata.size() > limit {
        return Err(Refusal::TooLarge(metadata.size()).into());
    }
    let handle = fs.open_read(path).await?;
    let bytes = read_all(fs, &handle, metadata.size(), text).await;
    let close = fs.close(&handle).await;
    let bytes = bytes?;
    close?;
    Ok((bytes, FileStamp::of(&metadata)))
}

/// `size` bytes from the start of an open file, reading ahead in parallel.
/// Reads may come back out of order or short; each lands at its offset.
/// For `text`, a NUL early on stops it: a binary file.
async fn read_all<F: RemoteFs>(fs: &F, handle: &str, size: u64, text: bool) -> Result<Vec<u8>> {
    let mut buffer = vec![0; usize::try_from(size)?];
    let mut next = 0;
    let mut remainders: Vec<(u64, u32)> = Vec::new();
    let mut reads = FuturesUnordered::new();
    loop {
        while reads.len() < MAX_IN_FLIGHT && (!remainders.is_empty() || next < size) {
            let (at, len) = remainders.pop().unwrap_or_else(|| {
                let len = (size - next).min(u64::from(CHUNK)) as u32;
                let request = (next, len);
                next += u64::from(len);
                request
            });
            reads.push(async move { (at, len, fs.read(handle, at, len).await) });
        }
        let Some((at, len, bytes)) = reads.next().await else {
            return Ok(buffer);
        };
        let bytes = match bytes? {
            Some(bytes) if !bytes.is_empty() => bytes,
            _ => bail!(EditError::Shrank),
        };
        let got = bytes.len().min(len as usize);
        let start = at as usize;
        buffer[start..start + got].copy_from_slice(&bytes[..got]);
        // Give up on a binary file before reading all of it.
        if text && at == 0 && bytes[..got.min(BINARY_PROBE)].contains(&0) {
            return Err(Refusal::NotText.into());
        }
        if got < len as usize {
            remainders.push((at + got as u64, len - got as u32));
        }
    }
}

/// Write `bytes` over a remote file in place, following links and creating
/// it when it is gone. With `expected`, the file must still match it, or
/// the save fails with `SaveFailure::Changed` and nothing is written; a
/// write cut short after the file was truncated is [`EditError::Interrupted`]
/// over its cause.
pub(crate) async fn write_in_place<F: RemoteFs>(
    fs: &F,
    path: &RemotePath,
    bytes: Vec<u8>,
    expected: Option<FileStamp>,
) -> Result<FileStamp> {
    if let Some(expected) = expected {
        let now = fs.stat(path).await?;
        if now.as_ref().map(FileStamp::of) != Some(expected) {
            return Err(SaveFailure::<Reason>::Changed.into());
        }
    }
    let handle = fs.open_replace(path).await?;
    // From here on the file is truncated: a failure leaves it incomplete.
    // The layer keeps the cause, so a dropped connection is still told
    // apart.
    let interrupted = |error: anyhow::Error| wrap(EditError::Interrupted, error);
    let written = write_all(fs, &handle, bytes).await;
    let close = fs.close(&handle).await;
    written.map_err(interrupted)?;
    close.map_err(interrupted)?;
    let metadata = fs
        .stat(path)
        .await?
        .ok_or_else(|| anyhow!(EditError::GoneAfterSave))?;
    Ok(FileStamp::of(&metadata))
}

async fn write_all<F: RemoteFs>(fs: &F, handle: &str, bytes: Vec<u8>) -> Result<()> {
    let mut chunks = bytes
        .chunks(CHUNK as usize)
        .enumerate()
        .map(|(index, chunk)| (index as u64 * u64::from(CHUNK), chunk.to_vec()));
    let mut writes = FuturesUnordered::new();
    loop {
        while writes.len() < MAX_IN_FLIGHT {
            let Some((offset, chunk)) = chunks.next() else {
                break;
            };
            writes.push(fs.write(handle, offset, chunk));
        }
        match writes.next().await {
            Some(result) => result?,
            None => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp() -> FileStamp {
        FileStamp::new(0, None)
    }

    fn decode(bytes: impl Into<Vec<u8>>) -> Result<TextFile, ReadFailure<Reason>> {
        TextFile::decode(bytes.into(), stamp())
    }

    #[test]
    fn plain_utf8_is_kept_as_it_is() {
        let file = decode("a\nb\n中文").unwrap();
        assert_eq!(file.text(), "a\nb\n中文");
        assert_eq!(file.format(), TextFormat::default());
        assert_eq!(file.format().encode(file.text()), "a\nb\n中文".as_bytes());
    }

    #[test]
    fn crlf_lines_are_edited_as_lf_and_written_back_as_crlf() {
        let file = decode(b"a\r\nb\r\n").unwrap();
        assert_eq!(file.text(), "a\nb\n");
        assert!(file.format().crlf());
        assert_eq!(file.format().encode("a\nb\nc\n"), b"a\r\nb\r\nc\r\n");
        // A CRLF pasted in does not become CR CR LF.
        assert_eq!(file.format().encode("a\r\nb"), b"a\r\nb");
    }

    #[test]
    fn mixed_line_ends_are_left_alone() {
        let file = decode(b"a\r\nb\nc").unwrap();
        assert_eq!(file.text(), "a\r\nb\nc");
        assert!(!file.format().crlf());
        assert_eq!(file.format().encode(file.text()), b"a\r\nb\nc");
    }

    #[test]
    fn a_byte_order_mark_is_hidden_and_written_back() {
        let file = decode(b"\xEF\xBB\xBFkey=1\r\n").unwrap();
        assert_eq!(file.text(), "key=1\n");
        assert_eq!(file.format(), TextFormat::new(true, true));
        assert_eq!(file.format().encode("key=2\n"), b"\xEF\xBB\xBFkey=2\r\n");
    }

    #[test]
    fn binary_and_other_encodings_are_not_text() {
        assert_eq!(decode(b"PK\x03\x04\x00\x00"), Err(ReadFailure::NotText));
        // 「中文」 in GBK.
        assert_eq!(decode(b"\xD6\xD0\xCE\xC4"), Err(ReadFailure::NotText));
        assert_eq!(decode(Vec::new()).unwrap().text(), "");
    }

    #[test]
    fn refusals_survive_the_trip_through_anyhow() {
        let told = |reason: Reason| format!("{reason:#}");
        let error: anyhow::Error = Refusal::TooLarge(9).into();
        assert_eq!(
            ReadFailure::from_error(error, told),
            ReadFailure::TooLarge(9)
        );
        assert_eq!(
            ReadFailure::<String>::from_error(anyhow!("connection lost"), told),
            ReadFailure::Failed("connection lost".into())
        );
        let error: anyhow::Error = SaveFailure::<Reason>::Changed.into();
        assert_eq!(
            SaveFailure::<Reason>::from_error(error, |reason| reason),
            SaveFailure::Changed
        );
        // Interrupted is a layer over the cause, which stays findable and
        // is what the failure tells.
        let cause = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset");
        let error = wrap(EditError::Interrupted, cause);
        assert!(
            error
                .chain()
                .any(|c| c.downcast_ref::<std::io::Error>().is_some())
        );
        assert_eq!(
            SaveFailure::<String>::from_error(error, told),
            SaveFailure::Interrupted("reset".into())
        );
        let error = anyhow!(EditError::GoneAfterSave);
        assert!(matches!(
            SaveFailure::<Reason>::from_error(error, |reason| reason),
            SaveFailure::Failed(reason) if reason.error().is::<EditError>()
        ));
    }
}
