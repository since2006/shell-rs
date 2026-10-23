//! What a local terminal runs: the system's default shell, or the program
//! 设置 › 终端 › 本地终端 names; and the shells installed here, to offer
//! for it.

use std::ffi::OsString;
#[cfg(any(unix, test))]
use std::path::Path;
use std::path::PathBuf;

use anyhow::{Result, anyhow};
use portable_pty::CommandBuilder;

use crate::i18n::t;

use super::LocalEnvironment;

/// The program a local terminal runs, as the user wrote it: a path or a
/// command, with any arguments. Without one, the system's default shell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalShell {
    command: Option<String>,
}

impl LocalShell {
    /// `command` without the blanks around it; blank is the default shell.
    pub fn new(command: &str) -> Self {
        let command = command.trim();
        Self {
            command: (!command.is_empty()).then(|| command.to_string()),
        }
    }

    pub fn command(&self) -> Option<&str> {
        self.command.as_deref()
    }

    /// What to spawn. Blocking: it looks at the file system, so it runs on
    /// the transport's thread.
    ///
    /// A shell named on its own starts as a login shell, as the default one
    /// does, so it reads the same profile (Homebrew's `PATH` is set up in
    /// `~/.zprofile`): portable-pty runs `$SHELL` with a `-` before its
    /// name. Anything else, or a shell with arguments, runs as written.
    pub(super) fn command_builder(&self) -> Result<CommandBuilder> {
        let Some(command) = &self.command else {
            return Ok(default_command());
        };
        let argv = split_command(command, QUOTING)
            .ok_or_else(|| anyhow!(t!("terminal.pty.unmatched_quote", command = command)))?;
        #[cfg(unix)]
        if let [program] = argv.as_slice()
            && is_shell_name(program)
            && let Some(path) = find_program(program)
        {
            let mut builder = CommandBuilder::new_default_prog();
            builder.env("SHELL", path);
            return Ok(builder);
        }
        Ok(CommandBuilder::from_argv(
            argv.into_iter().map(OsString::from).collect(),
        ))
    }
}

/// How a local terminal starts: the shell, the folder it starts in (the
/// home folder without one), what it showed when it last closed, to
/// draw again first, and the environment the shell may see.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalLaunch {
    pub shell: LocalShell,
    pub directory: Option<PathBuf>,
    pub replay: Option<Vec<u8>>,
    /// 本地终端的环境.
    pub environment: LocalEnvironment,
}

impl LocalLaunch {
    pub fn new(shell: LocalShell) -> Self {
        Self {
            shell,
            ..Self::default()
        }
    }
}

/// The system's default shell. On Unix the user's: `$SHELL`, or the one
/// the passwd database names. On Windows PowerShell, as Windows Terminal
/// and VS Code open: PowerShell 7 where it is installed, otherwise the
/// Windows PowerShell every Windows has; only without either the command
/// prompt (`%ComSpec%`), portable-pty's own default.
fn default_command() -> CommandBuilder {
    #[cfg(windows)]
    if let Some(program) = windows_default_shell() {
        return CommandBuilder::new(program);
    }
    CommandBuilder::new_default_prog()
}

/// What the default shell is here, for 「自动检测（/bin/zsh）」: its path
/// on Unix, its file name on Windows, as the shells offered are written.
/// Blocking.
pub fn default_shell_name() -> String {
    #[cfg(windows)]
    {
        let program = windows_default_shell()
            .unwrap_or_else(|| PathBuf::from(CommandBuilder::new_default_prog().get_shell()));
        program
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| program.to_string_lossy().into_owned())
    }
    #[cfg(not(windows))]
    CommandBuilder::new_default_prog().get_shell()
}

