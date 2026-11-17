//! OpenSSH's client configuration, read the way `ssh -G <name>` resolves
//! it, for the settings an imported host keeps: the address, port, user and
//! first identity file. Everything else in the file is skipped.
//!
//! - Every name on a `Host` line without a wildcard or a `!` is a host.
//! - A host's settings come from every block that applies to its name, in
//!   file order, the first value of each setting winning.
//! - `Match` blocks are not evaluated: they apply to no host, except
//!   `Match all`, which is how a file says "for every host from here on".
//! - `Include` reads other files in place, as OpenSSH does.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::{DEFAULT_PORT, ImportCandidate, has_wildcard, parse_port, read_capped, wildcard_match};

/// How deep includes may nest, as in OpenSSH: deeper ones are skipped,
/// which also ends a file that includes itself.
const MAX_INCLUDE_DEPTH: usize = 16;

/// A parsed configuration: its lines that matter, includes read in place.
#[derive(Debug, Default)]
pub struct SshConfig {
    items: Vec<Item>,
}

#[derive(Debug)]
enum Item {
    /// A `Host` line: a block for names matching these patterns.
    Host(Vec<String>),
    /// A `Match` line: a block that applies only when it is `Match all`.
    Match {
        all: bool,
    },
    Setting(Setting, String),
    /// The lines of the files an `Include` names, in order.
    Include(Vec<Item>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Setting {
    HostName,
    Port,
    User,
    IdentityFile,
    ProxyCommand,
}

impl Setting {
    fn parse(keyword: &str) -> Option<Self> {
        match keyword.to_ascii_lowercase().as_str() {
            "hostname" => Some(Setting::HostName),
            "port" => Some(Setting::Port),
            "user" => Some(Setting::User),
            "identityfile" => Some(Setting::IdentityFile),
            "proxycommand" => Some(Setting::ProxyCommand),
            _ => None,
        }
    }
}

/// The settings that apply to one name.
#[derive(Debug, Default)]
struct Resolved {
    host_name: Option<String>,
    port: Option<u16>,
    user: Option<String>,
    identity: Option<String>,
    /// A `ProxyCommand`, with its `%h`/`%p`/`%r` tokens still to be
    /// expanded for the host when it connects.
    proxy_command: Option<String>,
}

impl SshConfig {
    /// Parse a configuration, leaving its includes out.
    #[cfg(test)]
    pub fn parse(text: &str) -> Self {
        Self {
            items: parse_items(text, None),
        }
    }

    /// Parse a configuration and the files it includes: relative paths are
    /// under `ssh_dir`, `~` is `home`.
    pub fn parse_with_includes(text: &str, ssh_dir: &Path, home: &Path) -> Self {
        let includes = Includes {
            ssh_dir,
            home,
            depth: 0,
        };
        Self {
            items: parse_items(text, Some(&includes)),
        }
    }

    /// The hosts the configuration names, in file order, each with its
    /// settings resolved. `local_user` is who logs in when nothing says.
    /// Two names for the same user at the same place give one host.
    pub fn hosts(&self, home: &Path, local_user: &str) -> Vec<ImportCandidate> {
        let mut names = Vec::new();
        collect_names(&self.items, &mut names);
        let mut seen_names = HashSet::new();
        let mut seen_places = HashSet::new();
        let mut hosts = Vec::new();
        for name in names {
            if !seen_names.insert(name.to_lowercase()) {
                continue;
            }
            let resolved = self.resolve(&name);
            let address = match &resolved.host_name {
                Some(host_name) => expand_host_name(host_name, &name),
                None => name.clone(),
            };
            let port = resolved.port.unwrap_or(DEFAULT_PORT);
            let user = resolved.user.unwrap_or_else(|| local_user.to_string());
            if address.is_empty()
                || !seen_places.insert((address.to_lowercase(), port, user.clone()))
            {
                continue;
            }
            let identity = resolved
                .identity
                .and_then(|identity| identity_path(&identity, home, local_user, &address, &user));
            hosts.push(ImportCandidate {
                name,
                address,
                port,
                user,
                identity,
                proxy_command: resolved.proxy_command.clone(),
            });
        }
        hosts
    }

    /// The settings that apply to `name`, as `ssh -G name` would give them.
    fn resolve(&self, name: &str) -> Resolved {
        let mut resolved = Resolved::default();
        // Lines before the first `Host` apply to every host.
        let mut active = true;
        resolve_items(
            &self.items,
            &name.to_lowercase(),
            &mut active,
            false,
            &mut resolved,
        );
        resolved
    }
}

/// Where includes resolve, and how deep they already are.
struct Includes<'a> {
    ssh_dir: &'a Path,
    home: &'a Path,
    depth: usize,
}

fn parse_items(text: &str, includes: Option<&Includes>) -> Vec<Item> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut items = Vec::new();
    for line in text.lines() {
        let Some((keyword, args)) = split_line(line) else {
            continue;
        };
        match keyword.to_ascii_lowercase().as_str() {
            "host" => items.push(Item::Host(args)),
            "match" => items.push(Item::Match {
                all: args.len() == 1 && args[0].eq_ignore_ascii_case("all"),
            }),
            "include" => {
                if let Some(includes) = includes {
                    items.push(Item::Include(include_items(&args, includes)));
                }
            }
            _ => {
                if let Some(setting) = Setting::parse(&keyword) {
                    // A ProxyCommand is the whole rest of the line; every
                    // other setting takes its first argument.
                    let value = if setting == Setting::ProxyCommand {
                        args.join(" ")
                    } else if let Some(value) = args.first() {
                        value.clone()
                    } else {
                        continue;
                    };
                    if !value.is_empty() {
                        items.push(Item::Setting(setting, value));
                    }
                }
            }
        }
    }
    items
}

