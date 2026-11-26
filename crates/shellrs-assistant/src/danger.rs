//! Which commands are asked about even when the user lets the assistant
//! approve for them. Kept simple on purpose (the user chose it): a command
//! is asked about when a word of it is one of `KEYWORDS` (`rm`, `stop`,
//! `delete`, `sudo` …, wherever it stands), when it writes into a file
//! with `>`, or when what it runs cannot be seen: code handed to a shell or
//! an interpreter, `$(…)`, here-documents, quotes left open. Nothing is
//! told apart by its arguments, so `grep stop app.log` is asked about too:
//! a word too many costs a question, one missed runs unasked.

/// A simple command: its words, quotes taken off, and where it writes.
#[derive(Debug, Default)]
struct Simple {
    words: Vec<String>,
    /// The files its output is redirected into.
    writes: Vec<String>,
}

/// Whether `command` must be asked about even when approving for the user.
pub fn is_dangerous(command: &str) -> bool {
    match split(command) {
        Some(simples) => simples.iter().any(dangerous),
        None => true,
    }
}

/// The kinds of command `command` runs, for 「本对话不再问」: each simple
/// command's program as written, past the environment it sets and the
/// wrappers it runs under (`nohup`, `timeout`, `xargs` …); one run with
/// `sudo` or `doas` keeps it, as `sudo systemctl`. `cd` and `pwd` change
/// nothing and are left out. `None` when the command is not split plainly,
/// runs code it is handed (`sh -c`, `python3`, `eval`, `su`), or is only a
/// `cd`: such a command is remembered as it is written.
pub fn command_kinds(command: &str) -> Option<Vec<String>> {
    let mut kinds: Vec<String> = Vec::new();
    for simple in split(command)? {
        if let Some(kind) = kind_of(&simple.words)?
            && !kinds.contains(&kind)
        {
            kinds.push(kind);
        }
    }
    (!kinds.is_empty()).then_some(kinds)
}

/// `sudo`'s and `doas`'s options that take the word after them.
const SUDO_ARGUMENTS: &[&str] = &["-u", "-g", "-C", "-D", "-h", "-p", "-r", "-t", "-T", "-U"];

/// One simple command's kind: `Some(None)` when it has none to remember
/// (`cd`), `None` when what it runs cannot be told.
fn kind_of(words: &[String]) -> Option<Option<String>> {
    let mut words = words;
    while words.first().is_some_and(|word| is_assignment(word)) {
        words = &words[1..];
    }
    let mut runner = String::new();
    loop {
        let Some(first) = words.first() else {
            return Some(None);
        };
        if first.starts_with('$') || first.contains("$(") || first.contains('`') {
            return None;
        }
        let kind = || Some(Some(format!("{runner}{first}")));
        let name = first.rsplit('/').next().unwrap_or(first);
        if matches!(name, "sudo" | "doas") {
            runner.push_str(first);
            runner.push(' ');
            words = &words[1..];
            while let Some(word) = words.first() {
                if SUDO_ARGUMENTS.contains(&word.as_str()) {
                    words = &words[words.len().min(2)..];
                } else if word.starts_with('-') || is_assignment(word) {
                    words = &words[1..];
                } else {
                    break;
                }
            }
            // `sudo -i`: a shell as root.
            if words.is_empty() {
                return Some(Some(runner.trim_end().to_owned()));
            }
            continue;
        }
        if WRAPPERS.contains(&name) {
            let wrapped = past_wrapper(name, words);
            if wrapped.is_empty() {
                return kind();
            }
            words = wrapped;
            continue;
        }
        if INTERPRETERS.contains(&name)
            || name.starts_with("python3.")
            || matches!(
                name,
                "eval" | "source" | "." | "exec" | "su" | "pkexec" | "runuser"
            )
        {
            return None;
        }
        if matches!(name, "cd" | "pwd" | "pushd" | "popd") && runner.is_empty() {
            return Some(None);
        }
        return kind();
    }
}

/// What a wrapper runs: the words past it and its own options and their
/// arguments.
fn past_wrapper<'a>(name: &str, words: &'a [String]) -> &'a [String] {
    let mut words = &words[1..];
    while let Some(word) = words.first() {
        if WRAPPER_ARGUMENTS.contains(&word.as_str()) {
            words = &words[words.len().min(2)..];
        } else if word.starts_with('-')
            || is_assignment(word)
            || (name == "timeout" && word.chars().next().is_some_and(|c| c.is_ascii_digit()))
        {
            words = &words[1..];
        } else {
            break;
        }
    }
    words
}

