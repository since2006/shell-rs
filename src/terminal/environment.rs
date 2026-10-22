//! What a local terminal's shell may see of the environment.
//!
//! A shell starts in the environment ShellRS itself was started in, plus
//! the few variables a terminal needs. The packaging that put ShellRS on
//! this computer left some of its own behind in that environment, and
//! they are none of the user's business.
//!
//! What ShellRS puts in is the user's to say: the settings have a switch
//! for each of it, and one that turns them all off.

use portable_pty::CommandBuilder;
use serde::{Deserialize, Serialize};

/// The variables the packaging leaves behind.
///
/// `ARGV0` is the one that bites: zsh takes it for a special parameter
/// and writes it into the argv[0] of every command it runs, and bash has
/// no such parameter, so only zsh is hit. rustup's proxies are one
/// binary that tells which tool it is by its own argv[0], so in such a
/// shell `cargo`, `rustc`, `rustfmt` and the rest all stop working, each
/// of them saying it does not know the proxy name.
///
/// The AppImage runtime sets these: `ARGV0` is the AppImage's own name,
/// `APPIMAGE` the file it is, `APPDIR` the folder it is mounted in (gone
/// once ShellRS exits) and `OWD` the folder it was started in.
///
/// They are taken out of the shell only. ShellRS itself needs `APPIMAGE`
/// to start itself again after an update.
pub const LAUNCHER_VARS: &[&str] = &["ARGV0", "APPIMAGE", "APPDIR", "OWD"];

/// Take the packaging's leftovers out of the environment a shell starts
/// in. A variable that is not there is nothing to take out.
pub fn strip_launcher_vars(command: &mut CommandBuilder) {
    for name in LAUNCHER_VARS {
        command.env_remove(name);
    }
}

/// What a local terminal's shell may see: what ShellRS puts in, and how
/// much of its own process it hands over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalEnvironment {
    /// 设置 TERM_PROGRAM=ShellRS: lets programs know they run in ShellRS.
    pub term_program: bool,
    /// 设置 COLORTERM=truecolor: tells programs they may use true color.
    pub color_term: bool,
    /// 设置 TERM=xterm-256color: tells programs what terminal they run in.
    pub term: bool,
    /// 继承 ShellRS 进程的环境变量: what ShellRS itself was started with.
    pub inherit: bool,
}

impl Default for LocalEnvironment {
    fn default() -> Self {
        Self {
            term_program: true,
            color_term: true,
            term: true,
            inherit: true,
        }
    }
}

impl LocalEnvironment {
    /// 干净模式: nothing of ShellRS's own, only what a shell needs.
    pub fn clean() -> Self {
        Self {
            term_program: false,
            color_term: false,
            term: false,
            inherit: false,
        }
    }

    /// What this environment asks of the shell that starts.
    pub fn apply(&self, command: &mut CommandBuilder) {
        // The white list first: it clears what the builder holds, so the
        // three below have to come after it.
        if !self.inherit {
            keep_only(command, self);
        }
        strip_launcher_vars(command);
        if self.term {
            command.env("TERM", "xterm-256color");
        } else {
            command.env_remove("TERM");
        }
        if self.color_term {
            command.env("COLORTERM", "truecolor");
        } else {
            command.env_remove("COLORTERM");
        }
        if self.term_program {
            command.env("TERM_PROGRAM", "ShellRS");
        } else {
            command.env_remove("TERM_PROGRAM");
        }
    }
}

/// Leave only what a shell needs, and nothing of what ShellRS was
/// started with.
fn keep_only(command: &mut CommandBuilder, environment: &LocalEnvironment) {
    // The builder's own environment, not the process's: on Windows
    // portable-pty adds what the registry holds, and the process's own
    // does not have it.
    let keep: Vec<(String, String)> = command
        .iter_full_env_as_str()
        .filter(|(name, _)| keeps(name, environment))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();
    command.env_clear();
    for (name, value) in keep {
        command.env(name, value);
    }
}

/// Whether a variable of the environment is one the shell may keep.
fn keeps(name: &str, environment: &LocalEnvironment) -> bool {
    // The three the terminal sets are kept only when it sets them.
    if same_name(name, "TERM") {
        return environment.term;
    }
    if same_name(name, "COLORTERM") {
        return environment.color_term;
    }
    if same_name(name, "TERM_PROGRAM") {
        return environment.term_program;
    }
    // Git's own, which the user's tools read.
    if name.starts_with("GIT_") {
        return true;
    }
    KEPT_VARS.iter().any(|kept| same_name(name, kept))
}

/// Whether two names are the same variable: the case does not matter on
/// Windows, and it does everywhere else.
fn same_name(one: &str, other: &str) -> bool {
    if cfg!(windows) {
        one.eq_ignore_ascii_case(other)
    } else {
        one == other
    }
}

