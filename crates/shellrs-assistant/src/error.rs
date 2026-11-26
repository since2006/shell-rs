//! Why talking to a model service failed, as types: the apps word them.

use std::fmt;
use std::time::Duration;

/// What went wrong with a model service, sorted by what the user can do
/// about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderErrorKind {
    /// The key is wrong, expired or not allowed this model (401, 403).
    KeyRejected,
    /// The address or the model name is wrong (404).
    NotFound,
    /// The account has no balance or quota left (402, or a 429 that says
    /// so).
    QuotaExhausted,
    /// Too many requests for now (429).
    RateLimited,
    /// The service is overloaded (529, `overloaded_error`).
    Overloaded,
    /// The conversation is longer than the model takes.
    ContextTooLong,
    /// The service refused the request as written (400 and the like).
    BadRequest,
    /// The service failed (5xx).
    ServerError,
    /// The service could not be reached, or the connection broke.
    Network,
    /// The service took too long to answer.
    Timeout,
    /// The answer was not what the protocol says.
    Protocol,
}

/// A model service's failure: its kind, and what the service said, which
/// is kept as it came (the services' own words, often English).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderError {
    pub kind: ProviderErrorKind,
    /// The HTTP status, when there was an answer.
    pub status: Option<u16>,
    /// The service's error message, when it gave one.
    pub message: Option<String>,
    /// How long the service asked to wait before trying again.
    pub retry_after: Option<Duration>,
}

impl ProviderError {
    pub fn new(kind: ProviderErrorKind) -> Self {
        Self {
            kind,
            status: None,
            message: None,
            retry_after: None,
        }
    }

    pub(crate) fn with_message(mut self, message: impl Into<String>) -> Self {
        let message = message.into();
        if !message.trim().is_empty() {
            self.message = Some(message);
        }
        self
    }

    /// Whether the same request may succeed if sent again later.
    pub fn is_retryable(&self) -> bool {
        match self.kind {
            ProviderErrorKind::RateLimited
            | ProviderErrorKind::Overloaded
            | ProviderErrorKind::Network
            | ProviderErrorKind::Timeout => true,
            ProviderErrorKind::ServerError => {
                matches!(self.status, None | Some(500 | 502 | 503 | 504))
            }
            ProviderErrorKind::BadRequest => self.status == Some(408),
            _ => false,
        }
    }

    /// Sort an HTTP error answer: its status and the body the service
    /// sent, which is JSON in every protocol known.
    pub(crate) fn from_answer(status: u16, body: &[u8], retry_after: Option<Duration>) -> Self {
        let (code, message) = error_fields(body);
        let says = |words: &[&str]| {
            let text = format!("{code} {message}").to_ascii_lowercase();
            words.iter().any(|word| text.contains(word))
        };
        let context_full = says(&[
            "context_length",
            "context length",
            "context window",
            "too many tokens",
            "prompt is too long",
            "maximum context",
        ]);
        let quota = says(&[
            "insufficient_quota",
            "insufficient balance",
            "insufficient_balance",
            "credit balance",
            "quota",
            "billing",
        ]);
        let kind = match status {
            401 | 403 => ProviderErrorKind::KeyRejected,
            404 => ProviderErrorKind::NotFound,
            402 => ProviderErrorKind::QuotaExhausted,
            429 if quota => ProviderErrorKind::QuotaExhausted,
            429 => ProviderErrorKind::RateLimited,
            529 => ProviderErrorKind::Overloaded,
            _ if says(&["overloaded"]) => ProviderErrorKind::Overloaded,
            _ if context_full => ProviderErrorKind::ContextTooLong,
            500..=599 => ProviderErrorKind::ServerError,
            _ => ProviderErrorKind::BadRequest,
        };
        Self {
            kind,
            status: Some(status),
            message: None,
            retry_after,
        }
        .with_message(message)
    }

