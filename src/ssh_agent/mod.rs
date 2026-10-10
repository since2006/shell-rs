//! Agent preferences shared by settings, hosts and SSH transports.
mod picker;
pub use picker::{AgentPicker, AgentPickerEvent};

use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;

/// A preference, resolved anew for each connection. Never contains keys.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "path", rename_all = "snake_case")]
pub enum AgentSelection {
    #[default]
    Auto,
    Environment,
    Path(PathBuf),
}

impl AgentSelection {
    pub fn custom(value: &str) -> Result<Self, gpui_kit::SharedString> {
        let path = PathBuf::from(value.trim());
        if !path.is_absolute() {
            return Err(crate::i18n::t!("agent.path_invalid"));
        }
        Ok(Self::Path(path))
    }
}

/// Known endpoints, without connecting or asking the agent for identities.
/// Called on a worker; the picker must never prompt for authorization.
pub fn detected_agents() -> Vec<(String, PathBuf)> {
    #[cfg(unix)]
    {
        socket_candidates(known_agents(dirs::home_dir().as_deref()))
    }
    #[cfg(not(unix))]
    {
        Vec::new()
    }
}

#[cfg(unix)]
fn socket_candidates(candidates: Vec<(String, PathBuf)>) -> Vec<(String, PathBuf)> {
    use std::os::unix::fs::FileTypeExt;
    let mut agents = Vec::new();
    for (name, path) in candidates {
        if std::fs::metadata(&path).is_ok_and(|metadata| metadata.file_type().is_socket())
            && !agents.iter().any(|(_, existing)| existing == &path)
        {
            agents.push((name, path));
        }
    }
    agents
}

#[cfg(unix)]
pub(crate) fn known_agents(home: Option<&Path>) -> Vec<(String, PathBuf)> {
    #[cfg(target_os = "macos")]
    {
        macos_agents(home)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let Some(home) = home else { return Vec::new() };
        [
            ("Bitwarden", ".bitwarden-ssh-agent.sock"),
            ("1Password", ".1password/agent.sock"),
        ]
        .into_iter()
        .map(|(name, path)| (name.into(), home.join(path)))
        .collect()
    }
}

#[cfg(any(target_os = "macos", all(test, unix)))]
pub(crate) fn macos_agents(home: Option<&Path>) -> Vec<(String, PathBuf)> {
    let Some(home) = home else { return Vec::new() };
    // Vendor defaults shared by the picker and automatic connection selection.
    [
        ("Bitwarden", ".bitwarden-ssh-agent.sock"),
        (
            "Bitwarden (App Store)",
            "Library/Containers/com.bitwarden.desktop/Data/.bitwarden-ssh-agent.sock",
        ),
        (
            "1Password",
            "Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock",
        ),
    ]
    .into_iter()
    .map(|(name, path)| (name.into(), home.join(path)))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::AgentSelection;

    #[test]
    fn custom_paths_reject_empty_relative_and_shell_expansion() {
        for path in ["", "relative.sock", "~/agent.sock", "$HOME/agent.sock"] {
            assert!(AgentSelection::custom(path).is_err());
        }
        let path = std::env::temp_dir().join("agent with spaces.sock");
        assert_eq!(
            AgentSelection::custom(path.to_str().unwrap()).unwrap(),
            AgentSelection::Path(path)
        );
    }

    #[cfg(unix)]
    #[test]
    fn discovery_lists_only_sockets_and_deduplicates_without_connecting() {
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let socket = dir.path().join("agent");
        let _listener = UnixListener::bind(&socket).unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, "not a socket").unwrap();
        let agents = super::socket_candidates(vec![
            ("first".into(), socket.clone()),
            ("duplicate".into(), socket.clone()),
            ("file".into(), file),
            ("missing".into(), dir.path().join("missing")),
        ]);
        assert_eq!(agents, vec![("first".into(), socket)]);
        _listener.set_nonblocking(true).unwrap();
        assert_eq!(
            _listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert!(super::known_agents(None).is_empty());
    }
}