/// The lines of the files an `Include` line names, in the order glob(3)
/// would list them.
fn include_items(patterns: &[String], includes: &Includes) -> Vec<Item> {
    if includes.depth >= MAX_INCLUDE_DEPTH {
        return Vec::new();
    }
    let nested = Includes {
        ssh_dir: includes.ssh_dir,
        home: includes.home,
        depth: includes.depth + 1,
    };
    let mut items = Vec::new();
    for pattern in patterns {
        let pattern = match expand_tilde(pattern, includes.home) {
            Some(path) => path,
            None => includes.ssh_dir.join(pattern),
        };
        for path in expand_glob(&pattern) {
            if let Ok(text) = read_capped(&path) {
                items.extend(parse_items(&text, Some(&nested)));
            }
        }
    }
    items
}

/// The files a path with `*` and `?` in its components names, sorted per
/// component. Wildcards do not match a leading `.`, as in glob(3).
fn expand_glob(pattern: &Path) -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::new()];
    for component in pattern.components() {
        let part = component.as_os_str();
        let wanted = part.to_string_lossy();
        if !has_wildcard(&wanted) {
            for path in &mut paths {
                path.push(part);
            }
            continue;
        }
        let mut next = Vec::new();
        for dir in &paths {
            let Ok(entries) = fs::read_dir(dir) else {
                continue;
            };
            let mut names: Vec<_> = entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name())
                .filter(|name| {
                    let name = name.to_string_lossy();
                    (!name.starts_with('.') || wanted.starts_with('.'))
                        && wildcard_match(&wanted, &name)
                })
                .collect();
            names.sort();
            next.extend(names.into_iter().map(|name| dir.join(name)));
        }
        paths = next;
    }
    paths.retain(|path| path.is_file());
    paths
}

/// A line's keyword and arguments, or `None` for a blank line or a
/// comment. The keyword may be followed by `=`; arguments may be quoted;
/// an argument starting with `#` ends the line.
fn split_line(line: &str) -> Option<(String, Vec<String>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let end = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let keyword = line[..end].to_string();
    let rest = line[end..].trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest);
    Some((keyword, split_args(rest)))
}

