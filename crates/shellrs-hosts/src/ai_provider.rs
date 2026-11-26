//! The model services of the AI assistant: which service, at what address,
//! with which models. They sync with the hosts, so they live in the same
//! database; the key of each is in the keychain under its public id, never
//! here.

use gpui_shared_string::SharedString;

use crate::secrets::SecretRef;

use super::PublicId;

/// Stable identity of a model service. Never reused within a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AiProviderId(pub u64);

/// The wire protocol a model service speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum AiProtocol {
    /// Anthropic's Messages API.
    Anthropic,
    /// OpenAI's Chat Completions API, which most services speak.
    #[default]
    OpenAi,
    /// OpenAI's Responses API.
    OpenAiResponses,
}

impl AiProtocol {
    pub const ALL: [AiProtocol; 3] = [
        AiProtocol::Anthropic,
        AiProtocol::OpenAi,
        AiProtocol::OpenAiResponses,
    ];

    /// The stored spelling, which sync writes too: it never changes.
    pub fn as_str(self) -> &'static str {
        match self {
            AiProtocol::Anthropic => "anthropic",
            AiProtocol::OpenAi => "openai",
            AiProtocol::OpenAiResponses => "openai_responses",
        }
    }

    pub fn from_stored(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|protocol| protocol.as_str() == value)
    }
}

/// Why a draft cannot be saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiProviderDraftError {
    Name,
    /// The address is missing, or not `http://` or `https://`.
    BaseUrl,
    /// No model is named.
    Model,
}

/// The values the model service form commits. Never holds the key: it goes
/// straight to the keychain.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct AiProviderDraft {
    pub name: SharedString,
    pub protocol: AiProtocol,
    pub base_url: SharedString,
    /// The models offered, in the user's order: the first is the one used
    /// when none is picked.
    pub models: Vec<SharedString>,
}

impl AiProviderDraft {
    pub fn new<M: Into<SharedString>>(
        name: impl Into<SharedString>,
        protocol: AiProtocol,
        base_url: impl Into<SharedString>,
        models: impl IntoIterator<Item = M>,
    ) -> Self {
        Self {
            name: name.into(),
            protocol,
            base_url: base_url.into(),
            models: models.into_iter().map(Into::into).collect(),
        }
    }

    /// The draft with surrounding blanks removed, the address without a
    /// trailing `/` and each model once, or the first thing wrong with it.
    pub fn validated(mut self) -> Result<Self, AiProviderDraftError> {
        self.name = self.name.trim().to_string().into();
        if self.name.is_empty() {
            return Err(AiProviderDraftError::Name);
        }
        let base_url = self.base_url.trim().trim_end_matches('/');
        let scheme = base_url
            .split_once("://")
            .map(|(scheme, rest)| (scheme.to_ascii_lowercase(), rest));
        match scheme {
            Some((scheme, rest)) if (scheme == "http" || scheme == "https") && !rest.is_empty() => {
            }
            _ => return Err(AiProviderDraftError::BaseUrl),
        }
        self.base_url = base_url.to_string().into();
        let mut models: Vec<SharedString> = Vec::new();
        for model in &self.models {
            let model = model.trim();
            if !model.is_empty() && !models.iter().any(|seen| seen.as_ref() == model) {
                models.push(model.to_string().into());
            }
        }
        if models.is_empty() {
            return Err(AiProviderDraftError::Model);
        }
        self.models = models;
        Ok(self)
    }
}

/// A saved model service.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct AiProvider {
    pub id: AiProviderId,
    /// Its id for sync, and where its key is in the keychain. Random, so a
    /// new service never finds a key an old one left.
    pub public_id: PublicId,
    pub name: SharedString,
    pub protocol: AiProtocol,
    pub base_url: SharedString,
    /// Never empty once saved; see [`AiProviderDraft::models`].
    pub models: Vec<SharedString>,
    /// Position in the list.
    pub sort_order: i64,
    /// When the user last changed it; see [`super::Host::edited_ms`].
    pub edited_ms: u64,
}

