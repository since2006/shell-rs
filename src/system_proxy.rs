//! The desktop's proxy settings, handed to a shell as its environment
//! variables.
//!
//! On Linux the settings live in GNOME's store under `org.gnome.system.proxy`,
//! and nothing carries them into the environment: a terminal opened from the
//! desktop starts with none of `http_proxy` and the rest, so curl, git and
//! cargo here go around the proxy the user set in 设置 → 网络 → 网络代理.
//! Ptyxis reads the settings and hands them to every shell it opens; this
//! does the same for a local terminal of ShellRS's.
//!
//! Only 手动 mode is handed over. With 自动 the proxy comes from a PAC file,
//! which these variables can't say.

use portable_pty::CommandBuilder;

/// Put the desktop's proxy, if its settings name one, into the environment
/// a shell is about to start in. Only Linux has these settings; elsewhere
/// nothing is added.
#[cfg(target_os = "linux")]
pub fn inject(command: &mut CommandBuilder) {
    for (name, value) in current().map_or_else(Vec::new, |proxy| proxy.proxy_env()) {
        command.env(name, value);
    }
}

/// The same, adding nothing: the desktop's proxy settings are GNOME's on
/// Linux, and macOS and Windows hand theirs to programs another way.
#[cfg(not(target_os = "linux"))]
pub fn inject(_: &mut CommandBuilder) {}

/// The desktop's proxy, in the shape the environment variables take.
#[cfg(target_os = "linux")]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SystemProxy {
    /// `host:port` of the HTTP proxy, the one HTTPS goes through too.
    http: Option<String>,
    /// `host:port` of the HTTPS proxy, on its own when the settings have one.
    https: Option<String>,
    /// `host:port` of the SOCKS proxy.
    socks: Option<String>,
    /// `host:port` of the FTP proxy.
    ftp: Option<String>,
    /// The destinations that go around the proxy, as 设置 writes them.
    ignore_hosts: Vec<String>,
}

#[cfg(target_os = "linux")]
impl SystemProxy {
    /// The variables for this proxy: each of `http_proxy` and `HTTPS_PROXY`
    /// and their like in both cases, as programs read either, and
    /// `no_proxy` for the destinations that stay direct. The SOCKS proxy
    /// goes into `all_proxy` as `socks://`, which curl and wget both take.
    pub fn proxy_env(&self) -> Vec<(String, String)> {
        let mut pairs = Vec::new();
        let mut add = |lower: &str, upper: &str, value: String| {
            pairs.push((lower.to_string(), value.clone()));
            pairs.push((upper.to_string(), value));
        };
        if let Some(http) = &self.http {
            add("http_proxy", "HTTP_PROXY", format!("http://{http}"));
        }
        if let Some(https) = &self.https {
            add("https_proxy", "HTTPS_PROXY", format!("http://{https}"));
        }
        if let Some(socks) = &self.socks {
            add("all_proxy", "ALL_PROXY", format!("socks://{socks}"));
        }
        if let Some(ftp) = &self.ftp {
            add("ftp_proxy", "FTP_PROXY", format!("ftp://{ftp}"));
        }
        if !self.ignore_hosts.is_empty() {
            add("no_proxy", "NO_PROXY", self.ignore_hosts.join(","));
        }
        pairs
    }
}

/// What the desktop's settings say now. `None` without a proxy to hand
/// over: 自动 or 关闭 mode, no `gsettings` here, or none of its answers
/// making sense. Blocking: it runs `gsettings`, so it runs on the
/// transport's thread, never on the UI's.
#[cfg(target_os = "linux")]
fn current() -> Option<SystemProxy> {
    from_gsettings(&|schema, key| setting(schema, key))
}