fn split_args(text: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut chars = text.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let Some(&first) = chars.peek() else {
            break;
        };
        if first == '#' {
            break;
        }
        let mut arg = String::new();
        if first == '"' || first == '\'' {
            chars.next();
            for c in chars.by_ref() {
                if c == first {
                    break;
                }
                arg.push(c);
            }
        } else {
            while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
                if c == '\\'
                    && let Some(escaped) = chars.next()
                {
                    arg.push(escaped);
                } else {
                    arg.push(c);
                }
            }
        }
        args.push(arg);
    }
    args
}

/// Every name a `Host` line gives that is a host rather than a pattern.
fn collect_names(items: &[Item], names: &mut Vec<String>) {
    for item in items {
        match item {
            Item::Host(patterns) => names.extend(
                patterns
                    .iter()
                    .filter(|pattern| !pattern.starts_with('!') && !has_wildcard(pattern))
                    .cloned(),
            ),
            Item::Include(items) => collect_names(items, names),
            Item::Match { .. } | Item::Setting(..) => {}
        }
    }
}

/// Walk the lines for `name` (lowercased). `never` is set inside a file
/// included from a block that did not apply: none of its blocks applies
/// either. The block state after an include is what it was before it.
fn resolve_items(
    items: &[Item],
    name: &str,
    active: &mut bool,
    never: bool,
    resolved: &mut Resolved,
) {
    for item in items {
        match item {
            Item::Host(patterns) => *active = !never && host_matches(patterns, name),
            Item::Match { all } => *active = !never && *all,
            Item::Setting(setting, value) if *active => resolved.take(*setting, value),
            Item::Setting(..) => {}
            Item::Include(items) => {
                let outer = *active;
                resolve_items(items, name, active, never || !outer, resolved);
                *active = outer;
            }
        }
    }
}

/// Whether a `Host` line applies to `name`: one of its patterns matches
/// and none of its negated ones does. Case does not matter.
fn host_matches(patterns: &[String], name: &str) -> bool {
    let mut matched = false;
    for pattern in patterns {
        let pattern = pattern.to_lowercase();
        match pattern.strip_prefix('!') {
            Some(negated) if wildcard_match(negated, name) => return false,
            Some(_) => {}
            None => matched |= wildcard_match(&pattern, name),
        }
    }
    matched
}

impl Resolved {
    /// Keep a setting's value unless an earlier line already gave one.
    fn take(&mut self, setting: Setting, value: &str) {
        match setting {
            Setting::HostName => {
                self.host_name.get_or_insert_with(|| value.to_string());
            }
            Setting::Port => {
                // A port that is no port is skipped rather than taking the
                // place of a later, valid one.
                if self.port.is_none() {
                    self.port = parse_port(value);
                }
            }
            Setting::User => {
                self.user.get_or_insert_with(|| value.to_string());
            }
            Setting::IdentityFile => {
                self.identity.get_or_insert_with(|| value.to_string());
            }
            Setting::ProxyCommand => {
                // `none` means no command; the first real one wins.
                if !value.eq_ignore_ascii_case("none") {
                    self.proxy_command.get_or_insert_with(|| value.to_string());
                }
            }
        }
    }
}

/// `HostName` with its tokens filled in: `%h` is the name the host was
/// asked for, `%%` a percent sign.
fn expand_host_name(host_name: &str, name: &str) -> String {
    expand_tokens(host_name, |token| match token {
        'h' => Some(name.to_string()),
        _ => None,
    })
}

