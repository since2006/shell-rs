//! `~/.ssh/known_hosts`, read for the hosts OpenSSH has connected to. The
//! file has no user names, so every host logs in as `root`. Lines OpenSSH
//! would skip, or that name no one host, are skipped too.

use std::collections::HashSet;

use super::{DEFAULT_PORT, DEFAULT_USER, ImportCandidate, ImportScan, has_wildcard, parse_port};

pub fn parse(text: &str) -> ImportScan {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut scan = ImportScan::default();
    let mut seen = HashSet::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        // `@cert-authority` and `@revoked` lines are about keys, not hosts;
        // a line needs its names, key type and key to be one at all.
        let [names, _key_type, _key, ..] = fields.as_slice() else {
            continue;
        };
        if names.starts_with('@') {
            continue;
        }
        if names.starts_with('|') {
            scan.hashed += 1;
            continue;
        }
        let Some((address, port)) = names.split(',').find_map(place) else {
            continue;
        };
        if seen.insert((address.to_lowercase(), port)) {
            scan.candidates.push(ImportCandidate {
                name: address.clone(),
                address,
                port,
                user: DEFAULT_USER.to_string(),
                identity: None,
                proxy_command: None,
            });
        }
    }
    scan
}

/// The address and port one name stands for: `host`, or `[host]:port` for
/// a port other than 22. Patterns and negations name no one host.
fn place(name: &str) -> Option<(String, u16)> {
    if name.is_empty() || name.starts_with('!') || has_wildcard(name) {
        return None;
    }
    let Some(bracketed) = name.strip_prefix('[') else {
        return Some((name.to_string(), DEFAULT_PORT));
    };
    let (host, rest) = bracketed.split_once(']')?;
    let port = match rest.strip_prefix(':') {
        Some(port) => parse_port(port)?,
        None if rest.is_empty() => DEFAULT_PORT,
        None => return None,
    };
    (!host.is_empty()).then(|| (host.to_string(), port))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn places(text: &str) -> Vec<String> {
        parse(text)
            .candidates
            .into_iter()
            .map(|host| format!("{}@{}:{}", host.user, host.address, host.port))
            .collect()
    }

    #[test]
    fn each_host_is_read_once_whatever_its_keys() {
        let text = "\
# comment
web.example.com,10.0.0.1 ssh-ed25519 AAAA
web.example.com,10.0.0.1 ssh-rsa BBBB
WEB.example.com ecdsa-sha2-nistp256 CCCC
[10.0.0.7]:2222 ssh-ed25519 DDDD
10.0.0.7 ssh-ed25519 EEEE
[fe80::1]:2200 ssh-ed25519 FFFF
fe80::2 ssh-ed25519 GGGG
";
        assert_eq!(
            places(text),
            [
                "root@web.example.com:22",
                "root@10.0.0.7:2222",
                "root@10.0.0.7:22",
                "root@fe80::1:2200",
                "root@fe80::2:22",
            ]
        );
    }

    #[test]
    fn lines_naming_no_one_host_are_skipped_and_hashed_ones_counted() {
        let text = "\u{feff}\
@cert-authority *.example.com ssh-ed25519 AAAA
@revoked web ssh-ed25519 AAAA
|1|c2FsdA==|aGFzaA== ssh-ed25519 AAAA
|1|c2FsdA==|aGFzaB== ssh-rsa AAAA
*.corp ssh-ed25519 AAAA
!bad,db ssh-ed25519 AAAA
short-line ssh-ed25519
[broken:22 ssh-ed25519 AAAA
[x]:0 ssh-ed25519 AAAA
";
        let scan = parse(text);
        assert_eq!(scan.hashed, 2);
        assert_eq!(places(text), ["root@db:22"]);
    }
}