    /// Sort an error the service sent in its stream, after a 200.
    pub(crate) fn from_stream(body: &[u8]) -> Self {
        let (code, _) = error_fields(body);
        // Anthropic's error types, and OpenAI's codes, which may come after
        // a type (`"invalid_request_error insufficient_quota"`).
        let status = code
            .split_whitespace()
            .find_map(|code| {
                Some(match code {
                    "overloaded_error" => 529,
                    "rate_limit_error" | "rate_limit_exceeded" | "insufficient_quota" => 429,
                    "api_error" | "server_error" => 500,
                    "authentication_error" | "invalid_api_key" => 401,
                    "permission_error" => 403,
                    "not_found_error" | "model_not_found" => 404,
                    "request_too_large" => 413,
                    _ => return None,
                })
            })
            .unwrap_or(400);
        let mut error = Self::from_answer(status, body, None);
        error.status = None;
        error
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.kind)?;
        if let Some(status) = self.status {
            write!(f, " (HTTP {status})")?;
        }
        if let Some(message) = &self.message {
            write!(f, ": {message}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ProviderError {}

/// The code and the message of a JSON error body, in the shapes services
/// use: `{"error": {"type"|"code", "message"}}`, `{"error": "…"}`,
/// `{"message": "…"}`. Neither, for a body that is none of them.
fn error_fields(body: &[u8]) -> (String, String) {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        let text = String::from_utf8_lossy(body);
        return (String::new(), text.chars().take(500).collect());
    };
    let error = value.get("error").unwrap_or(&value);
    if let Some(text) = error.as_str() {
        return (String::new(), text.into());
    }
    let field = |name: &str| {
        error
            .get(name)
            .map(|value| match value {
                serde_json::Value::String(text) => text.clone(),
                other => other.to_string(),
            })
            .unwrap_or_default()
    };
    let code = [field("type"), field("code")].join(" ").trim().to_owned();
    (code, field("message"))
}

/// Why the conversations' database failed.
#[derive(Debug)]
pub enum StoreError {
    /// The file is from a newer ShellRS.
    Newer {
        version: i64,
    },
    Database(rusqlite::Error),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Newer { version } => {
                write!(
                    f,
                    "the conversations were saved by a newer ShellRS (version {version})"
                )
            }
            Self::Database(error) => write!(f, "the conversations' database failed: {error}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            Self::Newer { .. } => None,
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(status: u16, body: &str) -> ProviderErrorKind {
        ProviderError::from_answer(status, body.as_bytes(), None).kind
    }

    #[test]
    fn answers_are_sorted_by_what_the_user_can_do() {
        let anthropic = |kind: &str, message: &str| {
            format!(r#"{{"type":"error","error":{{"type":"{kind}","message":"{message}"}}}}"#)
        };
        assert_eq!(
            kind(401, &anthropic("authentication_error", "invalid x-api-key")),
            ProviderErrorKind::KeyRejected
        );
        assert_eq!(
            kind(404, r#"{"error":{"message":"model not found"}}"#),
            ProviderErrorKind::NotFound
        );
        assert_eq!(
            kind(
                429,
                r#"{"error":{"code":"insufficient_quota","message":"You exceeded your current quota"}}"#
            ),
            ProviderErrorKind::QuotaExhausted
        );
        assert_eq!(
            kind(402, r#"{"error":{"message":"Insufficient Balance"}}"#),
            ProviderErrorKind::QuotaExhausted
        );
        assert_eq!(
            kind(429, &anthropic("rate_limit_error", "slow down")),
            ProviderErrorKind::RateLimited
        );
        assert_eq!(
            kind(529, &anthropic("overloaded_error", "Overloaded")),
            ProviderErrorKind::Overloaded
        );
        assert_eq!(
            kind(
                400,
                &anthropic(
                    "invalid_request_error",
                    "prompt is too long: 210000 tokens > 200000 maximum"
                )
            ),
            ProviderErrorKind::ContextTooLong
        );
        assert_eq!(
            kind(
                400,
                r#"{"error":{"code":"context_length_exceeded","message":"…"}}"#
            ),
            ProviderErrorKind::ContextTooLong
        );
        assert_eq!(kind(400, "<html>bad</html>"), ProviderErrorKind::BadRequest);
        assert_eq!(kind(503, "unavailable"), ProviderErrorKind::ServerError);
    }

    #[test]
    fn what_the_service_said_is_kept() {
        let error = ProviderError::from_answer(
            401,
            br#"{"error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
            None,
        );
        assert_eq!(error.message.as_deref(), Some("invalid x-api-key"));
        assert_eq!(error.status, Some(401));
        let plain = ProviderError::from_answer(500, b"upstream timeout", None);
        assert_eq!(plain.message.as_deref(), Some("upstream timeout"));
    }

    #[test]
    fn only_what_may_pass_is_retried() {
        let retried = |status: u16| ProviderError::from_answer(status, b"{}", None).is_retryable();
        for status in [408, 429, 500, 502, 503, 504, 529] {
            assert!(retried(status), "{status}");
        }
        for status in [400, 401, 402, 403, 404, 501] {
            assert!(!retried(status), "{status}");
        }
        assert!(ProviderError::new(ProviderErrorKind::Network).is_retryable());
        assert!(
            ProviderError::from_stream(
                br#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#
            )
            .is_retryable()
        );
    }
}
