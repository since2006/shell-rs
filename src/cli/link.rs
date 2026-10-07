//! The links ShellRS is opened with, the way bastion hosts open Xshell:
//! `ShellRS ssh://user@host:port`, or `ShellRS -url ssh://… -newtab 名称`;
//! and WinSCP: `ShellRS sftp://user@host:port`, or with
//! `/sessionname=名称` in front.
//!
//! Only told apart from the `shellrs` command's arguments here; the link
//! itself is read by the app (`host::SshLink`), which says what is wrong
//! with it in a notification. Opened again, ShellRS hands the link to the
//! one already running, inside [`super::Request::Activate`].

use std::ffi::OsString;
use std::fmt;

use serde::{Deserialize, Serialize};

/// A link to connect to, as the caller wrote it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenLink {
    /// May hold a password. Empty when the arguments named no link, so the
    /// app can say so: a second ShellRS on Windows has no console to.
    pub url: String,
    /// What to call the tab: Xshell's `-newtab`, WinSCP's `/sessionname`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab: Option<String>,
}

impl fmt::Debug for OpenLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenLink")
            .field("url", &without_password(&self.url))
            .field("tab", &self.tab)
            .finish()
    }
}

/// `url` with its password replaced, for logs. Never the text itself when
/// it cannot be read: where the password ends is unknown then.
fn without_password(url: &str) -> String {
    if url.is_empty() {
        return String::new();
    }
    match url::Url::parse(url) {
        Ok(mut parsed) => {
            if parsed.password().is_some() {
                let _ = parsed.set_password(Some("***"));
            }
            parsed.to_string()
        }
        Err(_) => "<unreadable link>".into(),
    }
}

/// Xshell's options. `-newwin` asks for a window of its own, which means
/// nothing to ShellRS's one window.
const URL_OPTION: &str = "-url";
const TAB_OPTION: &str = "-newtab";
const WINDOW_OPTION: &str = "-newwin";

/// WinSCP's switches, written `/name[=value]` or `-name[=value]`:
/// `/sessionname=名称` names the tab, the others mean nothing to ShellRS
/// (`/newinstance` asks for a window of its own, `/ini=nul` not to read
/// WinSCP's settings, `/privatekey=` and `/hostkey=` are ShellRS's to
/// know).
const WINSCP_SWITCHES: &[&str] = &[
    SESSION_NAME,
    "newinstance",
    "privatekey",
    "hostkey",
    "passphrase",
    "rawsettings",
    "ini",
    "log",
    "timeout",
];
const SESSION_NAME: &str = "sessionname";

/// The link `args` (without the program) open, or `None` when they are the
/// `shellrs` command's. Only the first argument decides, so `shellrs exec
/// web "curl http://x"` stays a command: a link launch starts with one of
/// Xshell's options, one of WinSCP's switches or the link itself.
pub fn link_arguments(args: &[OsString]) -> Option<OpenLink> {
    let args: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let first = args.first()?;
    if !is_option(first) && winscp_switch(first).is_none() && !looks_like_link(first) {
        return None;
    }
    let mut given = None;
    let mut bare = None;
    let mut tab = None;
    let mut args = args.into_iter().peekable();
    while let Some(arg) = args.next() {
        let option = arg.to_lowercase();
        if option == URL_OPTION {
            let url = args.next();
            given = given.or(url);
        } else if option == TAB_OPTION {
            // Its name, unless the next argument is something else.
            if args
                .peek()
                .is_some_and(|next| !next.starts_with('-') && !looks_like_link(next))
            {
                tab = tab_name(&args.next().unwrap_or_default()).or(tab);
            }
        } else if let Some((switch, value)) = winscp_switch(&arg) {
            if switch == SESSION_NAME {
                tab = value.and_then(tab_name).or(tab);
            }
        } else if option == WINDOW_OPTION || arg.starts_with('-') {
            // Ignored, like any option ShellRS does not know.
        } else if bare.is_none() && looks_like_link(&arg) {
            bare = Some(arg);
        }
    }
    Some(OpenLink {
        url: given.or(bare).unwrap_or_default(),
        tab,
    })
}

fn is_option(arg: &str) -> bool {
    [URL_OPTION, TAB_OPTION, WINDOW_OPTION].contains(&arg.to_lowercase().as_str())
}