/// The key file an `IdentityFile` value names: `~` and `%d` are the home
/// directory, `%u` the local user, `%h` the address, `%r` the remote user.
/// A relative path is under the home directory. `none` names no file.
fn identity_path(
    value: &str,
    home: &Path,
    local_user: &str,
    address: &str,
    user: &str,
) -> Option<PathBuf> {
    if value.eq_ignore_ascii_case("none") {
        return None;
    }
    let home_text = home.to_string_lossy();
    let expanded = expand_tokens(value, |token| match token {
        'd' => Some(home_text.to_string()),
        'u' => Some(local_user.to_string()),
        'h' => Some(address.to_string()),
        'r' => Some(user.to_string()),
        _ => None,
    });
    let path = expand_tilde(&expanded, home).unwrap_or_else(|| PathBuf::from(&expanded));
    Some(if path.is_absolute() {
        path
    } else {
        home.join(path)
    })
}

/// `~` or `~/rest` under `home`, built component by component so the
/// separators are this system's. `None` when the path does not start so.
fn expand_tilde(path: &str, home: &Path) -> Option<PathBuf> {
    let rest = if path == "~" {
        ""
    } else {
        path.strip_prefix("~/")?
    };
    Some(
        rest.split('/')
            .filter(|part| !part.is_empty())
            .fold(home.to_path_buf(), |path, part| path.join(part)),
    )
}

