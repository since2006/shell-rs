//! One-time codes (TOTP, RFC 6238), as authenticator apps make them: six
//! digits from HMAC-SHA1 of a base32 secret, a new one every 30 seconds.
//! A host that has its secret fills in the code when the server's
//! keyboard-interactive question asks for one ([`prompt_matches`]).

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hmac::{Hmac, KeyInit as _, Mac as _};
use sha1::Sha1;
use zeroize::Zeroizing;

/// How long one code lasts, in seconds.
pub const STEP_SECONDS: u64 = 30;

/// What a question for a one-time code usually says, in lower case: what a
/// question is matched against when the host names no wording of its own.
pub const USUAL_PROMPTS: [&str; 15] = [
    "verification code",
    "verify code",
    "one-time",
    "one time",
    "otp",
    "mfa",
    "2fa",
    "two-factor",
    "two factor",
    "passcode",
    "authenticator",
    "验证码",   // i18n: keep
    "动态码",   // i18n: keep
    "动态口令", // i18n: keep
    "动态密码", // i18n: keep
];

/// Why a secret cannot make codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TotpSecretError {
    /// Nothing but spaces.
    Empty,
    /// Not base32: a letter other than A–Z and 2–7, or too short to hold a
    /// byte.
    Invalid,
}

impl fmt::Display for TotpSecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "the one-time code secret is empty",
            Self::Invalid => "the one-time code secret isn't base32",
        })
    }
}

impl std::error::Error for TotpSecretError {}

/// `text` as it is saved: in capitals, without the spaces and dashes apps
/// group it with, or the `=` that pads it.
pub fn normalize_secret(text: &str) -> Zeroizing<String> {
    Zeroizing::new(
        text.chars()
            .filter(|c| !c.is_whitespace() && *c != '-' && *c != '=')
            .map(|c| c.to_ascii_uppercase())
            .collect(),
    )
}

/// The secret's bytes, from base32 written as authenticator apps show it
/// ([`normalize_secret`]). Bits left over after the last whole byte are
/// dropped, as those apps drop them.
pub fn parse_secret(text: &str) -> Result<Zeroizing<Vec<u8>>, TotpSecretError> {
    let text = normalize_secret(text);
    if text.is_empty() {
        return Err(TotpSecretError::Empty);
    }
    let mut bytes = Zeroizing::new(Vec::with_capacity(text.len() * 5 / 8));
    let (mut buffer, mut bits) = (0u32, 0u32);
    for c in text.bytes() {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'2'..=b'7' => c - b'2' + 26,
            _ => return Err(TotpSecretError::Invalid),
        };
        buffer = (buffer << 5) | u32::from(value);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            bytes.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    if bytes.is_empty() {
        return Err(TotpSecretError::Invalid);
    }
    Ok(bytes)
}

/// Which code is current `unix_seconds` after the epoch.
pub fn step_at(unix_seconds: u64) -> u64 {
    unix_seconds / STEP_SECONDS
}

/// The code that is current now, by this computer's clock.
pub fn current_step() -> u64 {
    step_at(unix_now().as_secs())
}

/// How long until code `step` is current: nothing once it is.
pub fn until_step(step: u64) -> Duration {
    Duration::from_secs(step.saturating_mul(STEP_SECONDS)).saturating_sub(unix_now())
}

fn unix_now() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

