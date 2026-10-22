//! What a local terminal's shell may see of the environment.
//!
//! A shell starts in the environment ShellRS itself was started in, plus
//! the few variables a terminal needs. The packaging that put ShellRS on
//! this computer left some of its own behind in that environment, and
//! they are none of the user's business.

use portable_pty::CommandBuilder;

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
}
