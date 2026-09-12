//! Where shellr keeps its data on disk.

use std::path::PathBuf;

/// Overrides the data directory. Set by tests and handy during development.
const DATA_DIR_ENV: &str = "SHELLR_DATA_DIR";

/// The directory holding shellr's own files, e.g.
/// `~/Library/Application Support/shellr` on macOS. Falls back to the
/// current directory on the platforms where `dirs` has nothing to offer.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(DATA_DIR_ENV) {
        return PathBuf::from(dir);
    }
    dirs::data_dir()
        .map(|dir| dir.join("shellr"))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The SQLite file holding sessions and groups. The parent directory is
/// created if it does not exist yet.
pub fn database_path() -> std::io::Result<PathBuf> {
    let dir = data_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("shellr.db"))
}

/// shellr's private host-key trust store. It deliberately does not read or
/// modify OpenSSH's `~/.ssh/known_hosts`.
pub fn known_hosts_path() -> std::io::Result<PathBuf> {
    let dir = data_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("known_hosts"))
}