/// The six-digit code of `secret` for `step`.
pub fn code(secret: &[u8], step: u64) -> Zeroizing<String> {
    let mut mac = Hmac::<Sha1>::new_from_slice(secret).expect("a key of any length");
    mac.update(&step.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = usize::from(digest[digest.len() - 1] & 0x0f);
    let truncated = u32::from_be_bytes([
        digest[offset] & 0x7f,
        digest[offset + 1],
        digest[offset + 2],
        digest[offset + 3],
    ]);
    Zeroizing::new(format!("{:06}", truncated % 1_000_000))
}

/// Whether a question that says `text` asks for the one-time code: it holds
/// `wording`, the host's own, or one of the [`USUAL_PROMPTS`] when the host
/// has none. Case does not count, nor spaces or a colon around `wording`.
pub fn prompt_matches(wording: Option<&str>, text: &str) -> bool {
    let text = text.to_lowercase();
    match wording
        .map(|wording| wording.trim().trim_end_matches([':', '：']).trim_end()) // i18n: keep
        .filter(|wording| !wording.is_empty())
    {
        Some(wording) => text.contains(&wording.to_lowercase()),
        None => USUAL_PROMPTS.iter().any(|usual| text.contains(usual)),
    }
}

/// Which of one keyboard-interactive exchange's `questions` ask for the
/// one-time code ([`prompt_matches`]). A server may explain in the
/// exchange's `name` or `instructions` and ask only `Code:`: a lone
/// question under words that match asks for it too, unless it asks for a
/// password.
pub fn asked_for_code(
    wording: Option<&str>,
    name: &str,
    instructions: &str,
    questions: &[&str],
) -> Vec<bool> {
    let mut asked: Vec<bool> = questions
        .iter()
        .map(|question| prompt_matches(wording, question))
        .collect();
    if let ([question], [lone]) = (questions, asked.as_mut_slice())
        && !*lone
        && !asks_password(question)
    {
        *lone = prompt_matches(wording, name) || prompt_matches(wording, instructions);
    }
    asked
}

/// Whether a question asks for a password or a passphrase.
fn asks_password(question: &str) -> bool {
    let question = question.to_lowercase();
    ["password", "passphrase", "密码", "口令"] // i18n: keep
        .iter()
        .any(|word| question.contains(word))
}

#[cfg(test)]
#[path = "totp_docker_tests.rs"]
mod docker_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6238's secret for SHA-1, "12345678901234567890", in base32.
    const RFC_SECRET: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

    #[test]
    fn codes_match_the_rfc_6238_test_vectors() {
        let secret = parse_secret(RFC_SECRET).unwrap();
        assert_eq!(secret.as_slice(), b"12345678901234567890");
        // Appendix B's eight digits, of which authenticator apps show the
        // last six.
        for (time, expected) in [
            (59, "287082"),
            (1_111_111_109, "081804"),
            (1_111_111_111, "050471"),
            (1_234_567_890, "005924"),
            (2_000_000_000, "279037"),
            (20_000_000_000, "353130"),
        ] {
            assert_eq!(code(&secret, step_at(time)).as_str(), expected, "{time}");
        }
    }

    #[test]
    fn a_secret_may_be_grouped_padded_and_in_small_letters() {
        let plain = parse_secret("JBSWY3DPEHPK3PXP").unwrap();
        assert_eq!(plain.as_slice(), b"Hello!\xde\xad\xbe\xef");
        for written in [
            "jbsw y3dp ehpk 3pxp",
            "JBSW-Y3DP-EHPK-3PXP",
            " JBSWY3DPEHPK3PXP====\n",
        ] {
            assert_eq!(parse_secret(written).unwrap(), plain, "{written}");
        }
        assert_eq!(
            normalize_secret("jbsw y3dp-ehpk 3pxp==").as_str(),
            "JBSWY3DPEHPK3PXP"
        );
        // Google Authenticator's own secrets end on a part of a byte.
        assert_eq!(parse_secret("JBSWY3DPEE").unwrap().len(), 6);
    }

    #[test]
    fn what_is_not_base32_is_refused() {
        assert_eq!(parse_secret("").unwrap_err(), TotpSecretError::Empty);
        assert_eq!(parse_secret(" - ").unwrap_err(), TotpSecretError::Empty);
        assert_eq!(
            parse_secret("JBSWY3DP0HPK").unwrap_err(),
            TotpSecretError::Invalid
        );
        assert_eq!(
            parse_secret("123456").unwrap_err(),
            TotpSecretError::Invalid
        );
        assert_eq!(parse_secret("A").unwrap_err(), TotpSecretError::Invalid);
    }

    #[test]
    fn a_host_s_wording_matches_wherever_it_is_in_the_question() {
        let wording = Some("Please enter the mobile app OTP verification code");
        assert!(prompt_matches(
            wording,
            "Please enter the mobile app OTP verification code: "
        ));
        assert!(prompt_matches(
            Some("  otp verification code:  "),
            "[MFA] Please enter the mobile app OTP verification code"
        ));
        assert!(!prompt_matches(wording, "Password: "));
        assert!(!prompt_matches(wording, "Verification code: "));
    }

    #[test]
    fn without_a_wording_the_usual_questions_match() {
        for question in [
            "Verification code: ",
            "One-time password (OATH) for `me': ",
            "Please enter the mobile app OTP verification code:",
            "Enter MFA code: ",
            "2FA token: ",
            "请输入动态口令：",
            "请输入验证码：",
        ] {
            assert!(prompt_matches(None, question), "{question}");
            assert!(prompt_matches(Some(" "), question), "{question}");
        }
        for question in ["Password: ", "密码：", "Passphrase for key: ", "(me@box) "] {
            assert!(!prompt_matches(None, question), "{question}");
        }
    }

    #[test]
    fn a_lone_question_asks_for_the_code_its_exchange_explains() {
        let wording = Some("OTP verification code");
        assert_eq!(
            asked_for_code(
                wording,
                "",
                "Please enter the mobile app OTP verification code.",
                &["Code: "]
            ),
            [true]
        );
        assert_eq!(
            asked_for_code(None, "Two-factor login", "", &["Code: "]),
            [true]
        );
        // A password asked under the same words stays a password.
        assert_eq!(
            asked_for_code(
                None,
                "",
                "Password, then a verification code.",
                &["Password: "]
            ),
            [false]
        );
        // Several questions are matched each by its own words.
        assert_eq!(
            asked_for_code(
                None,
                "",
                "Two-factor login",
                &["Password: ", "Verification code: "]
            ),
            [false, true]
        );
        assert_eq!(asked_for_code(wording, "", "", &["Password: "]), [false]);
        assert!(asked_for_code(None, "", "", &[]).is_empty());
    }

    #[test]
    fn waiting_for_a_later_code_takes_at_most_one_step() {
        let now = current_step();
        assert_eq!(until_step(now), Duration::ZERO);
        assert!(until_step(now + 1) <= Duration::from_secs(STEP_SECONDS));
    }
}