impl AiProvider {
    /// A service with a fresh public id. The store makes sure it is not one
    /// another service already has.
    pub fn new(id: AiProviderId, draft: AiProviderDraft) -> Self {
        Self {
            id,
            public_id: PublicId::generate(),
            name: draft.name,
            protocol: draft.protocol,
            base_url: draft.base_url,
            models: draft.models,
            sort_order: 0,
            edited_ms: 0,
        }
    }

    /// The editable fields, for pre-filling the form.
    pub fn draft(&self) -> AiProviderDraft {
        AiProviderDraft {
            name: self.name.clone(),
            protocol: self.protocol,
            base_url: self.base_url.clone(),
            models: self.models.clone(),
        }
    }

    /// `model` if the service offers it, else its first: the model a
    /// conversation or setting that names one uses.
    pub fn model_or_first(&self, model: Option<&str>) -> Option<SharedString> {
        model
            .and_then(|model| self.models.iter().find(|each| each.as_ref() == model))
            .or_else(|| self.models.first())
            .cloned()
    }

    /// Where its API key is kept.
    pub fn key_secret(&self) -> SecretRef {
        SecretRef::ai_key(self.public_id.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocols_round_trip_through_the_stored_spelling() {
        for protocol in AiProtocol::ALL {
            assert_eq!(AiProtocol::from_stored(protocol.as_str()), Some(protocol));
        }
        assert_eq!(AiProtocol::from_stored("grpc"), None);
    }

    #[test]
    fn a_draft_is_trimmed_and_checked() {
        let draft = AiProviderDraft::new(
            " DeepSeek ",
            AiProtocol::OpenAi,
            " https://api.deepseek.com/v1/ ",
            [" deepseek-chat ", "", "deepseek-reasoner", "deepseek-chat"],
        )
        .validated()
        .unwrap();
        assert_eq!(draft.name.as_ref(), "DeepSeek");
        assert_eq!(draft.base_url.as_ref(), "https://api.deepseek.com/v1");
        assert_eq!(draft.models, ["deepseek-chat", "deepseek-reasoner"]);

        let wrong = |name: &str, url: &str, model: &str| {
            AiProviderDraft::new(name, AiProtocol::OpenAi, url, [model])
                .validated()
                .unwrap_err()
        };
        assert_eq!(wrong(" ", "http://x", "m"), AiProviderDraftError::Name);
        assert_eq!(
            wrong("n", "api.openai.com", "m"),
            AiProviderDraftError::BaseUrl
        );
        assert_eq!(wrong("n", "ftp://x", "m"), AiProviderDraftError::BaseUrl);
        assert_eq!(wrong("n", "https://", "m"), AiProviderDraftError::BaseUrl);
        assert_eq!(
            wrong("n", "http://localhost:11434/v1", " "),
            AiProviderDraftError::Model
        );
        let none: [&str; 0] = [];
        assert_eq!(
            AiProviderDraft::new("n", AiProtocol::OpenAi, "http://x", none)
                .validated()
                .unwrap_err(),
            AiProviderDraftError::Model
        );
    }

    #[test]
    fn a_model_not_offered_is_the_first_instead() {
        let provider = AiProvider::new(
            AiProviderId(1),
            AiProviderDraft::new("n", AiProtocol::OpenAi, "https://a", ["one", "two"]),
        );
        assert_eq!(provider.model_or_first(Some("two")).as_deref(), Some("two"));
        assert_eq!(
            provider.model_or_first(Some("gone")).as_deref(),
            Some("one")
        );
        assert_eq!(provider.model_or_first(None).as_deref(), Some("one"));
    }

    #[test]
    fn the_key_is_kept_under_the_public_id() {
        let provider = AiProvider::new(
            AiProviderId(1),
            AiProviderDraft::new("n", AiProtocol::Anthropic, "https://a", ["m"]),
        );
        assert_eq!(
            provider.key_secret().account(),
            format!("ai_key:{}", provider.public_id.as_str())
        );
    }
}
