//! The folder a shell says it is in: OSC 7 (`file://host/path`, which the
//! setup ShellRS offers writes, and fish sends by itself) and iTerm2's OSC
//! 1337 `CurrentDir=`. `NoticeScanner` finds them in the output on the
//! parser thread; the right sidebar's 文件 follows them.

use percent_encoding::percent_decode_str;

/// A folder the terminal's output reported: where its shell is, if the
/// report came from this host and not from one ssh'd to inside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportedDirectory {
    /// The host the report names; none when it names none (OSC 1337, an
    /// empty host, `localhost`).
    pub host: Option<String>,
    pub path: String,
}

impl ReportedDirectory {
    /// Restore this POSIX folder in a shell, with the path kept as one
    /// literal argument even when it contains quotes or shell syntax.
    pub(crate) fn cd_command(&self) -> Option<String> {
        (self.path.starts_with('/') && !self.path.chars().any(char::is_control))
            .then(|| format!("cd -- '{}'", self.path.replace('\'', r"'\''")))
    }

    /// Whether the report comes from the host called `hostname` (its
    /// `uname -n`). One that names no host is taken at its word; one that
    /// names a host is taken only from a host whose name is known.
    pub fn is_from(&self, hostname: Option<&str>) -> bool {
        match (&self.host, hostname) {
            (None, _) => true,
            (Some(host), Some(name)) => host.eq_ignore_ascii_case(name),
            (Some(_), None) => false,
        }
    }
}

/// The folder an OSC sequence reports, by its parameters as the parser
/// splits them on `;`, if it is a well-formed report.
pub(super) fn reported_directory(params: &[&[u8]]) -> Option<ReportedDirectory> {
    // A path may hold `;`, which the parser split on.
    let rest = || {
        params
            .get(1..)
            .unwrap_or_default()
            .iter()
            .map(|param| std::str::from_utf8(param).ok())
            .collect::<Option<Vec<_>>>()
            .map(|parts| parts.join(";"))
    };
    let directory = match *params.first()? {
        b"7" => {
            let url = rest()?;
            let scheme = url.get(..7)?;
            if !scheme.eq_ignore_ascii_case("file://") {
                return None;
            }
            let url = &url[7..];
            let slash = url.find('/')?;
            let (host, path) = url.split_at(slash);
            let path = percent_decode_str(path).decode_utf8().ok()?.into_owned();
            let host = (!host.is_empty() && !host.eq_ignore_ascii_case("localhost"))
                .then(|| host.to_string());
            ReportedDirectory { host, path }
        }
        b"1337" => ReportedDirectory {
            host: None,
            path: rest()?.strip_prefix("CurrentDir=")?.to_string(),
        },
        _ => return None,
    };
    (directory.path.starts_with('/') && !directory.path.chars().any(char::is_control))
        .then_some(directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(sequence: &str) -> Option<ReportedDirectory> {
        let params: Vec<&[u8]> = sequence.split(';').map(str::as_bytes).collect();
        reported_directory(&params)
    }

    fn at(host: Option<&str>, path: &str) -> Option<ReportedDirectory> {
        Some(ReportedDirectory {
            host: host.map(str::to_string),
            path: path.to_string(),
        })
    }

    #[test]
    fn osc_7_names_the_host_and_an_encoded_path() {
        assert_eq!(
            parse("7;file://web-01/srv/a%20b/%E6%96%87%E4%BB%B6"),
            at(Some("web-01"), "/srv/a b/文件")
        );
        // A `;` in the path, split off by the parser, is put back.
        assert_eq!(
            parse("7;file://web-01/srv/a;b"),
            at(Some("web-01"), "/srv/a;b")
        );
        // Raw UTF-8 as some shells send it.
        assert_eq!(
            parse("7;FILE://web-01/srv/文件"),
            at(Some("web-01"), "/srv/文件")
        );
        // No host, or this one by name, is no host to check.
        assert_eq!(parse("7;file:///tmp"), at(None, "/tmp"));
        assert_eq!(parse("7;file://localhost/tmp"), at(None, "/tmp"));
    }

    #[test]
    fn osc_1337_reports_a_path_alone() {
        assert_eq!(parse("1337;CurrentDir=/srv/app"), at(None, "/srv/app"));
        assert_eq!(parse("1337;SetMark"), None);
    }

    #[test]
    fn what_is_not_a_folder_is_not_reported() {
        for sequence in [
            "7;https://web-01/srv",
            "7;file://web-01",
            "7;file:/srv",
            "7;file://web-01/a%0Ab",
            "7;file://web-01/%FF",
            "1337;CurrentDir=relative",
            "9;file://web-01/srv",
            "0;title",
        ] {
            assert_eq!(parse(sequence), None, "{sequence}");
        }
    }

    #[test]
    fn a_report_naming_a_host_counts_only_from_that_host() {
        let named = at(Some("WEB-01"), "/srv").unwrap();
        assert!(named.is_from(Some("web-01")));
        assert!(!named.is_from(Some("db-01")));
        assert!(!named.is_from(None));
        assert!(at(None, "/srv").unwrap().is_from(None));
    }

    #[test]
    fn restoring_a_folder_quotes_it_as_one_literal_argument() {
        #[cfg(unix)]
        for path in [
            "/srv/itcm",
            "/srv/a b/中文",
            "/srv/it's;$(printf oops)`printf oops`",
            "/-logs",
        ] {
            let directory = at(None, path).unwrap();
            let command = directory.cd_command().unwrap();
            let output = std::process::Command::new("sh")
                .args(["-c", &format!("set -- {command}; printf '%s\\0' \"$@\"")])
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, format!("cd\0--\0{path}\0").as_bytes());
        }
        assert_eq!(
            at(None, "/srv/itcm").unwrap().cd_command().as_deref(),
            Some("cd -- '/srv/itcm'")
        );
        for path in ["relative", "/tmp\ninjected", "/tmp\r", "/tmp\x1b"] {
            assert!(at(None, path).unwrap().cd_command().is_none());
        }
    }
}
