//! Whole text files for the editor: read into memory, written back in place.
//!
//! Unlike transfers there is no `.filepart` and no rename: a save truncates
//! the file and writes it again, as vim, nano and VS Code Remote do, so the
//! owner, the permissions, links pointing at it and hard links all stay.
//! What keeps a save from clobbering someone else's change is the stamp: the
//! size and modification time the file had when it was read or last saved.

use super::{EntryKind, FileMetadata, LocalDirectoryProvider, RemotePath, client::RemoteFs};
use anyhow::{Result, anyhow, bail};
use futures::{StreamExt as _, stream::FuturesUnordered};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
    /// mixed line ends is kept exactly as it is.
    pub fn decode(mut bytes: Vec<u8>, stamp: FileStamp) -> Result<Self, ReadFailure> {
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

/// Why a file was not opened for editing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadFailure {
    /// A directory, a device or the like.
    NotFile,
    /// Larger than `EDIT_LIMIT`; the size in bytes.
    TooLarge(u64),
    /// Binary, or text in an encoding other than UTF-8.
    NotText,
    Failed(String),
}

impl std::fmt::Display for ReadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadFailure::NotFile => f.write_str("不是普通文件"),
            ReadFailure::TooLarge(size) => write!(f, "文件太大（{size} 字节）"),
            ReadFailure::NotText => f.write_str("不是 UTF-8 文本文件"),
            ReadFailure::Failed(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ReadFailure {}

impl ReadFailure {
    /// The refusal inside `error`, or the error itself as a failure.
    pub(crate) fn from_error(error: anyhow::Error) -> Self {
        match error.downcast::<ReadFailure>() {
            Ok(refusal) => refusal,
            Err(error) => ReadFailure::Failed(format!("{error:#}")),
        }
    }
}

/// Why a save did not happen, or did not finish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveFailure {
    /// The file is no longer what was read or last saved: changed, or gone.
    /// Nothing was written.
    Changed,
    /// The file was truncated and then the write failed: what is on disk
    /// now is incomplete, and no longer what was read.
    Interrupted(String),
    /// Nothing was written.
    Failed(String),
}

impl std::fmt::Display for SaveFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveFailure::Changed => f.write_str("文件在打开后已被修改"),
            SaveFailure::Interrupted(message) | SaveFailure::Failed(message) => {
                f.write_str(message)
            }
        }
    }
}

impl std::error::Error for SaveFailure {}

impl SaveFailure {
    pub(crate) fn from_error(error: anyhow::Error) -> Self {
        match error.downcast::<SaveFailure>() {
            Ok(refusal) => refusal,
            Err(error) => SaveFailure::Failed(format!("{error:#}")),
        }
    }
}

/// Read a remote text file, following links. Refusals come back as a
/// `ReadFailure` inside the error, so that the worker can still tell a
/// broken connection from the rest.
pub(crate) async fn read_text<F: RemoteFs>(fs: &F, path: &RemotePath) -> Result<TextFile> {
    let Some(metadata) = fs.stat(path).await? else {
        bail!("文件不存在");
    };
    if metadata.kind() != EntryKind::File {
        return Err(ReadFailure::NotFile.into());
    }
    if metadata.size() > EDIT_LIMIT {
        return Err(ReadFailure::TooLarge(metadata.size()).into());
    }
    let handle = fs.open_read(path).await?;
    let bytes = read_all(fs, &handle, metadata.size()).await;
    let close = fs.close(&handle).await;
    let bytes = bytes?;
    close?;
    Ok(TextFile::decode(bytes, FileStamp::of(&metadata))?)
}

/// Read a local text file through `provider`. Blocking, like the provider.
pub fn read_local_text(
    provider: &dyn LocalDirectoryProvider,
    path: &std::path::Path,
) -> Result<TextFile, ReadFailure> {
    let (bytes, stamp) = provider
        .read_file(path, EDIT_LIMIT)
        .map_err(ReadFailure::from_error)?;
    TextFile::decode(bytes, stamp)
}

/// Write the editor's bytes over a local file through `provider`. Blocking.
pub fn write_local_text(
    provider: &dyn LocalDirectoryProvider,
    path: &std::path::Path,
    bytes: &[u8],
    expected: Option<FileStamp>,
) -> Result<FileStamp, SaveFailure> {
    provider
        .write_file(path, bytes, expected)
        .map_err(SaveFailure::from_error)
}

/// `size` bytes from the start of an open file, reading ahead in parallel.
/// Reads may come back out of order or short; each lands at its offset.
async fn read_all<F: RemoteFs>(fs: &F, handle: &str, size: u64) -> Result<Vec<u8>> {
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
            _ => bail!("文件在读取时变短了，请重新打开"),
        };
        let got = bytes.len().min(len as usize);
        let start = at as usize;
        buffer[start..start + got].copy_from_slice(&bytes[..got]);
        // Give up on a binary file before reading all of it.
        if at == 0 && bytes[..got.min(BINARY_PROBE)].contains(&0) {
            return Err(ReadFailure::NotText.into());
        }
        if got < len as usize {
            remainders.push((at + got as u64, len - got as u32));
        }
    }
}