/// This computer's name, as shells put it in a title: `uname -n` on Unix
/// (zsh's `%m` and bash's `\h` keep what comes before the first dot), the
/// computer name on Windows.
pub(super) fn host_name() -> Option<String> {
    #[cfg(unix)]
    {
        let mut name = [0u8; 256];
        // SAFETY: `gethostname` writes at most the buffer's length.
        if unsafe { libc::gethostname(name.as_mut_ptr().cast(), name.len()) } != 0 {
            return None;
        }
        let end = name
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(name.len());
        String::from_utf8(name[..end].to_vec())
            .ok()
            .filter(|name| !name.is_empty())
    }
    #[cfg(windows)]
    std::env::var("COMPUTERNAME")
        .ok()
        .filter(|name| !name.is_empty())
}

/// The shells to offer, each as the field would hold it. On Unix the
/// common ones that are here (`COMMON_SHELLS`), one line per program; on
/// Windows PowerShell 7 where it is installed, Windows PowerShell and the
/// command prompt. Blocking.
pub fn installed_shells() -> Vec<String> {
    #[cfg(unix)]
    {
        let listed = std::fs::read_to_string("/etc/shells")
            .map(|text| listed_shells(&text))
            .unwrap_or_default();
        let mut folders: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|paths| {
                std::env::split_paths(&paths)
                    .filter(|folder| folder.is_absolute())
                    .collect()
            })
            .unwrap_or_default();
        folders.extend(SHELL_FOLDERS.iter().map(PathBuf::from));
        common_shells(&listed, &folders, is_executable, program_key)
    }
    #[cfg(windows)]
    {
        let mut shells = Vec::new();
        if find_on_path("pwsh.exe").is_some() {
            shells.push("pwsh.exe".to_string());
        } else if let Some(pwsh) = installed_pwsh() {
            shells.push(quote_word(&pwsh.to_string_lossy()));
        }
        if windows_powershell().is_some() {
            shells.push("powershell.exe".to_string());
        }
        shells.push("cmd.exe".to_string());
        shells
    }
}

/// How the words of a command are quoted.
// Each platform uses only its own; the tests use both everywhere.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Quoting {
    /// As a POSIX shell reads them: single quotes, double quotes, and
    /// backslashes that take the next character as it is.
    Posix,
    /// As Windows programs read their command line, near enough: double
    /// quotes only; a backslash is part of the path.
    Windows,
}

#[cfg(not(windows))]
const QUOTING: Quoting = Quoting::Posix;
#[cfg(windows)]
const QUOTING: Quoting = Quoting::Windows;

/// The words of `line`, split at blanks outside quotes. `None` when a
/// quote is left open.
fn split_command(line: &str, quoting: Quoting) -> Option<Vec<String>> {
    let posix = quoting == Quoting::Posix;
    let mut words = Vec::new();
    let mut word = String::new();
    // Whether a word has begun: `""` is a word, empty.
    let mut in_word = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        // Inside double quotes a backslash quotes only what
                        // would otherwise end or expand them.
                        '\\' if posix => match chars.next()? {
                            c @ ('"' | '\\' | '$' | '`') => word.push(c),
                            c => {
                                word.push('\\');
                                word.push(c);
                            }
                        },
                        c => word.push(c),
                    }
                }
            }
            '\'' if posix => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => word.push(c),
                    }
                }
            }
            '\\' if posix => {
                in_word = true;
                // A backslash at the very end stands for itself.
                word.push(chars.next().unwrap_or('\\'));
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    Some(words)
}

/// `word` as the field writes it, so `split_command` reads it back as one
/// word: a file chosen with the system's picker. On Windows it is wrapped
/// in double quotes when it has blanks; a path cannot have a double quote.
pub fn quote_word(word: &str) -> String {
    quote_word_for(word, QUOTING)
}

fn quote_word_for(word: &str, quoting: Quoting) -> String {
    match quoting {
        Quoting::Posix => {
            let plain = !word.is_empty()
                && word
                    .chars()
                    .all(|c| c.is_alphanumeric() || "/._-+@%:,=~".contains(c));
            if plain {
                word.to_string()
            } else {
                format!("'{}'", word.replace('\'', r"'\''"))
            }
        }
        Quoting::Windows => {
            if word.is_empty() || word.chars().any(char::is_whitespace) {
                format!("\"{word}\"")
            } else {
                word.to_string()
            }
        }
    }
}