/// One answer of `gsettings get`, trimmed; none when it fails or isn't
/// there to run.
#[cfg(target_os = "linux")]
fn setting(schema: &str, key: &str) -> Option<String> {
    let output = std::process::Command::new("gsettings")
        .args(["get", schema, key])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The proxy from the answers of `gsettings get`, told apart from the
/// asking itself so the parsing can be tested.
#[cfg(target_os = "linux")]
fn from_gsettings(get: &impl Fn(&str, &str) -> Option<String>) -> Option<SystemProxy> {
    if gvariant_string(&get(SCHEMA, "mode")?)? != "manual" {
        return None;
    }
    let same = get(SCHEMA, "use-same-proxy")
        .and_then(|raw| gvariant_bool(&raw))
        .unwrap_or(false);
    let ignore_hosts = get(SCHEMA, "ignore-hosts")
        .and_then(|raw| gvariant_string_array(&raw))
        .unwrap_or_default();
    let http = endpoint(get, HTTP_SCHEMA);
    // With 同一个代理服务器 for all protocols, GNOME leaves the ones it
    // covers without a value of their own; they go through the HTTP one.
    let https = endpoint(get, HTTPS_SCHEMA).or_else(|| same.then(|| http.clone()).flatten());
    let ftp = endpoint(get, FTP_SCHEMA).or_else(|| same.then(|| http.clone()).flatten());
    let socks = endpoint(get, SOCKS_SCHEMA);
    Some(SystemProxy {
        http,
        https,
        socks,
        ftp,
        ignore_hosts,
    })
}

#[cfg(target_os = "linux")]
const SCHEMA: &str = "org.gnome.system.proxy";
#[cfg(target_os = "linux")]
const HTTP_SCHEMA: &str = "org.gnome.system.proxy.http";
#[cfg(target_os = "linux")]
const HTTPS_SCHEMA: &str = "org.gnome.system.proxy.https";
#[cfg(target_os = "linux")]
const SOCKS_SCHEMA: &str = "org.gnome.system.proxy.socks";
#[cfg(target_os = "linux")]
const FTP_SCHEMA: &str = "org.gnome.system.proxy.ftp";

/// One protocol's proxy, `host:port`: none without a host or a port, since
/// either alone is no proxy to reach.
#[cfg(target_os = "linux")]
fn endpoint(get: &impl Fn(&str, &str) -> Option<String>, schema: &str) -> Option<String> {
    let host = gvariant_string(&get(schema, "host")?)?;
    let port = get(schema, "port").and_then(|raw| raw.trim().parse::<u16>().ok())?;
    host_port(&host, port)
}

/// `host:port`, with an IPv6 host in brackets as URLs take it; none with
/// nothing to put before or after the colon.
#[cfg(target_os = "linux")]
fn host_port(host: &str, port: u16) -> Option<String> {
    let host = host.trim();
    if host.is_empty() || port == 0 {
        return None;
    }
    if host.contains(':') && !host.starts_with('[') {
        return Some(format!("[{host}]:{port}"));
    }
    Some(format!("{host}:{port}"))
}

/// A GVariant string, `'…'`, with `\'` and `\\` inside it.
#[cfg(target_os = "linux")]
fn gvariant_string(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let inner = raw.strip_prefix('\'')?.strip_suffix('\'')?;
    let mut text = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(char) = chars.next() {
        match char {
            '\\' => text.push(chars.next()?),
            char => text.push(char),
        }
    }
    Some(text)
}

/// A GVariant boolean, `true` or `false`.
#[cfg(target_os = "linux")]
fn gvariant_bool(raw: &str) -> Option<bool> {
    match raw.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// A GVariant array of strings: `['one', 'two']`, or `@as []` when it is
/// empty, the type written because the brackets alone say nothing.
#[cfg(target_os = "linux")]
fn gvariant_string_array(raw: &str) -> Option<Vec<String>> {
    let raw = raw.trim();
    let raw = raw.strip_prefix("@as").map(str::trim).unwrap_or(raw);
    let inner = raw.strip_prefix('[')?.strip_suffix(']')?;
    let mut items = Vec::new();
    let mut rest = inner.trim();
    while !rest.is_empty() {
        let end = closing_quote(rest)?;
        items.push(gvariant_string(&rest[..end])?);
        rest = rest[end..].trim_start();
        // The last item has no comma after it.
        if !rest.is_empty() {
            rest = rest.strip_prefix(',')?.trim_start();
        }
    }
    Some(items)
}

/// Where the string starting at the front of `text` ends, one past its
/// closing quote; none without one.
#[cfg(target_os = "linux")]
fn closing_quote(text: &str) -> Option<usize> {
    let mut chars = text.char_indices();
    match chars.next() {
        Some((_, '\'')) => {}
        _ => return None,
    }
    while let Some((index, char)) = chars.next() {
        match char {
            '\\' => {
                chars.next()?;
            }
            '\'' => return Some(index + char.len_utf8()),
            _ => {}
        }
    }
    None
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// The answers `gsettings get` gives on one computer, looked up the
    /// way `current` asks for them.
    fn answers<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str, &str) -> Option<String> {
        move |schema, key| {
            pairs
                .iter()
                .find(|(name, _)| *name == format!("{schema} {key}"))
                .map(|(_, value)| (*value).to_string())
        }
    }

    #[test]
    fn a_manual_proxy_becomes_the_variables_both_cases() {
        let proxy = from_gsettings(&answers(&[
            (&format!("{SCHEMA} mode"), "'manual'"),
            (&format!("{SCHEMA} use-same-proxy"), "false"),
            (
                &format!("{SCHEMA} ignore-hosts"),
                "['localhost', '127.0.0.0/8', '::1']",
            ),
            (&format!("{HTTP_SCHEMA} host"), "'127.0.0.1'"),
            (&format!("{HTTP_SCHEMA} port"), "12334"),
            (&format!("{HTTPS_SCHEMA} host"), "'127.0.0.1'"),
            (&format!("{HTTPS_SCHEMA} port"), "12334"),
            (&format!("{SOCKS_SCHEMA} host"), "'127.0.0.1'"),
            (&format!("{SOCKS_SCHEMA} port"), "12334"),
            (&format!("{FTP_SCHEMA} host"), "'127.0.0.1'"),
            (&format!("{FTP_SCHEMA} port"), "12334"),
        ]))
        .unwrap();
        assert_eq!(
            proxy.proxy_env(),
            [
                ("http_proxy", "http://127.0.0.1:12334"),
                ("HTTP_PROXY", "http://127.0.0.1:12334"),
                ("https_proxy", "http://127.0.0.1:12334"),
                ("HTTPS_PROXY", "http://127.0.0.1:12334"),
                ("all_proxy", "socks://127.0.0.1:12334"),
                ("ALL_PROXY", "socks://127.0.0.1:12334"),
                ("ftp_proxy", "ftp://127.0.0.1:12334"),
                ("FTP_PROXY", "ftp://127.0.0.1:12334"),
                ("no_proxy", "localhost,127.0.0.0/8,::1"),
                ("NO_PROXY", "localhost,127.0.0.0/8,::1"),
            ]
            .map(|(name, value)| (name.to_string(), value.to_string()))
        );
    }

    #[test]
    fn anything_but_manual_is_not_handed_over() {
        for mode in ["'none'", "'auto'", "'nonsense'"] {
            let proxy = from_gsettings(&answers(&[(&format!("{SCHEMA} mode"), mode)]));
            assert_eq!(proxy, None, "{mode} is handed over");
        }
    }

    #[test]
    fn one_protocol_without_a_host_or_port_is_no_proxy() {
        let proxy = from_gsettings(&answers(&[
            (&format!("{SCHEMA} mode"), "'manual'"),
            (&format!("{SCHEMA} use-same-proxy"), "false"),
            (&format!("{HTTP_SCHEMA} host"), "''"),
            (&format!("{HTTP_SCHEMA} port"), "12334"),
            (&format!("{HTTPS_SCHEMA} host"), "'proxy.example.com'"),
            (&format!("{HTTPS_SCHEMA} port"), "0"),
        ]))
        .unwrap();
        assert_eq!(proxy.proxy_env(), []);
    }

    #[test]
    fn one_proxy_for_all_protocols_fills_the_ones_left_empty() {
        let proxy = from_gsettings(&answers(&[
            (&format!("{SCHEMA} mode"), "'manual'"),
            (&format!("{SCHEMA} use-same-proxy"), "true"),
            (&format!("{HTTP_SCHEMA} host"), "'proxy.example.com'"),
            (&format!("{HTTP_SCHEMA} port"), "8080"),
        ]))
        .unwrap();
        let env = proxy.proxy_env();
        assert!(env.contains(&(
            "https_proxy".to_string(),
            "http://proxy.example.com:8080".to_string()
        )));
        // The same server, with the scheme ftp takes it by.
        assert!(env.contains(&(
            "ftp_proxy".to_string(),
            "ftp://proxy.example.com:8080".to_string()
        )));
        // The SOCKS proxy is its own setting, never the HTTP one's.
        assert!(!env.iter().any(|(name, _)| name == "all_proxy"));
    }

    #[test]
    fn an_ipv6_host_takes_brackets() {
        let proxy = from_gsettings(&answers(&[
            (&format!("{SCHEMA} mode"), "'manual'"),
            (&format!("{SCHEMA} use-same-proxy"), "false"),
            (&format!("{HTTP_SCHEMA} host"), "'::1'"),
            (&format!("{HTTP_SCHEMA} port"), "1080"),
        ]))
        .unwrap();
        assert_eq!(
            proxy.proxy_env()[0],
            ("http_proxy".to_string(), "http://[::1]:1080".to_string())
        );
    }

    #[test]
    fn no_ignore_hosts_is_no_no_proxy() {
        let proxy = from_gsettings(&answers(&[
            (&format!("{SCHEMA} mode"), "'manual'"),
            (&format!("{SCHEMA} use-same-proxy"), "false"),
            (&format!("{SCHEMA} ignore-hosts"), "@as []"),
            (&format!("{HTTP_SCHEMA} host"), "'proxy.example.com'"),
            (&format!("{HTTP_SCHEMA} port"), "8080"),
        ]))
        .unwrap();
        assert!(!proxy.proxy_env().iter().any(|(name, _)| name == "no_proxy"));
    }

    #[test]
    fn a_string_keeps_what_an_escape_stands_for() {
        assert_eq!(gvariant_string(r"'quo\'te'"), Some("quo'te".into()));
        assert_eq!(
            gvariant_string(r"'back\\slash'"),
            Some(r"back\slash".into())
        );
        assert_eq!(gvariant_string("'plain'"), Some("plain".into()));
        assert_eq!(gvariant_string("plain"), None);
    }

    #[test]
    fn an_array_of_strings_stops_at_the_commas_outside_quotes() {
        assert_eq!(
            gvariant_string_array("['a', 'b,c', '::1']"),
            Some(vec!["a".to_string(), "b,c".to_string(), "::1".to_string()])
        );
        assert_eq!(gvariant_string_array("@as []"), Some(Vec::new()));
        assert_eq!(gvariant_string_array("[]"), Some(Vec::new()));
        assert_eq!(gvariant_string_array("'no'"), None);
    }

    /// The settings of the computer this runs on, whatever they are: the
    /// reading doesn't fall over, and what it hands over is in shape.
    #[test]
    #[ignore = "读真实的 gsettings：cargo test --lib system_proxy -- --ignored"]
    fn the_real_settings_say_something_in_shape() {
        let Some(proxy) = current() else {
            return;
        };
        for (name, value) in proxy.proxy_env() {
            assert!(!value.is_empty(), "{name} is empty");
        }
    }
}