/// Write `bytes` over a remote file in place, following links and creating
/// it when it is gone. With `expected`, the file must still match it, or
/// the save fails with `SaveFailure::Changed` and nothing is written.
pub(crate) async fn write_in_place<F: RemoteFs>(
    fs: &F,
    path: &RemotePath,
    bytes: Vec<u8>,
    expected: Option<FileStamp>,
) -> Result<FileStamp> {
    if let Some(expected) = expected {
        let now = fs.stat(path).await?;
        if now.as_ref().map(FileStamp::of) != Some(expected) {
            return Err(SaveFailure::Changed.into());
        }
    }
    let handle = fs.open_replace(path).await?;
    // From here on the file is truncated: a failure leaves it incomplete.
    // The context keeps the cause, so a dropped connection is still told
    // apart.
    let interrupted = |error: anyhow::Error| {
        let message = format!("{error:#}");
        error.context(SaveFailure::Interrupted(message))
    };
    let written = match write_all(fs, &handle, bytes).await {
        Ok(()) => fs.sync(&handle).await,
        Err(error) => Err(error),
    };
    let close = fs.close(&handle).await;
    written.map_err(interrupted)?;
    close.map_err(interrupted)?;
    let metadata = fs
        .stat(path)
        .await?
        .ok_or_else(|| anyhow!("保存后找不到文件"))?;
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

    #[test]
    fn plain_utf8_is_kept_as_it_is() {
        let file = TextFile::decode("a\nb\n中文".into(), stamp()).unwrap();
        assert_eq!(file.text(), "a\nb\n中文");
        assert_eq!(file.format(), TextFormat::default());
        assert_eq!(file.format().encode(file.text()), "a\nb\n中文".as_bytes());
    }

    #[test]
    fn crlf_lines_are_edited_as_lf_and_written_back_as_crlf() {
        let file = TextFile::decode(b"a\r\nb\r\n".to_vec(), stamp()).unwrap();
        assert_eq!(file.text(), "a\nb\n");
        assert!(file.format().crlf());
        assert_eq!(file.format().encode("a\nb\nc\n"), b"a\r\nb\r\nc\r\n");
        // A CRLF pasted in does not become CR CR LF.
        assert_eq!(file.format().encode("a\r\nb"), b"a\r\nb");
    }

    #[test]
    fn mixed_line_ends_are_left_alone() {
        let file = TextFile::decode(b"a\r\nb\nc".to_vec(), stamp()).unwrap();
        assert_eq!(file.text(), "a\r\nb\nc");
        assert!(!file.format().crlf());
        assert_eq!(file.format().encode(file.text()), b"a\r\nb\nc");
    }

    #[test]
    fn a_byte_order_mark_is_hidden_and_written_back() {
        let file = TextFile::decode(b"\xEF\xBB\xBFkey=1\r\n".to_vec(), stamp()).unwrap();
        assert_eq!(file.text(), "key=1\n");
        assert_eq!(file.format(), TextFormat::new(true, true));
        assert_eq!(file.format().encode("key=2\n"), b"\xEF\xBB\xBFkey=2\r\n");
    }

    #[test]
    fn binary_and_other_encodings_are_not_text() {
        assert_eq!(
            TextFile::decode(b"PK\x03\x04\x00\x00".to_vec(), stamp()),
            Err(ReadFailure::NotText)
        );
        // 「中文」 in GBK.
        assert_eq!(
            TextFile::decode(b"\xD6\xD0\xCE\xC4".to_vec(), stamp()),
            Err(ReadFailure::NotText)
        );
        assert_eq!(TextFile::decode(Vec::new(), stamp()).unwrap().text(), "");
    }

    #[test]
    fn refusals_survive_the_trip_through_anyhow() {
        let error: anyhow::Error = ReadFailure::TooLarge(9).into();
        assert_eq!(ReadFailure::from_error(error), ReadFailure::TooLarge(9));
        assert_eq!(
            ReadFailure::from_error(anyhow!("连接已断开")),
            ReadFailure::Failed("连接已断开".into())
        );
        let error: anyhow::Error = SaveFailure::Changed.into();
        assert_eq!(SaveFailure::from_error(error), SaveFailure::Changed);
        // Interrupted rides as context over the cause, which stays findable.
        let cause = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset");
        let error = anyhow::Error::from(cause).context(SaveFailure::Interrupted("reset".into()));
        assert!(
            error
                .chain()
                .any(|c| c.downcast_ref::<std::io::Error>().is_some())
        );
        assert_eq!(
            SaveFailure::from_error(error),
            SaveFailure::Interrupted("reset".into())
        );
    }
}