/// Shells that know a `-` before their name means a login shell. Another
/// program named on its own is not given one: `tmux` would start `$SHELL`,
/// itself, in each of its windows.
#[cfg(any(unix, test))]
const SHELL_NAMES: &[&str] = &[
    "ash", "bash", "csh", "dash", "elvish", "fish", "ksh", "mksh", "nu", "oksh", "pwsh", "sh",
    "tcsh", "xonsh", "yash", "zsh",
];

#[cfg(any(unix, test))]
fn is_shell_name(program: &str) -> bool {
    Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| SHELL_NAMES.contains(&name))
}

/// The shells offered, in this order. The others `/etc/shells` lists (csh,
/// tcsh, ksh, dash…) are left to the file picker: few use them, and they
/// made the menu long.
#[cfg(unix)]
const COMMON_SHELLS: &[&str] = &["zsh", "bash", "sh", "fish", "nu", "pwsh"];

/// Where a shell is looked for besides `PATH`: the system's folders, and
/// Homebrew's, which a GUI app's `PATH` leaves out and `/etc/shells` lists
/// only when told to.
#[cfg(unix)]
const SHELL_FOLDERS: &[&str] = &["/bin", "/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"];

/// Of `COMMON_SHELLS`, those `usable`: as `/etc/shells` lists them first,
/// then as found in `folders`; one line per program, by `key`.
#[cfg(unix)]
fn common_shells<K: Eq + std::hash::Hash>(
    listed: &[String],
    folders: &[PathBuf],
    usable: impl Fn(&Path) -> bool,
    key: impl Fn(&Path) -> K,
) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut shells = Vec::new();
    for name in COMMON_SHELLS {
        let named = listed
            .iter()
            .map(PathBuf::from)
            .filter(|path| path.file_name() == Some(std::ffi::OsStr::new(name)));
        for path in named.chain(folders.iter().map(|folder| folder.join(name))) {
            if usable(&path) && seen.insert(key(&path)) {
                shells.push(path.to_string_lossy().into_owned());
            }
        }
    }
    shells
}

/// The paths `/etc/shells` lists: one a line, without comments.
#[cfg(any(unix, test))]
fn listed_shells(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.split('#').next().unwrap_or_default().trim())
        .filter(|line| line.starts_with('/'))
        .map(String::from)
        .collect()
}

/// Where `program` is: itself when it is an absolute path, otherwise the
/// first of that name on `PATH`. `None` when it is not there, or not a
/// program; spawning it as written then says why.
#[cfg(unix)]
fn find_program(program: &str) -> Option<PathBuf> {
    if program.contains('/') {
        let path = PathBuf::from(program);
        return (path.is_absolute() && is_executable(&path)).then_some(path);
    }
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// The same program wherever its folder is reached from: the folder with
/// its links resolved, and the name as it is, so `sh` stays apart from the
/// `dash` it may link to.
#[cfg(unix)]
fn program_key(path: &Path) -> (PathBuf, Option<OsString>) {
    let folder = path.parent().unwrap_or(Path::new("/"));
    (
        std::fs::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf()),
        path.file_name().map(OsString::from),
    )
}

#[cfg(windows)]
fn windows_default_shell() -> Option<PathBuf> {
    find_on_path("pwsh.exe")
        .or_else(installed_pwsh)
        .or_else(windows_powershell)
}

/// PowerShell 7 where its installer puts it, for when it is not on `PATH`.
#[cfg(windows)]
fn installed_pwsh() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("ProgramFiles")?).join(r"PowerShell\7\pwsh.exe");
    path.is_file().then_some(path)
}

#[cfg(windows)]
fn windows_powershell() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("SystemRoot")?)
        .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    path.is_file().then_some(path)
}

