//! Where ShellRS keeps its data on disk.

use std::path::PathBuf;

/// Overrides the data directory. Set by tests and handy during development.
const DATA_DIR_ENV: &str = "SHELLRS_DATA_DIR";

/// The directory holding ShellRS's own files, e.g.
/// `~/Library/Application Support/shellrs` on macOS. Falls back to the
/// current directory on the platforms where `dirs` has nothing to offer.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(DATA_DIR_ENV) {
        return PathBuf::from(dir);
    }
    dirs::data_dir()
        .map(|dir| dir.join("shellrs"))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The SQLite file holding sessions and groups. The parent directory is
/// created if it does not exist yet.
pub fn database_path() -> std::io::Result<PathBuf> {
    let dir = data_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("shellrs.db"))
}

/// The interface settings, a small JSON file beside the database.
pub fn settings_path() -> std::io::Result<PathBuf> {
    let dir = data_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("settings.json"))
}

/// Where the running app listens for the `shellrs` command. The command
/// and the app both ask here, so both follow `SHELLRS_DATA_DIR`.
pub fn cli_socket_path() -> PathBuf {
    data_dir().join("cli.sock")
}

/// ShellRS's private host-key trust store. It deliberately does not read or
/// modify OpenSSH's `~/.ssh/known_hosts`.
pub fn known_hosts_path() -> std::io::Result<PathBuf> {
    let dir = data_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("known_hosts"))
}
