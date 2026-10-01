//! What can go wrong while checking for, fetching or installing an update,
//! worded for the person reading 设置 › 关于 or the update dialog.

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateError {
    /// The server could not be reached, or the transfer broke off.
    Network(String),
    /// The server answered, but not with the file.
    Http(u16),
    /// Nothing arrived for a while in the middle of a download.
    Stalled,
    /// The manifest is not signed by a key this build trusts, or was
    /// signed for another channel or version.
    BadSignature,
    /// The manifest could not be read.
    BadManifest(String),
    /// The downloaded file is not the one the manifest describes.
    Corrupt,
    /// Not enough room for the download and the unpacked copy.
    NoSpace,
    /// Reading or writing a local file failed.
    Disk(String),
    /// Putting the new version in place failed.
    Install(String),
    /// Stopped on purpose: a newer check, or automatic updates turned off.
    Cancelled,
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(reason) => write!(f, "无法连接更新服务器：{reason}"),
            Self::Http(status) => write!(f, "更新服务器返回 HTTP {status}"),
            Self::Stalled => f.write_str("下载长时间没有进展，已中断"),
            Self::BadSignature => f.write_str("更新清单的签名无效"),
            Self::BadManifest(reason) => write!(f, "无法读取更新清单：{reason}"),
            Self::Corrupt => f.write_str("下载的文件与更新清单不符"),
            Self::NoSpace => f.write_str("磁盘空间不足"),
            Self::Disk(reason) => write!(f, "无法写入更新文件：{reason}"),
            Self::Install(reason) => write!(f, "无法安装新版本：{reason}"),
            Self::Cancelled => f.write_str("已取消"),
        }
    }
}

impl std::error::Error for UpdateError {}

impl From<std::io::Error> for UpdateError {
    fn from(error: std::io::Error) -> Self {
        Self::Disk(error.to_string())
    }
}