/// The first `name` on `PATH`. Not `is_file`: PowerShell 7 from the
/// Microsoft Store is an app execution alias, which cannot be followed.
#[cfg(windows)]
fn find_on_path(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(name))
        .find(|candidate| {
            candidate
                .symlink_metadata()
                .is_ok_and(|metadata| !metadata.is_dir())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str, quoting: Quoting) -> Vec<String> {
        split_command(line, quoting).expect("quotes are closed")
    }

    #[test]
    fn a_blank_command_is_the_default_shell() {
        assert_eq!(LocalShell::new("  ").command(), None);
        assert_eq!(LocalShell::new(""), LocalShell::default());
        assert_eq!(
            LocalShell::new(" /bin/zsh -l ").command(),
            Some("/bin/zsh -l")
        );
    }

    #[test]
    fn posix_commands_split_as_a_shell_reads_them() {
        let posix = Quoting::Posix;
        assert_eq!(words("/bin/zsh", posix), ["/bin/zsh"]);
        assert_eq!(
            words("  /bin/bash   --login -i ", posix),
            ["/bin/bash", "--login", "-i"]
        );
        assert_eq!(
            words(r#"'/opt/My Shells/fish' -C "echo \"hi\" \$HOME \n""#, posix),
            ["/opt/My Shells/fish", "-C", r#"echo "hi" $HOME \n"#]
        );
        assert_eq!(words(r"/opt/My\ Shells/nu", posix), ["/opt/My Shells/nu"]);
        assert_eq!(words(r#"sh -c '' """#, posix), ["sh", "-c", "", ""]);
        assert_eq!(words(r"a'b'c\", posix), [r"abc\"]);
        assert!(words("", posix).is_empty());
        assert_eq!(split_command("sh -c 'exit", posix), None);
        assert_eq!(split_command(r#"sh -c "exit\""#, posix), None);
    }

    #[test]
    fn windows_commands_keep_their_backslashes() {
        let windows = Quoting::Windows;
        assert_eq!(
            words(
                r#""C:\Program Files\PowerShell\7\pwsh.exe" -NoLogo"#,
                windows
            ),
            [r"C:\Program Files\PowerShell\7\pwsh.exe", "-NoLogo"]
        );
        assert_eq!(
            words(r"C:\Windows\System32\cmd.exe /k cd \d", windows),
            [r"C:\Windows\System32\cmd.exe", "/k", "cd", r"\d"]
        );
        // A single quote is no quote to Windows.
        assert_eq!(
            words("cmd /c echo 'a b'", windows),
            ["cmd", "/c", "echo", "'a", "b'"]
        );
        assert_eq!(split_command(r#""C:\Program Files"#, windows), None);
    }

    #[test]
    fn a_chosen_file_reads_back_as_one_word() {
        for (quoting, path) in [
            (Quoting::Posix, "/bin/zsh"),
            (
                Quoting::Posix,
                "/Applications/My Shell.app/Contents/MacOS/sh",
            ),
            (Quoting::Posix, "/opt/it's/fish"),
            (Quoting::Posix, "/opt/$HOME/\"x\"\\y"),
            (Quoting::Windows, r"C:\Windows\System32\cmd.exe"),
            (Quoting::Windows, r"C:\Program Files\PowerShell\7\pwsh.exe"),
        ] {
            let quoted = quote_word_for(path, quoting);
            assert_eq!(words(&quoted, quoting), [path], "{quoted}");
        }
        assert_eq!(quote_word_for("/bin/zsh", Quoting::Posix), "/bin/zsh");
        assert_eq!(
            quote_word_for(r"C:\Program Files\Git\bin\bash.exe", Quoting::Windows),
            r#""C:\Program Files\Git\bin\bash.exe""#
        );
    }

    #[test]
    fn only_shells_named_on_their_own_start_as_login_shells() {
        assert!(is_shell_name("zsh"));
        assert!(is_shell_name("/opt/homebrew/bin/fish"));
        assert!(!is_shell_name("tmux"));
        assert!(!is_shell_name("/usr/bin/zsh5"));
    }

    #[test]
    fn etc_shells_lists_paths_without_its_comments() {
        let text = "# List of acceptable shells for chpass(1).\n\n/bin/bash\n  /bin/zsh # default\n/usr/bin/tmux\nnot-a-path\n";
        assert_eq!(
            listed_shells(text),
            ["/bin/bash", "/bin/zsh", "/usr/bin/tmux"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_shell_on_its_own_is_a_login_shell_and_anything_else_runs_as_written() {
        let builder = LocalShell::new("sh").command_builder().unwrap();
        assert!(builder.is_default_prog());
        let shell = builder.get_env("SHELL").unwrap().to_str().unwrap();
        assert!(shell.starts_with('/') && shell.ends_with("/sh"), "{shell}");

        let builder = LocalShell::new("/bin/sh -l").command_builder().unwrap();
        assert!(!builder.is_default_prog());
        assert_eq!(builder.get_argv(), &["/bin/sh", "-l"]);

        // Not a shell, or not there: spawning it as written says why.
        for command in ["/usr/bin/env", "/no/such/zsh"] {
            let builder = LocalShell::new(command).command_builder().unwrap();
            assert!(!builder.is_default_prog());
            assert_eq!(builder.get_argv(), &[command]);
        }

        assert!(
            LocalShell::default()
                .command_builder()
                .unwrap()
                .is_default_prog()
        );
        let error = LocalShell::new("sh -c 'exit")
            .command_builder()
            .unwrap_err()
            .to_string();
        assert!(error.contains("sh -c 'exit"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn the_installed_shells_are_common_programs_here_and_include_sh() {
        let shells = installed_shells();
        assert!(shells.iter().all(|shell| is_executable(Path::new(shell))));
        assert!(
            shells.iter().all(|shell| COMMON_SHELLS
                .iter()
                .any(|name| shell.ends_with(&format!("/{name}")))),
            "{shells:?}"
        );
        assert!(
            shells.iter().any(|shell| shell.ends_with("/sh")),
            "{shells:?}"
        );
        assert!(default_shell_name().starts_with('/'));
    }

    #[cfg(unix)]
    #[test]
    fn only_common_shells_are_offered_each_program_once() {
        let listed = [
            "/bin/bash",
            "/bin/csh",
            "/bin/dash",
            "/bin/zsh",
            "/usr/bin/bash",
            "/bin/sh",
        ]
        .map(String::from);
        let folders = ["/usr/bin", "/opt/homebrew/bin"].map(PathBuf::from);
        let here = [
            "/bin/bash",
            "/bin/csh",
            "/bin/dash",
            "/bin/zsh",
            "/usr/bin/bash",
            "/usr/bin/zsh",
            "/bin/sh",
            "/opt/homebrew/bin/bash",
            "/opt/homebrew/bin/fish",
        ];
        let usable = |path: &Path| here.iter().any(|here| Path::new(here) == path);
        // `/bin` is `/usr/bin` here, as on a merged `/usr`.
        let key = |path: &Path| path.to_string_lossy().replace("/usr/bin/", "/bin/");
        assert_eq!(
            common_shells(&listed, &folders, usable, key),
            [
                "/bin/zsh",
                "/bin/bash",
                "/opt/homebrew/bin/bash",
                "/bin/sh",
                "/opt/homebrew/bin/fish",
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_starts_powershell_before_the_command_prompt() {
        let shells = installed_shells();
        assert_eq!(shells.last().map(String::as_str), Some("cmd.exe"));
        assert!(
            shells.iter().any(|shell| shell == "powershell.exe"),
            "{shells:?}"
        );
        let default = default_shell_name();
        assert!(
            matches!(default.as_str(), "pwsh.exe" | "powershell.exe"),
            "{default}"
        );
        let builder = LocalShell::default().command_builder().unwrap();
        assert!(!builder.is_default_prog());
        let program = PathBuf::from(&builder.get_argv()[0]);
        assert_eq!(
            program.file_name().unwrap().to_str(),
            Some(default.as_str())
        );
    }
}
