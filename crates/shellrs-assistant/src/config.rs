//! Which model service a session talks to, as the app resolved it: the
//! saved record and its key from the keychain, read before the session
//! starts. This crate reads neither.

use std::net::IpAddr;

use zeroize::Zeroizing;

/// The wire protocol of a model service.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Protocol {
    /// Anthropic's Messages API (`/v1/messages`), and the services that
    /// speak it.
    Anthropic,
    /// OpenAI's Chat Completions API (`/v1/chat/completions`), which most
    /// services speak.
    OpenAi,
    /// OpenAI's Responses API (`/v1/responses`).
    OpenAiResponses,
}

impl Protocol {
    pub const ALL: [Self; 3] = [Self::Anthropic, Self::OpenAi, Self::OpenAiResponses];

    /// How the protocol is written in a model service's record. Never
    /// changes: synced records hold it.
    pub fn key(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::OpenAiResponses => "openai_responses",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|protocol| protocol.key() == key)
    }

    /// The path its requests go to, after the service's address.
    pub fn path(self) -> &'static str {
        match self {
            Self::Anthropic => "/v1/messages",
            Self::OpenAi => "/v1/chat/completions",
            Self::OpenAiResponses => "/v1/responses",
        }
    }
}

/// `path` after a service's address, as the services expect it: an address
/// that already names a version of the API (`…/v1`, `…/api/paas/v4`,
/// `…/v1beta/openai`) is followed by the path without its `/v1`.
pub fn endpoint_url(base_url: &str, path: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    let after_scheme = base.split_once("://").map_or(base, |(_, rest)| rest);
    let versioned = after_scheme.split('/').skip(1).any(is_version);
    let path = match path.strip_prefix("/v1") {
        Some(rest) if versioned => rest,
        _ => path,
    };
    format!("{base}{path}")
}

/// `v1`, `v4`, `v1beta`: a path segment that names an API's version.
fn is_version(segment: &str) -> bool {
    let Some(rest) = segment.strip_prefix('v') else {
        return false;
    };
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && rest[digits..].chars().all(|c| c.is_ascii_alphanumeric())
}

/// A model service's API key. Its `Debug` shows nothing of it, and it is
/// wiped when dropped.
#[derive(Clone)]
pub struct ApiKey(Zeroizing<String>);

impl ApiKey {
    pub fn new(key: impl Into<String>) -> Self {
        Self(Zeroizing::new(key.into()))
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(…)")
    }
}

/// The model service a session talks to.
#[derive(Clone, Debug)]
pub struct ProviderConfig {
    pub protocol: Protocol,
    /// The address the protocol's path goes after ([`endpoint_url`]), as
    /// services give it: `https://api.anthropic.com`,
    /// `https://api.deepseek.com`, `https://open.bigmodel.cn/api/paas/v4`.
    pub base_url: String,
    pub model: String,
    /// `None` for a service that takes none, such as Ollama on this
    /// computer.
    pub api_key: Option<ApiKey>,
}

impl ProviderConfig {
    /// Where requests go: the address with the protocol's path after it.
    pub fn endpoint(&self) -> String {
        endpoint_url(&self.base_url, self.protocol.path())
    }

    /// Where the service lists its models: `/v1/models` in every protocol.
    pub fn models_endpoint(&self) -> String {
        endpoint_url(&self.base_url, "/v1/models")
    }

    /// What tells one replay of a model's own content from another's:
    /// content is sent back as the model gave it only to the same model of
    /// the same service.
    pub(crate) fn source(&self) -> String {
        format!(
            "{}|{}|{}",
            self.protocol.key(),
            self.base_url.trim().trim_end_matches('/'),
            self.model.trim()
        )
    }

    fn host(&self) -> Option<String> {
        reqwest::Url::parse(self.base_url.trim())
            .ok()?
            .host_str()
            .map(|host| host.trim_matches(['[', ']']).to_ascii_lowercase())
    }

    /// Anthropic's own service, which takes Claude's own parameters; a
    /// service that only speaks its protocol may refuse them.
    pub(crate) fn is_anthropic(&self) -> bool {
        self.protocol == Protocol::Anthropic && self.host().as_deref() == Some("api.anthropic.com")
    }