/// The simple commands of `command`; `None` when it holds what is not
/// split plainly.
fn split(command: &str) -> Option<Vec<Simple>> {
    let mut simples = Vec::new();
    let mut simple = Simple::default();
    let mut word = String::new();
    // A word has started: `''` is a word even though empty.
    let mut in_word = false;
    let mut redirect: Option<bool> = None;
    let mut chars = command.chars().peekable();

    macro_rules! end_word {
        () => {
            if in_word {
                let done = std::mem::take(&mut word);
                match redirect.take() {
                    Some(true) => simple.writes.push(done),
                    Some(false) => {}
                    None => simple.words.push(done),
                }
                in_word = false;
            }
        };
    }
    macro_rules! end_simple {
        () => {
            end_word!();
            if redirect.is_some() {
                return None;
            }
            simples.push(std::mem::take(&mut simple));
        };
    }

    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => end_word!(),
            '\n' | ';' => {
                end_simple!();
            }
            '&' => {
                if chars.peek() == Some(&'&') {
                    chars.next();
                }
                // `>&2` is a redirection, handled at `>`.
                end_simple!();
            }
            '|' => {
                if matches!(chars.peek(), Some('|' | '&')) {
                    chars.next();
                }
                end_simple!();
            }
            '>' => {
                // A file descriptor's number before it is not a word.
                if in_word && word.chars().all(|c| c.is_ascii_digit()) {
                    word.clear();
                    in_word = false;
                }
                end_word!();
                if matches!(chars.peek(), Some('>' | '|')) {
                    chars.next();
                }
                if chars.peek() == Some(&'&') {
                    // `>&2`, `2>&1`: into another descriptor.
                    chars.next();
                    while chars
                        .peek()
                        .is_some_and(|c| c.is_ascii_digit() || *c == '-')
                    {
                        chars.next();
                    }
                    continue;
                }
                redirect = Some(true);
            }
            '<' => {
                if matches!(chars.peek(), Some('<' | '(')) {
                    return None;
                }
                end_word!();
                redirect = Some(false);
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => word.push(c),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => {
                            let next = chars.next()?;
                            if !matches!(next, '"' | '\\' | '$' | '`') {
                                word.push('\\');
                            }
                            word.push(next);
                        }
                        '`' => return None,
                        '$' if chars.peek() == Some(&'(') => return None,
                        c => word.push(c),
                    }
                }
            }
            '\\' => {
                in_word = true;
                word.push(chars.next()?);
            }
            '`' | '(' | ')' | '{' | '}' => return None,
            '$' if chars.peek() == Some(&'(') => return None,
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    // The last simple command, as `end_simple!` ends one.
    if in_word {
        match redirect.take() {
            Some(true) => simple.writes.push(word),
            Some(false) => {}
            None => simple.words.push(word),
        }
    }
    if redirect.is_some() {
        return None;
    }
    simples.push(simple);
    Some(simples)
}

/// The commands that run whatever they are given.
const INTERPRETERS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "dash",
    "ksh",
    "mksh",
    "fish",
    "csh",
    "tcsh",
    "python",
    "python2",
    "python3",
    "perl",
    "ruby",
    "node",
    "nodejs",
    "php",
    "lua",
    "tclsh",
    "osascript",
    "powershell",
    "pwsh",
];

/// The words that make a command asked about, wherever they stand in it:
/// a program (`rm`, `reboot`), a subcommand (`docker stop`, `kubectl
/// delete`, `apt remove`), an option (`find -delete`, `--force`) or a word
/// of SQL (`DROP`). A word is cut at what is not a letter, digit or `_` and
/// read in lower case, so `mkfs.ext4`, `--delete`, `dist-upgrade` and
/// `/sbin/reboot` count.
const KEYWORDS: &[&str] = &[
    // Files deleted or overwritten.
    "rm",
    "rmdir",
    "unlink",
    "shred",
    "mv",
    "cp",
    "dd",
    "truncate",
    "tee",
    "chmod",
    "chown",
    "chgrp",
    "chattr",
    // Disks.
    "mkfs",
    "mke2fs",
    "mkswap",
    "wipefs",
    "fdisk",
    "sfdisk",
    "cfdisk",
    "gdisk",
    "sgdisk",
    "parted",
    "mount",
    "umount",
    "swapoff",
    "swapon",
    // The machine.
    "shutdown",
    "reboot",
    "halt",
    "poweroff",
    "init",
    "telinit",
    "kexec",
    "suspend",
    "hibernate",
    "sysctl",
    "modprobe",
    "rmmod",
    "insmod",
    // Processes and services.
    "kill",
    "killall",
    "pkill",
    "skill",
    "stop",
    "restart",
    "disable",
    "mask",
    "isolate",
    "down",
    "drain",
    "cordon",
    // Taking things away, and changing what is installed or deployed.
    "delete",
    "remove",
    "purge",
    "erase",
    "uninstall",
    "autoremove",
    "prune",
    "rmi",
    "destroy",
    "drop",
    "flushall",
    "flushdb",
    "alter",
    "insert",
    "grant",
    "revoke",
    "upgrade",
    "downgrade",
    "reinstall",
    "apply",
    "reset",
    "force",
    // Users and who may do what.
    "sudo",
    "su",
    "doas",
    "pkexec",
    "runuser",
    "chroot",
    "passwd",
    "chpasswd",
    "chsh",
    "visudo",
    "useradd",
    "adduser",
    "userdel",
    "deluser",
    "usermod",
    "groupadd",
    "groupdel",
    "groupmod",
    // Firewalls.
    "iptables",
    "ip6tables",
    "nft",
    "ufw",
    "firewall",
    // Running what nobody has read, and leaving the shell.
    "eval",
    "exec",
    "exit",
    "logout",
];