/// The name and value of one of WinSCP's switches, the name in lower case.
fn winscp_switch(arg: &str) -> Option<(String, Option<&str>)> {
    let switch = arg.strip_prefix(['/', '-'])?;
    let (name, value) = match switch.split_once('=') {
        Some((name, value)) => (name, Some(value)),
        None => (switch, None),
    };
    let name = name.to_lowercase();
    WINSCP_SWITCHES
        .contains(&name.as_str())
        .then_some((name, value))
}

/// A tab's name as given, without the spaces or quotes around it; `None`
/// when that leaves nothing.
fn tab_name(given: &str) -> Option<String> {
    let name = given.trim().trim_matches('"').trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// `scheme://…`, whatever the scheme: the app says which ones it opens.
fn looks_like_link(arg: &str) -> bool {
    let Some((scheme, _)) = arg.split_once("://") else {
        return false;
    };
    let mut chars = scheme.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(args: &[&str]) -> Option<OpenLink> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        link_arguments(&args)
    }

    fn open(url: &str, tab: Option<&str>) -> Option<OpenLink> {
        Some(OpenLink {
            url: url.into(),
            tab: tab.map(str::to_string),
        })
    }

    #[test]
    fn a_link_on_its_own_opens() {
        let url = "ssh://b478e26f-811b-4a90-81c3-74929127898a@172.16.0.28:12024";
        assert_eq!(link(&[url]), open(url, None));
    }

    #[test]
    fn xshells_options_are_understood_in_any_case_and_order() {
        let url = "ssh://deploy:secret@10.0.0.9:2222";
        assert_eq!(
            link(&["-url", url, "-newtab", "生产库"]),
            open(url, Some("生产库"))
        );
        assert_eq!(
            link(&["-NewTab", " 生产库 ", "-newwin", "-URL", url]),
            open(url, Some("生产库"))
        );
        // `-newtab` without a name.
        assert_eq!(link(&["-newtab", url]), open(url, None));
        assert_eq!(link(&["-newwin", url, "-newtab"]), open(url, None));
        // `-url` wins over a bare link, and the first of each counts.
        assert_eq!(
            link(&["ssh://a@b", "-url", url, "-url", "ssh://c@d"]),
            open(url, None)
        );
        // Unknown options are passed over, their values with them unless
        // they look like a link.
        assert_eq!(link(&["-newwin", "-folder", "x", url]), open(url, None));
    }

    #[test]
    fn winscps_switches_are_understood_too() {
        // How JumpServer opens WinSCP: the link alone.
        let url = "sftp://b478e26f:secret@172.16.0.28:12024";
        assert_eq!(link(&[url]), open(url, None));
        assert_eq!(
            link(&["/sessionname=生产库", url]),
            open(url, Some("生产库"))
        );
        assert_eq!(
            link(&[
                "/newinstance",
                "/ini=nul",
                "-SessionName=\"文件 服务器\"",
                url
            ]),
            open(url, Some("文件 服务器"))
        );
        assert_eq!(
            link(&[
                "/rawsettings",
                "Compression=1",
                url,
                "/hostkey=ssh-ed25519 255 x"
            ]),
            open(url, None)
        );
        assert_eq!(link(&["/sessionname=生产库"]), open("", Some("生产库")));
    }

    #[test]
    fn options_without_a_link_still_open_so_the_app_can_say_why() {
        assert_eq!(link(&["-newtab", "生产库"]), open("", Some("生产库")));
        assert_eq!(link(&["-url"]), open("", None));
    }

    #[test]
    fn the_commands_arguments_are_left_to_the_command() {
        for args in [
            &[][..],
            &["list"],
            &["exec", "web", "curl http://example.com"],
            &["upload", "web", "./a", "/tmp/"],
            &["--help"],
            &["-h"],
            &["--version"],
            &["ssh-not-a-link"],
            &["://nothing-before"],
            &["/tmp/sessionname"],
            &["/usr/bin/sftp://x"],
        ] {
            assert_eq!(link(args), None, "{args:?}");
        }
    }

    #[test]
    fn debug_hides_the_password() {
        let shown = format!(
            "{:?}",
            OpenLink {
                url: "ssh://deploy:hunter2@10.0.0.9".into(),
                tab: None,
            }
        );
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("deploy"), "{shown}");
        let unreadable = format!(
            "{:?}",
            OpenLink {
                url: "ssh://deploy:hunter2@[bad".into(),
                tab: None,
            }
        );
        assert!(!unreadable.contains("hunter2"), "{unreadable}");
    }
}