    /// A service on this computer or the local network, as Ollama and LM
    /// Studio are: its models are small.
    fn is_local(&self) -> bool {
        let Some(host) = self.host() else {
            return false;
        };
        if host == "localhost" || host.ends_with(".local") {
            return true;
        }
        match host.parse::<IpAddr>() {
            Ok(IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
            Ok(IpAddr::V6(ip)) => ip.is_loopback() || (ip.segments()[0] & 0xfe00) == 0xfc00,
            Err(_) => false,
        }
    }

    /// How many tokens the conversation may hold, as far as the assistant
    /// counts: a guess low enough for the smaller models of the service, so
    /// trimming starts before the service refuses.
    pub fn context_window(&self) -> u64 {
        if self.is_anthropic() {
            200_000
        } else if self.is_local() {
            32_000
        } else {
            128_000
        }
    }

    /// `max_tokens` for Anthropic's protocol, which requires one: what
    /// Claude takes, and less for a service that only speaks the protocol.
    pub(crate) fn anthropic_max_tokens(&self) -> u32 {
        if self.is_anthropic() { 32_000 } else { 8_192 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(protocol: Protocol, base_url: &str) -> ProviderConfig {
        ProviderConfig {
            protocol,
            base_url: base_url.into(),
            model: "m".into(),
            api_key: None,
        }
    }

    #[test]
    fn a_key_never_shows_in_debug() {
        let key = ApiKey::new("sk-very-secret");
        assert_eq!(format!("{key:?}"), "ApiKey(…)");
        let mut config = config(Protocol::Anthropic, "https://api.anthropic.com/");
        config.api_key = Some(key);
        assert!(!format!("{config:?}").contains("secret"));
    }

    #[test]
    fn each_protocol_has_its_own_path() {
        let cases = [
            (
                Protocol::Anthropic,
                "https://api.anthropic.com",
                "https://api.anthropic.com/v1/messages",
            ),
            (
                Protocol::Anthropic,
                " https://api.deepseek.com/anthropic/ ",
                "https://api.deepseek.com/anthropic/v1/messages",
            ),
            (
                Protocol::OpenAi,
                "https://api.openai.com",
                "https://api.openai.com/v1/chat/completions",
            ),
            // An address that names its version keeps it.
            (
                Protocol::OpenAi,
                "http://localhost:11434/v1",
                "http://localhost:11434/v1/chat/completions",
            ),
            (
                Protocol::OpenAi,
                "https://open.bigmodel.cn/api/paas/v4/",
                "https://open.bigmodel.cn/api/paas/v4/chat/completions",
            ),
            (
                Protocol::OpenAi,
                "https://generativelanguage.googleapis.com/v1beta/openai",
                "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
            ),
            (
                Protocol::OpenAiResponses,
                "https://api.openai.com",
                "https://api.openai.com/v1/responses",
            ),
            // A host that looks like a version is not one.
            (
                Protocol::OpenAiResponses,
                "https://v1.example.com",
                "https://v1.example.com/v1/responses",
            ),
        ];
        for (protocol, base_url, endpoint) in cases {
            assert_eq!(
                config(protocol, base_url).endpoint(),
                endpoint,
                "{base_url}"
            );
        }
        assert_eq!(
            config(Protocol::Anthropic, "https://api.anthropic.com").models_endpoint(),
            "https://api.anthropic.com/v1/models"
        );
        assert_eq!(
            config(Protocol::OpenAi, "https://api.siliconflow.cn/v1").models_endpoint(),
            "https://api.siliconflow.cn/v1/models"
        );
    }

    #[test]
    fn a_service_is_known_by_its_address() {
        assert!(config(Protocol::Anthropic, "https://api.anthropic.com").is_anthropic());
        assert!(!config(Protocol::Anthropic, "https://api.deepseek.com/anthropic").is_anthropic());
        assert_eq!(
            config(Protocol::Anthropic, "https://api.anthropic.com").context_window(),
            200_000
        );
        for local in [
            "http://localhost:11434",
            "http://127.0.0.1:1234/v1",
            "http://192.168.1.20:8000",
            "http://[::1]:8080",
            "http://studio.local:1234",
        ] {
            assert_eq!(
                config(Protocol::OpenAi, local).context_window(),
                32_000,
                "{local}"
            );
        }
        assert_eq!(
            config(Protocol::OpenAi, "https://api.deepseek.com").context_window(),
            128_000
        );
    }

    #[test]
    fn protocol_keys_read_back() {
        for protocol in Protocol::ALL {
            assert_eq!(Protocol::from_key(protocol.key()), Some(protocol));
        }
        assert_eq!(Protocol::from_key("grpc"), None);
    }
}
