//! What looks like a secret taken out of what is sent to a model service:
//! private keys, tokens, passwords written on a command line. A best
//! effort: what it misses goes as it is, so the user sees every tool's
//! output before the model's answer is acted on.

use std::sync::LazyLock;

use regex::{Captures, Regex};

/// What a secret is replaced with.
const REMOVED: &str = "[secret removed]";

struct Rule {
    pattern: Regex,
    /// The group that is the secret; the rest of the match stays.
    group: usize,
}

static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    let rule = |pattern: &str, group| Rule {
        pattern: Regex::new(pattern).expect("a valid pattern"),
        group,
    };
    vec![
        rule(
            r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?(?:-----END [A-Z0-9 ]*PRIVATE KEY-----|\z)",
            0,
        ),
        rule(r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b", 0),
        rule(r"\bgh[pousr]_[A-Za-z0-9]{30,}\b", 0),
        rule(r"\bgithub_pat_[A-Za-z0-9_]{20,}\b", 0),
        rule(r"\bglpat-[A-Za-z0-9_-]{20,}\b", 0),
        rule(r"\bxox[abprs]-[A-Za-z0-9-]{10,}\b", 0),
        rule(r"\bsk-[A-Za-z0-9_-]{20,}\b", 0),
        rule(
            r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b",
            0,
        ),
        rule(
            r"(?i)\b(?:authorization:\s*(?:bearer|basic)|bearer)\s+([A-Za-z0-9._~+/=-]{12,})",
            1,
        ),
        rule(
            // `DB_PASSWORD=…` as well as `password: …`; not a value a rule
            // above already took out.
            r#"(?i)\b[a-z0-9_]*?(?:password|passwd|pwd|passphrase|secret|token|api[_-]?key|access[_-]?key|client[_-]?secret)["']?\s*[:=]\s*["']?([^\s"',;\[][^\s"',;]{3,})"#,
            1,
        ),
        rule(
            r"(?i)\b(?:mysql|mysqladmin|mysqldump|mariadb)\b[^\n]*?\s-p([^\s-][^\s]*)",
            1,
        ),
        rule(r"(?i)\b[a-z][a-z0-9+.-]*://[^\s:/@]+:([^\s@/]+)@", 1),
    ]
});

/// `text` with what looks like a secret replaced.
pub fn redact(text: &str) -> String {
    let mut text = text.to_owned();
    for rule in RULES.iter() {
        if !rule.pattern.is_match(&text) {
            continue;
        }
        text = rule
            .pattern
            .replace_all(&text, |captures: &Captures| {
                let whole = captures.get(0).expect("a match");
                match captures.get(rule.group) {
                    Some(secret) if rule.group > 0 => {
                        let start = secret.start() - whole.start();
                        let end = secret.end() - whole.start();
                        let whole = whole.as_str();
                        format!("{}{REMOVED}{}", &whole[..start], &whole[end..])
                    }
                    _ => REMOVED.to_owned(),
                }
            })
            .into_owned();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_taken_out_and_what_is_around_them_stays() {
        let cases = [
            (
                "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk\n-----END OPENSSH PRIVATE KEY-----\nnext",
                "[secret removed]\nnext",
            ),
            ("id AKIAIOSFODNN7EXAMPLE ok", "id [secret removed] ok"),
            (
                "token ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789",
                "token [secret removed]",
            ),
            (
                "export OPENAI_API_KEY=sk-proj-abcdefghijklmnopqrstuvwxyz",
                "export OPENAI_API_KEY=[secret removed]",
            ),
            (
                "password=hunter22 user=bob",
                "password=[secret removed] user=bob",
            ),
            (
                "DB_PASSWORD: \"s3cr3t!\"",
                "DB_PASSWORD: \"[secret removed]\"",
            ),
            (
                "mysql -u root -pSecret123 db",
                "mysql -u root -p[secret removed] db",
            ),
            (
                "Authorization: Bearer abcdefghijklmnop.qrs",
                "Authorization: Bearer [secret removed]",
            ),
            (
                "git clone https://bob:t0ps3cret@git.example.com/x.git",
                "git clone https://bob:[secret removed]@git.example.com/x.git",
            ),
        ];
        for (text, redacted) in cases {
            assert_eq!(redact(text), redacted, "{text}");
        }
    }

    #[test]
    fn ordinary_output_is_left_as_it_is() {
        for text in [
            "Filesystem      Size  Used Avail Use% Mounted on\n/dev/sda1        50G   20G   28G  42% /",
            "ssh: connect to host example.com port 22: Connection refused",
            "password: ",
            "PasswordAuthentication no",
            "mysql -u root -p db",
        ] {
            assert_eq!(redact(text), text);
        }
    }
}