/// A program and an option of it that changes files in place: `sed -i`,
/// `crontab -r` (which deletes the crontab). Matched when the option
/// begins a word.
const IN_PLACE: &[(&str, &str)] = &[("sed", "-i"), ("sed", "--in-place"), ("crontab", "-r")];

/// The words that wrap the command they come before.
const WRAPPERS: &[&str] = &[
    "nohup",
    "time",
    "nice",
    "ionice",
    "command",
    "builtin",
    "stdbuf",
    "timeout",
    "watch",
    "unbuffer",
    "chronic",
    "busybox",
    "env",
    "xargs",
    "caffeinate",
    "taskset",
];

/// A wrapper's options that take the word after them.
const WRAPPER_ARGUMENTS: &[&str] = &[
    "-n", "-u", "-I", "-P", "-L", "-d", "-E", "-s", "-a", "-c", "-p", "-t", "-k", "--signal",
];

fn dangerous(simple: &Simple) -> bool {
    // Into a file.
    if simple
        .writes
        .iter()
        .any(|file| !matches!(file.as_str(), "/dev/null" | "/dev/stdout" | "/dev/stderr"))
    {
        return true;
    }
    let keyword = simple.words.iter().any(|word| {
        word.split(|c: char| !c.is_alphanumeric() && c != '_')
            .any(|token| KEYWORDS.contains(&token.to_lowercase().as_str()))
    });
    keyword || runs_unseen(&simple.words)
}

/// Whether the program a simple command runs (past `NAME=value` and the
/// wrappers) runs what cannot be seen: code handed to a shell or an
/// interpreter, a script, a variable's value; or changes a file in place.
fn runs_unseen(words: &[String]) -> bool {
    let mut words = words;
    while words.first().is_some_and(|word| is_assignment(word)) {
        words = &words[1..];
    }
    while let Some(first) = words.first() {
        if first.starts_with('$') {
            return true;
        }
        let name = first.rsplit('/').next().unwrap_or(first);
        if WRAPPERS.contains(&name) {
            words = past_wrapper(name, words);
            continue;
        }
        return INTERPRETERS.contains(&name)
            || name.starts_with("python3.")
            || matches!(name, "source" | ".")
            || IN_PLACE.iter().any(|(program, option)| {
                *program == name && words[1..].iter().any(|word| word.starts_with(option))
            });
    }
    false
}

