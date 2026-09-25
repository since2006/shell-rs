//! Where ShellRS keeps its data on disk.

use std::path::{Path, PathBuf};

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
    cli_endpoint(&data_dir())
}

/// Where an app keeping its data in `dir` listens for the `shellrs`
/// command: a socket in that directory, or on Windows a named pipe.
///
/// Pipe names are one namespace for the whole machine, so the name is made
/// from the data directory. That lives in the user's profile, so each user,
/// and each `SHELLRS_DATA_DIR`, gets a pipe of its own.
pub fn cli_endpoint(dir: &Path) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(pipe_name(dir))
    } else {
        dir.join("cli.sock")
    }
}

/// `\\.\pipe\shellrs-cli-` and a hash of the directory. Windows paths
/// ignore case, so the hash does too.
fn pipe_name(dir: &Path) -> String {
    use sha2::{Digest as _, Sha256};

    let dir = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let digest = Sha256::digest(dir.to_string_lossy().to_lowercase().as_bytes());
    let hex: String = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!(r"\\.\pipe\shellrs-cli-{hex}")
}

/// ShellRS's private host-key trust store. It deliberately does not read or
/// modify OpenSSH's `~/.ssh/known_hosts`.
pub fn known_hosts_path() -> std::io::Result<PathBuf> {
    let dir = data_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("known_hosts"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::pipe_name;

    #[test]
    fn each_data_directory_gets_a_pipe_of_its_own() {
        let pipe = pipe_name(Path::new("/Users/me/AppData/Roaming/shellrs"));
        assert!(pipe.starts_with(r"\\.\pipe\shellrs-cli-"));
        assert_eq!(pipe.len(), r"\\.\pipe\shellrs-cli-".len() + 16);
        assert_eq!(
            pipe,
            pipe_name(Path::new("/USERS/me/appdata/Roaming/SHELLRS"))
        );
        assert_ne!(
            pipe,
            pipe_name(Path::new("/Users/other/AppData/Roaming/shellrs"))
        );
    }
}
