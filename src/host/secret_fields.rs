//! The password, private key and passphrase fields of a login form: the
//! host form's password, and the credential form's password or key file and
//! passphrase. Each form lays the fields out in its own grid under its own
//! ids; this owns their state, reads back what the keychain saved for them,
//! and says what a commit should write.

use gpui_kit::component::{
    Sizable as _,
    button::Button,
    h_flex,
    input::{Input, InputContentType, InputEvent, InputState},
};
use gpui_kit::*;
use zeroize::Zeroizing;

use crate::i18n::t;
use crate::secrets::{SecretRef, SharedSecretStore};

/// What the keychain had for the login being edited.
#[derive(Default)]
struct SavedSecrets {
    password: Option<Zeroizing<String>>,
    passphrase: Option<Zeroizing<String>>,
    /// Which entries were looked up at all. An entry that was not looked up
    /// leaves its field meaning nothing when empty.
    looked_for_password: bool,
    looked_for_passphrase: bool,
}

pub struct SecretFields {
    password: Entity<InputState>,
    key_path: Entity<InputState>,
    passphrase: Entity<InputState>,
    /// Where saved secrets are read from. Writes go through the store, which
    /// owns the one error channel.
    secrets: SharedSecretStore,
    /// Whether the saved secret has been read back yet. Until it has, an
    /// empty field means "still loading", not "the user cleared it", so
    /// committing early never deletes anything.
    password_loaded: bool,
    passphrase_loaded: bool,
    _key_path_subscription: Subscription,
}

