//! The AI assistant's model services' part of `HostData`: written through
//! and synced like the credentials, their keys in the keychain.

use super::{Change, HostData, edited_after};
use crate::{AiProvider, AiProviderDraft, AiProviderId, PublicId};

impl HostData {
    /// Every model service, in list order.
    pub fn ai_providers(&self) -> &[AiProvider] {
        &self.ai_providers
    }

    pub fn ai_provider(&self, id: AiProviderId) -> Option<&AiProvider> {
        self.ai_providers.iter().find(|provider| provider.id == id)
    }

    /// The model service with this public id, as settings and conversations
    /// name it.
    pub fn ai_provider_by_public_id(&self, public_id: &str) -> Option<&AiProvider> {
        self.ai_providers
            .iter()
            .find(|provider| provider.public_id.as_str() == public_id)
    }

    /// Add a model service at the end of the list. Its key, if any, is saved
    /// apart with [`Self::save_secret`] under [`AiProvider::key_secret`].
    pub fn insert_ai_provider(&mut self, draft: AiProviderDraft) -> AiProviderId {
        let sort_order = self
            .ai_providers
            .iter()
            .map(|provider| provider.sort_order)
            .max()
            .unwrap_or(-1)
            + 1;
        let id = AiProviderId(self.next_ai_provider_id);
        self.next_ai_provider_id += 1;
        let mut provider = AiProvider::new(id, draft);
        provider.public_id = PublicId::generate_unused(|candidate| {
            self.ai_providers
                .iter()
                .any(|provider| &provider.public_id == candidate)
        });
        provider.sort_order = sort_order;
        provider.edited_ms = edited_after(0);
        self.persist(Change::NewAiProvider, |db| db.insert_ai_provider(&provider));
        self.ai_providers.push(provider);
        self.sync_revision += 1;
        self.changed();
        id
    }

    /// Replace the editable fields of a model service. Its key stays.
    pub fn update_ai_provider(&mut self, id: AiProviderId, draft: AiProviderDraft) -> bool {
        let Some(provider) = self
            .ai_providers
            .iter_mut()
            .find(|provider| provider.id == id)
        else {
            return false;
        };
        let public_id = provider.public_id.clone();
        let sort_order = provider.sort_order;
        let edited_ms = provider.edited_ms;
        *provider = AiProvider::new(id, draft);
        provider.public_id = public_id;
        provider.sort_order = sort_order;
        provider.edited_ms = edited_after(edited_ms);
        let provider = provider.clone();
        self.persist(Change::SaveAiProvider, |db| {
            db.update_ai_provider(&provider)
        });
        self.sync_revision += 1;
        self.changed();
        true
    }

    /// Delete a model service and its key.
    pub fn remove_ai_provider(&mut self, id: AiProviderId) -> bool {
        let Some(index) = self
            .ai_providers
            .iter()
            .position(|provider| provider.id == id)
        else {
            return false;
        };
        let provider = self.ai_providers.remove(index);
        self.persist(Change::DeleteAiProvider, |db| db.remove_ai_provider(id));
        self.save_secret(provider.key_secret(), None);
        self.sync_revision += 1;
        self.changed();
        true
    }

    /// Put a model service at `to` in the list; the ones whose place moves
    /// are edited.
    pub fn move_ai_provider(&mut self, id: AiProviderId, to: usize) -> bool {
        let Some(from) = self
            .ai_providers
            .iter()
            .position(|provider| provider.id == id)
        else {
            return false;
        };
        let to = to.min(self.ai_providers.len() - 1);
        if from == to {
            return false;
        }
        let provider = self.ai_providers.remove(from);
        self.ai_providers.insert(to, provider);
        let mut moved = Vec::new();
        for (order, provider) in self.ai_providers.iter_mut().enumerate() {
            if provider.sort_order != order as i64 {
                provider.sort_order = order as i64;
                provider.edited_ms = edited_after(provider.edited_ms);
                moved.push(provider.clone());
            }
        }
        for provider in &moved {
            self.persist(Change::SaveAiProvider, |db| db.update_ai_provider(provider));
        }
        self.sync_revision += 1;
        self.changed();
        true
    }

    /// Put the services in their list order, as a sync round leaves them.
    pub(super) fn sort_ai_providers(&mut self) {
        self.ai_providers
            .sort_by_key(|provider| (provider.sort_order, provider.id.0));
    }
}

#[cfg(test)]
mod tests {
    use crate::secrets::InMemorySecretStore;
    use crate::secrets::SecretStore as _;
    use crate::{AiProtocol, AiProviderDraft, HostData, HostDatabase, SecretJob};
    use std::sync::Arc;

    fn draft(name: &str) -> AiProviderDraft {
        AiProviderDraft::new(
            name,
            AiProtocol::OpenAi,
            "https://api.deepseek.com",
            ["deepseek-chat"],
        )
    }

    #[test]
    fn a_model_service_is_written_through_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shellrs.db");
        let mut data = HostData::load(HostDatabase::open(&path).unwrap()).unwrap();
        let one = data.insert_ai_provider(draft("one"));
        let two = data.insert_ai_provider(draft("two"));
        let mut renamed = draft("first");
        renamed.models = vec!["deepseek-reasoner".into(), "deepseek-chat".into()];
        assert!(data.update_ai_provider(one, renamed));
        assert!(data.move_ai_provider(two, 0));
        let public_id = data.ai_provider(one).unwrap().public_id.clone();

        let again = HostData::load(HostDatabase::open(&path).unwrap()).unwrap();
        let names: Vec<_> = again
            .ai_providers()
            .iter()
            .map(|p| p.name.to_string())
            .collect();
        assert_eq!(names, ["two", "first"]);
        let first = again.ai_provider_by_public_id(public_id.as_str()).unwrap();
        assert_eq!(first.models, ["deepseek-reasoner", "deepseek-chat"]);
        assert!(first.edited_ms > 0);
    }

    #[test]
    fn deleting_a_model_service_deletes_its_key() {
        let secrets = Arc::new(InMemorySecretStore::default());
        let mut data = HostData::empty().with_secrets(secrets.clone());
        let id = data.insert_ai_provider(draft("one"));
        let key = data.ai_provider(id).unwrap().key_secret();
        data.save_secret(key.clone(), Some("sk-test".into()));
        for job in data.take_secret_jobs() {
            let outcome = job.run();
            data.secret_done(outcome);
        }
        assert_eq!(
            secrets
                .get(&key)
                .unwrap()
                .as_deref()
                .map(|key| key.as_str()),
            Some("sk-test")
        );

        assert!(data.remove_ai_provider(id));
        let jobs: Vec<SecretJob> = data.take_secret_jobs();
        assert_eq!(jobs.len(), 1);
        for job in jobs {
            let outcome = job.run();
            data.secret_done(outcome);
        }
        assert_eq!(secrets.get(&key).unwrap(), None);
        assert!(data.ai_providers().is_empty());
    }
}
