//! Which package of a release fits this machine.
//!
//! The keys are part of the manifest format, so they never change. A
//! universal macOS build runs its native half, so an Apple Silicon Mac asks
//! for `macos-aarch64` even though both keys point at the same package; an
//! Intel-only build under Rosetta would ask for `macos-x86_64` and keep
//! getting Intel builds, which is why macOS ships universal.

/// The manifest's key for this build's platform.
pub fn platform_key() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "macos-aarch64"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "macos-x86_64"
    } else if cfg!(all(windows, target_arch = "x86_64")) {
        "windows-x86_64"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "linux-x86_64"
    } else {
        "unsupported"
    }
}

/// The platform as 设置 › 关于 names it.
pub fn platform_label() -> &'static str {
    match platform_key() {
        "macos-aarch64" => "macOS · Apple Silicon",
        "macos-x86_64" => "macOS · Intel",
        "windows-x86_64" => "Windows · x64",
        "linux-x86_64" => "Linux · x64",
        _ => std::env::consts::OS,
    }
}

/// `ShellRS/0.2.0 (macos; aarch64)`: all the update server learns about
/// the machine asking.
pub fn user_agent(version: &str) -> String {
    format!(
        "ShellRS/{version} ({}; {})",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_agent_names_the_version_system_and_architecture() {
        let agent = user_agent("1.2.3");
        assert!(agent.starts_with("ShellRS/1.2.3 ("), "{agent}");
        assert!(agent.contains(std::env::consts::OS), "{agent}");
        assert!(
            agent.ends_with(&format!("{})", std::env::consts::ARCH)),
            "{agent}"
        );
    }

    #[test]
    fn this_machine_has_a_package() {
        assert_ne!(platform_key(), "unsupported");
        assert!(!platform_label().is_empty());
    }
}