/// Replace `%x` tokens with what `value_of` gives; `%%` is `%`, and a
/// token it does not know is left as written.
fn expand_tokens(text: &str, value_of: impl Fn(char) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some(token) => match value_of(token) {
                Some(value) => out.push_str(&value),
                None => {
                    out.push('%');
                    out.push(token);
                }
            },
            None => out.push('%'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/home/me";

    fn hosts(text: &str) -> Vec<ImportCandidate> {
        SshConfig::parse(text).hosts(Path::new(HOME), "me")
    }

    fn summary(text: &str) -> Vec<String> {
        hosts(text)
            .into_iter()
            .map(|host| format!("{} {}@{}:{}", host.name, host.user, host.address, host.port))
            .collect()
    }

    #[test]
    fn the_first_value_of_each_setting_wins() {
        let text = "\
Host web
  HostName 10.0.0.1
  User deploy
Host *
  User admin
  Port 2222
Host db
  HostName 10.0.0.2
";
        assert_eq!(
            summary(text),
            ["web deploy@10.0.0.1:2222", "db admin@10.0.0.2:2222"]
        );
        // A catch-all written first takes precedence over the host's own.
        let text = "Host *\n  User admin\nHost web\n  User deploy\n";
        assert_eq!(summary(text), ["web admin@web:22"]);
    }

    #[test]
    fn patterns_apply_settings_but_are_no_hosts_themselves() {
        let text = "\
Host *.prod !bastion.prod
  User ops
Host app.prod bastion.prod *.dev !x
  Port 2200
Host 10.0.0.? db-*
  User nobody
";
        assert_eq!(
            summary(text),
            [
                "app.prod ops@app.prod:2200",
                "bastion.prod me@bastion.prod:2200"
            ]
        );
    }

    #[test]
    fn a_host_name_fills_in_its_tokens() {
        let text = "Host web\n  HostName %h.example.com\nHost odd\n  HostName 100%%\n";
        assert_eq!(
            summary(text),
            ["web me@web.example.com:22", "odd me@100%:22"]
        );
    }

    #[test]
    fn lines_are_read_as_openssh_reads_them() {
        let text = "\u{feff}# comment\r\n\
host=web\r\n\
  HOSTNAME = \"10.0.0.1\"   # trailing comment\r\n\
  user\tdeploy\r\n\
  Port nope\r\n\
  Port 2022\r\n\
\r\n\
Host 'quoted name'\r\n";
        assert_eq!(
            summary(text),
            ["web deploy@10.0.0.1:2022", "quoted name me@quoted name:22"]
        );
    }

    #[test]
    fn a_match_block_applies_to_no_host_unless_it_matches_all() {
        let text = "\
Host web
  HostName 10.0.0.1
Match host web exec \"true\"
  User leaked
Host db
Match all
  Port 2222
";
        assert_eq!(summary(text), ["web me@10.0.0.1:2222", "db me@db:2222"]);
    }

    #[test]
    fn two_names_for_the_same_place_give_one_host() {
        let text = "\
Host web web.example.com
  HostName web.example.com
Host WEB
Host web-root
  HostName web.example.com
  User root
";
        assert_eq!(
            summary(text),
            [
                "web me@web.example.com:22",
                "web-root root@web.example.com:22"
            ]
        );
    }

    #[test]
    fn a_proxied_host_is_still_a_host() {
        let text = "Host app\n  HostName 10.0.0.5\n  ProxyJump bastion\nHost bastion\n";
        assert_eq!(
            summary(text),
            ["app me@10.0.0.5:22", "bastion me@bastion:22"]
        );
    }

    #[test]
    fn a_proxy_command_is_imported_with_its_tokens_intact() {
        let text =
            "Host app\n  HostName 10.0.0.5\n  ProxyCommand cloudflared access ssh --hostname %h\n";
        let parsed = hosts(text);
        assert_eq!(
            parsed[0].proxy_command,
            Some("cloudflared access ssh --hostname %h".to_string())
        );
        // `none` means no command, not a command that is the word `none`.
        let none = hosts("Host plain\n  ProxyCommand none\n");
        assert_eq!(none[0].proxy_command, None);
    }

    #[test]
    fn the_first_identity_file_is_kept_with_its_tokens_filled_in() {
        let text = "\
Host web
  HostName 10.0.0.1
  User deploy
  IdentityFile ~/.ssh/%r_%h
  IdentityFile ~/.ssh/second
Host abs
  IdentityFile %d/keys/abs
Host rel
  IdentityFile keys/rel
Host none
  IdentityFile none
Host odd
  IdentityFile ~/.ssh/%C
";
        let found: Vec<Option<PathBuf>> =
            hosts(text).into_iter().map(|host| host.identity).collect();
        let home = Path::new(HOME);
        assert_eq!(
            found,
            [
                Some(home.join(".ssh").join("deploy_10.0.0.1")),
                Some(PathBuf::from(format!("{HOME}/keys/abs"))),
                Some(home.join("keys/rel")),
                None,
                Some(home.join(".ssh").join("%C")),
            ]
        );
    }

    #[test]
    fn includes_are_read_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let ssh = home.join(".ssh");
        let conf = ssh.join("conf.d");
        fs::create_dir_all(&conf).unwrap();
        fs::write(conf.join("b.conf"), "Host b\n  User bee\n").unwrap();
        fs::write(conf.join("a.conf"), "Host a\n  User ay\n").unwrap();
        fs::write(conf.join(".hidden.conf"), "Host hidden\n").unwrap();
        fs::write(ssh.join("work"), "User worker\nHost w1\n").unwrap();
        fs::write(home.join("other"), "Host other\n").unwrap();
        fs::write(ssh.join("loop"), "Host looped\nInclude loop\n").unwrap();
        let text = "\
Include conf.d/*.conf ~/other missing
Host w*
  Include work
Host after
Include loop
";
        let found: Vec<String> = SshConfig::parse_with_includes(text, &ssh, home)
            .hosts(home, "me")
            .into_iter()
            .map(|host| format!("{} {}", host.name, host.user))
            .collect();
        // `work` is included from a `Host w*` block, so its first line only
        // applies to names starting with w, and the block state is back to
        // `Host w*`'s after it: `after` gets nothing from it.
        assert_eq!(
            found,
            [
                "a ay",
                "b bee",
                "other me",
                "w1 worker",
                "after me",
                "looped me"
            ]
        );
    }

    #[test]
    fn blocks_in_a_file_included_from_a_block_that_does_not_apply_never_apply() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = dir.path().join(".ssh");
        fs::create_dir_all(&ssh).unwrap();
        fs::write(ssh.join("extra"), "Host *\n  User wrong\n").unwrap();
        let text = "Host web\nMatch exec \"test -f x\"\n  Include extra\n";
        let found = SshConfig::parse_with_includes(text, &ssh, dir.path()).hosts(dir.path(), "me");
        assert_eq!(found[0].user, "me");
    }
}