impl SecretFields {
    pub fn new(
        secrets: SharedSecretStore,
        key_path: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(t!("host.secret_fields.ask_each_time"))
        });
        let key_path = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("host.secret_fields.key_path_placeholder"))
                .default_value(key_path.unwrap_or_default())
        });
        let passphrase = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(t!("host.secret_fields.ask_each_time"))
        });
        // A passphrase belongs to a key file, so picking another key makes
        // whatever is in the field meaningless. Clearing it is also the
        // visible cue that the new key needs its own passphrase. Observers
        // hear of the new path, which the credential form reads the public
        // key from.
        let key_path_subscription = cx.subscribe_in(
            &key_path,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.forget_loaded_passphrase(window, cx);
                    cx.notify();
                }
            },
        );
        Self {
            password,
            key_path,
            passphrase,
            secrets,
            password_loaded: false,
            passphrase_loaded: false,
            _key_path_subscription: key_path_subscription,
        }
    }

    /// Whether secrets can be saved on this machine at all. Without a
    /// keychain the secret fields are disabled.
    pub fn keychain_available(&self) -> bool {
        self.secrets.is_available()
    }

    /// Read what the keychain holds for the login being edited, off the UI
    /// thread: the call blocks and on macOS may raise a system authorization
    /// dialog. The form stays usable while it runs. `password` is the entry
    /// the password field edits; the passphrase is the saved key file's.
    pub fn load_saved(
        &self,
        password: Option<SecretRef>,
        key_path: Option<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let secrets = self.secrets.clone();
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    SavedSecrets {
                        looked_for_password: password.is_some(),
                        looked_for_passphrase: key_path.is_some(),
                        password: password.and_then(|secret| secrets.get(&secret).ok().flatten()),
                        passphrase: key_path
                            .map(|path| SecretRef::passphrase(path.as_ref()))
                            .and_then(|secret| secrets.get(&secret).ok().flatten()),
                    }
                })
                .await;
            this.update_in(cx, |this, window, cx| this.fill_saved(loaded, window, cx))
                .ok();
        })
        .detach();
    }

    /// Put the saved secrets into their fields. A field the user has already
    /// typed into wins: the read is asynchronous and may land late.
    fn fill_saved(&mut self, loaded: SavedSecrets, window: &mut Window, cx: &mut Context<Self>) {
        if loaded.looked_for_password {
            if let Some(password) = loaded.password
                && self.password.read(cx).value().is_empty()
            {
                self.password
                    .update(cx, |input, cx| input.set_value(&*password, window, cx));
            }
            self.password_loaded = true;
        }
        if loaded.looked_for_passphrase {
            if let Some(passphrase) = loaded.passphrase
                && self.passphrase.read(cx).value().is_empty()
            {
                self.passphrase
                    .update(cx, |input, cx| input.set_value(&*passphrase, window, cx));
            }
            self.passphrase_loaded = true;
        }
        cx.notify();
    }

    /// Drop a passphrase that belonged to a key file the form no longer
    /// points at, so it cannot be written under the new key's name.
    fn forget_loaded_passphrase(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.passphrase_loaded {
            return;
        }
        self.passphrase_loaded = false;
        self.passphrase
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    /// Empty the passphrase field, as a key from elsewhere needs a
    /// passphrase of its own. Committing an empty field afterwards leaves
    /// whatever the keychain has alone.
    pub fn clear_passphrase(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.passphrase_loaded = false;
        self.passphrase
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    /// What the empty password field says leaving it empty does.
    pub fn set_password_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.password.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx)
        });
    }

    /// What the empty passphrase field says leaving it empty does.
    pub fn set_passphrase_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.passphrase.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx)
        });
    }

    /// Ask for a private key file with the system's picker.
    pub fn choose_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(t!("host.secret_fields.choose_key_prompt")),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                this.key_path.update(cx, |input, cx| {
                    input.set_value(path.to_string_lossy().into_owned(), window, cx)
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The key file, without surrounding blanks.
    pub fn key_path(&self, cx: &App) -> String {
        self.key_path.read(cx).value().trim().to_string()
    }

    /// The password as typed. Blanks around it are part of it.
    pub fn password(&self, cx: &App) -> String {
        self.password.read(cx).value().to_string()
    }

    pub fn passphrase(&self, cx: &App) -> String {
        self.passphrase.read(cx).value().to_string()
    }

    /// What a commit does with the password's keychain entry.
    pub fn password_change(&self, cx: &App) -> Option<Option<String>> {
        secret_change(self.password(cx), self.password_loaded)
    }

    /// What a commit does with the passphrase's keychain entry.
    pub fn passphrase_change(&self, cx: &App) -> Option<Option<String>> {
        secret_change(self.passphrase(cx), self.passphrase_loaded)
    }

    /// Drop the plaintext from the fields. Called as the dialog closes,
    /// rather than waiting for the form to go.
    pub fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for input in [&self.password, &self.passphrase] {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    pub fn password_input(&self, id: &'static str) -> Input {
        self.secret_input(&self.password, id)
    }

    pub fn passphrase_input(&self, id: &'static str) -> Input {
        self.secret_input(&self.passphrase, id)
    }

    fn secret_input(&self, state: &Entity<InputState>, id: &'static str) -> Input {
        Input::new(state)
            .id(id)
            .small()
            .mask_toggle()
            .content_type(InputContentType::Password)
            .disabled(!self.keychain_available())
    }

    /// The key file's field with its 「选择…」 button beside it.
    pub fn key_path_input(
        fields: &Entity<Self>,
        id: &'static str,
        choose_id: &'static str,
        cx: &App,
    ) -> impl IntoElement + use<> {
        let key_path = fields.read(cx).key_path.clone();
        let fields = fields.clone();
        h_flex()
            .gap_2()
            .w_full()
            .child(Input::new(&key_path).id(id).small().flex_1())
            .child(
                Button::new(choose_id)
                    .label(t!("host.secret_fields.choose"))
                    .small()
                    .on_click(move |_, window, cx| {
                        fields.update(cx, |fields, cx| fields.choose_key(window, cx))
                    }),
            )
    }
}

/// What to do with a secret field on commit: `None` leaves the keychain
/// alone, `Some(None)` deletes the entry, `Some(Some(value))` writes it.
///
/// An empty field only means "delete" once the saved value has been read
/// back. Before that it just means the read has not landed, so committing
/// straight away can never wipe a saved password.
fn secret_change(value: String, loaded: bool) -> Option<Option<String>> {
    if !value.is_empty() {
        Some(Some(value))
    } else if loaded {
        Some(None)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::secret_change;

    #[test]
    fn a_filled_field_is_written() {
        assert_eq!(
            secret_change("hunter2".into(), false),
            Some(Some("hunter2".into()))
        );
        assert_eq!(
            secret_change("hunter2".into(), true),
            Some(Some("hunter2".into()))
        );
    }

    #[test]
    fn an_emptied_field_deletes_only_once_the_saved_value_was_read() {
        assert_eq!(secret_change(String::new(), true), Some(None));
        assert_eq!(secret_change(String::new(), false), None);
    }

    #[test]
    fn surrounding_whitespace_is_part_of_the_secret() {
        assert_eq!(
            secret_change("  spaced  ".into(), true),
            Some(Some("  spaced  ".into()))
        );
    }
}