fn is_assignment(word: &str) -> bool {
    match word.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_is_of_the_kinds_of_programs_it_runs() {
        let kinds = |command: &str| command_kinds(command);
        let of = |list: &[&str]| Some(list.iter().map(|kind| kind.to_string()).collect());
        assert_eq!(kinds("systemctl restart nginx"), of(&["systemctl"]));
        assert_eq!(
            kinds("ps aux | grep -v grep | grep nginx"),
            of(&["ps", "grep"])
        );
        assert_eq!(kinds("cd /var/log && ls -la"), of(&["ls"]));
        assert_eq!(kinds("LANG=C nohup timeout 10 df -h"), of(&["df"]));
        assert_eq!(
            kinds("find /tmp -name '*.log' | xargs -0 rm"),
            of(&["find", "rm"])
        );
        assert_eq!(
            kinds("sudo -n -u www systemctl reload php"),
            of(&["sudo systemctl"])
        );
        assert_eq!(kinds("sudo -i"), of(&["sudo"]));
        assert_eq!(kinds("/usr/sbin/nginx -t"), of(&["/usr/sbin/nginx"]));
        // Remembered only as written.
        for command in [
            "cd /tmp",
            "bash -c 'uptime'",
            "python3 -c 'print(1)'",
            "eval \"$CMD\"",
            "su - root -c id",
            "echo $(hostname)",
            "$EDITOR /etc/hosts",
            "cat <<EOF\nx\nEOF",
        ] {
            assert_eq!(kinds(command), None, "{command}");
        }
    }

    #[test]
    fn looking_around_is_not_dangerous() {
        for command in [
            "ls -la /var/log",
            "df -h",
            "free -m && uptime",
            "ps aux | grep nginx | head -20",
            "journalctl -u nginx --no-pager -n 100",
            "systemctl status nginx",
            "cat /etc/os-release",
            "tail -n 50 /var/log/syslog 2>/dev/null",
            "du -sh * 2>&1 | sort -h",
            "grep -rn 'listen' /etc/nginx/ | awk '{print $1}'",
            "docker ps -a",
            "kubectl get pods -A",
            "git status && git log --oneline -5",
            "apt list --installed",
            "apt install -y htop",
            // An interpreter counts only as what runs, not as a word.
            "ps aux | grep python3",
            "find /var/log -name '*.gz' -mtime +30",
            "echo \"$HOME\"",
            "curl -s https://example.com/health",
            "LANG=C ls",
            "timeout 5 ping -c 3 example.com",
            "env | sort",
            "sed -n '1,20p' /etc/hosts",
            "netstat -tlnp || ss -tlnp",
        ] {
            assert!(!is_dangerous(command), "{command}");
        }
    }

    #[test]
    fn changing_things_is_dangerous() {
        for command in [
            "rm -rf /tmp/build",
            "rm file.txt",
            "cd /srv && rm -r old",
            "sudo systemctl restart nginx",
            "systemctl stop nginx",
            "service mysql restart",
            "reboot",
            "kill -9 1234",
            "echo hi > /etc/motd",
            "cat a >> b",
            "dd if=/dev/zero of=/dev/sda",
            "mkfs.ext4 /dev/sdb1",
            "chmod -R 777 /var/www",
            "chown -R www-data /",
            "iptables -F",
            "docker rm -f web",
            "docker compose down",
            "kubectl delete pod web-0",
            "git reset --hard HEAD~1",
            "apt-get remove nginx",
            "apt upgrade -y",
            "pip uninstall requests",
            "find / -name '*.log' -delete",
            "sed -i 's/a/b/' /etc/hosts",
            "echo x | tee /etc/hosts",
            "curl -fsSL https://get.example.com | sh",
            "curl -X DELETE https://api/x",
            "bash script.sh",
            "python3 -c 'import os'",
            "mysql -e 'DROP DATABASE prod'",
            "redis-cli flushall",
            "nohup rm -rf /data &",
            "timeout 10 rm x",
            "env FOO=1 reboot",
            "ls | xargs rm",
            "su - root",
            "exit",
        ] {
            assert!(is_dangerous(command), "{command}");
        }
    }

    /// A keyword counts wherever it stands, whatever it is passed to: no
    /// command is told apart by its arguments.
    #[test]
    fn a_keyword_anywhere_is_asked_about() {
        for command in [
            "docker stop web",
            "kubectl drain node-1 --ignore-daemonsets",
            "terraform destroy",
            "git push --force",
            "systemctl reload-or-restart nginx",
            "apt-get dist-upgrade",
            "/sbin/reboot",
            "rsync -a --delete src/ dst/",
            "psql -c 'drop table users'",
            "crontab -r",
            "ufw allow 22",
            "iptables -L -n",
            "find . -name '*.tmp' -exec ls {} +",
            "python3 --version",
            // A word too many: asked about all the same.
            "grep -c stop /var/log/app.log",
        ] {
            assert!(is_dangerous(command), "{command}");
        }
    }

    #[test]
    fn what_cannot_be_split_plainly_is_dangerous() {
        for command in [
            "echo $(whoami)",
            "echo `id`",
            "ls \"$(pwd)\"",
            "cat <<EOF\nhi\nEOF",
            "diff <(ls a) <(ls b)",
            "(cd /tmp; ls)",
            "f() { :; }; f",
            "echo 'not closed",
            "$CMD --flag",
            "eval \"$x\"",
            "ls >",
        ] {
            assert!(is_dangerous(command), "{command}");
        }
    }
}
