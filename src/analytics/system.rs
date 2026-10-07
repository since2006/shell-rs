//! The machine as Aptabase hears of it: the system's name and version.

use os_info::{Type, Version};

/// The operating system, named as Aptabase's dashboard knows them:
/// `macOS`, `Windows`, or a Linux distribution such as `Ubuntu`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemInfo {
    pub os_name: String,
    /// Empty when the system does not say.
    pub os_version: String,
}

impl SystemInfo {
    /// Ask the system. It may start a process or two (`uname`,
    /// `lsb_release`, `sw_vers`), so only the analytics thread asks.
    pub fn detect() -> Self {
        let info = os_info::get();
        Self::from_os(info.os_type(), info.version())
    }

    fn from_os(kind: Type, version: &Version) -> Self {
        let os_name = match kind {
            // os_info's own name for it is "Mac OS".
            Type::Macos => "macOS".to_string(),
            Type::Windows => "Windows".to_string(),
            Type::Unknown => std::env::consts::OS.to_string(),
            kind => kind.to_string(),
        };
        let os_version = match version {
            Version::Unknown => String::new(),
            version => version.to_string(),
        };
        Self {
            os_name,
            os_version,
        }
    }

    /// `ShellRS/0.1.5 (macOS 15.1.0; aarch64)`. Aptabase tells the devices
    /// behind one address apart by it, so it says what differs between
    /// machines and nothing that names one.
    pub fn user_agent(&self, version: &str) -> String {
        let system = format!("{} {}", self.os_name, self.os_version);
        format!(
            "ShellRS/{version} ({}; {})",
            system.trim(),
            std::env::consts::ARCH
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systems_are_named_as_aptabase_knows_them() {
        let macos = SystemInfo::from_os(Type::Macos, &Version::Semantic(15, 1, 0));
        assert_eq!(macos.os_name, "macOS");
        assert_eq!(macos.os_version, "15.1.0");
        let windows = SystemInfo::from_os(Type::Windows, &Version::Semantic(10, 0, 22631));
        assert_eq!(windows.os_name, "Windows");
        assert_eq!(windows.os_version, "10.0.22631");
        let ubuntu = SystemInfo::from_os(Type::Ubuntu, &Version::Custom("24.04".into()));
        assert_eq!(ubuntu.os_name, "Ubuntu");
        let unknown = SystemInfo::from_os(Type::Unknown, &Version::Unknown);
        assert_eq!(unknown.os_name, std::env::consts::OS);
        assert_eq!(unknown.os_version, "");
    }

    #[test]
    fn the_user_agent_names_the_system_and_architecture() {
        let macos = SystemInfo::from_os(Type::Macos, &Version::Semantic(15, 1, 0));
        assert_eq!(
            macos.user_agent("0.1.5"),
            format!("ShellRS/0.1.5 (macOS 15.1.0; {})", std::env::consts::ARCH)
        );
        let bare = SystemInfo::from_os(Type::Unknown, &Version::Unknown);
        assert_eq!(
            bare.user_agent("0.1.5"),
            format!(
                "ShellRS/0.1.5 ({}; {})",
                std::env::consts::OS,
                std::env::consts::ARCH
            )
        );
    }

    #[test]
    fn this_machine_has_a_name() {
        // os_info starts `uname` and the like.
        let _forks = crate::testing::no_forks();
        let info = SystemInfo::detect();
        assert!(!info.os_name.is_empty());
    }
}