/// The variables a shell needs, and the user's own that are worth
/// keeping: the ones a program reads to know where things are.
#[cfg(unix)]
const KEPT_VARS: &[&str] = &[
    // Where things are.
    "HOME",
    "USER",
    "LOGNAME",
    "PATH",
    "SHELL",
    "PWD",
    "TMPDIR",
    // The language.
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "LC_COLLATE",
    "LC_NUMERIC",
    "LC_TIME",
    "LC_MONETARY",
    // The clock.
    "TZ",
    // The agent, without which `ssh` cannot use the keys.
    "SSH_AUTH_SOCK",
    "SSH_AGENT_PID",
    // The desktop.
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "DBUS_SESSION_BUS_ADDRESS",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XAUTHORITY",
    // What the user's own programs read.
    "EDITOR",
    "VISUAL",
    "PAGER",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
];

/// The variables a shell needs on Windows, and the user's own that are
/// worth keeping.
#[cfg(windows)]
const KEPT_VARS: &[&str] = &[
    // Where things are.
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "PROGRAMW6432",
    "COMMONPROGRAMFILES",
    "COMMONPROGRAMFILES(X86)",
    "PUBLIC",
    "ALLUSERSPROFILE",
    "TEMP",
    "TMP",
    // Who runs it.
    "USERNAME",
    "USERDOMAIN",
    "COMPUTERNAME",
    // What the machine is.
    "OS",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "PROCESSOR_IDENTIFIER",
    "PROCESSOR_REVISION",
];

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    #[test]
    fn the_strip_takes_the_packaging_out_and_leaves_the_rest() {
        let mut command = CommandBuilder::new("/bin/sh");
        // What the AppImage runtime leaves in the environment.
        for name in LAUNCHER_VARS {
            command.env(name, "left-over");
        }
        command.env("PATH", "/usr/bin");
        strip_launcher_vars(&mut command);
        for name in LAUNCHER_VARS {
            assert_eq!(command.get_env(name), None, "{name} is left in");
        }
        assert_eq!(command.get_env("PATH"), Some(OsStr::new("/usr/bin")));
    }

    #[test]
    fn a_shell_without_the_packaging_keeps_its_environment() {
        let mut command = CommandBuilder::new("/bin/sh");
        command.env("PATH", "/usr/bin");
        strip_launcher_vars(&mut command);
        assert_eq!(command.get_env("PATH"), Some(OsStr::new("/usr/bin")));
    }

    #[test]
    fn the_clean_environment_keeps_only_what_a_shell_needs() {
        let mut command = CommandBuilder::new("/bin/sh");
        command.env("PATH", "/usr/bin");
        command.env("HOME", "/home/tester");
        command.env("SSH_AUTH_SOCK", "/tmp/agent");
        command.env("LD_LIBRARY_PATH", "/opt/lib");
        command.env("VIRTUAL_ENV", "/opt/venv");
        LocalEnvironment::clean().apply(&mut command);
        assert_eq!(command.get_env("PATH"), Some(OsStr::new("/usr/bin")));
        assert_eq!(command.get_env("HOME"), Some(OsStr::new("/home/tester")));
        // The agent, without which `ssh` cannot use the keys.
        assert_eq!(
            command.get_env("SSH_AUTH_SOCK"),
            Some(OsStr::new("/tmp/agent"))
        );
        // What ShellRS was started with is gone.
        assert_eq!(command.get_env("LD_LIBRARY_PATH"), None);
        assert_eq!(command.get_env("VIRTUAL_ENV"), None);
        // And the three the terminal sets are not set either.
        assert_eq!(command.get_env("TERM"), None);
        assert_eq!(command.get_env("COLORTERM"), None);
        assert_eq!(command.get_env("TERM_PROGRAM"), None);
    }

    #[test]
    fn a_terminal_that_sets_nothing_leaves_the_variables_out() {
        let mut command = CommandBuilder::new("/bin/sh");
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        command.env("TERM_PROGRAM", "iTerm.app");
        LocalEnvironment::clean().apply(&mut command);
        assert_eq!(command.get_env("TERM"), None);
        assert_eq!(command.get_env("COLORTERM"), None);
        assert_eq!(command.get_env("TERM_PROGRAM"), None);
    }

    #[test]
    fn the_terminal_sets_what_it_says_it_does() {
        let mut command = CommandBuilder::new("/bin/sh");
        command.env("TERM", "dumb");
        command.env("TERM_PROGRAM", "iTerm.app");
        LocalEnvironment::default().apply(&mut command);
        assert_eq!(command.get_env("TERM"), Some(OsStr::new("xterm-256color")));
        assert_eq!(command.get_env("COLORTERM"), Some(OsStr::new("truecolor")));
        assert_eq!(command.get_env("TERM_PROGRAM"), Some(OsStr::new("ShellRS")));
    }

    #[test]
    fn the_white_list_holds_what_a_shell_needs() {
        for name in ["HOME", "PATH", "SHELL", "SSH_AUTH_SOCK"] {
            assert!(KEPT_VARS.contains(&name), "{name} is not kept");
        }
        for name in ["LD_LIBRARY_PATH", "VIRTUAL_ENV", "PYTHONPATH"] {
            assert!(!KEPT_VARS.contains(&name), "{name} is kept");
        }
    }
}
