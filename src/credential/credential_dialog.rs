use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, WindowExt as _,
    clipboard::Clipboard,
    form::{Field, Form},
    h_flex,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use zeroize::Zeroizing;

use crate::host::{
    CredentialDraft, CredentialId, CredentialKind, DEFAULT_USER, GeneratedKey, HostStore,
    KeyAlgorithm, PastedKey, PastedKeyError, SecretFields, join_sentences, read_public_key,
};
use crate::i18n::{t, tn};
use crate::secrets::SecretRef;
use crate::shared::{
    Segment, SegmentedControl, commit_footer, dismiss_form_error, form_error_notification,
};

/// Where the SSH agent is found, as the form explains it.
fn agent_note() -> SharedString {
    if cfg!(windows) {
        t!("credential.dialog.agent_note_windows")
    } else {
        t!("credential.dialog.agent_note")
    }
}

/// What an empty passphrase field means for a key that already exists, and
/// for one about to be generated.
fn passphrase_placeholder(source: KeySource) -> SharedString {
    match source {
        KeySource::Generate => t!("credential.dialog.no_passphrase"),
        KeySource::File | KeySource::Paste => t!("credential.dialog.ask_for_passphrase"),
    }
}

/// Which credential dialog to open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialDialog {
    New,
    /// A new credential, at a key it generates: 「生成密钥…」.
    GenerateKey,
    Edit(CredentialId),
}

/// Where a key credential's private key comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum KeySource {
    /// A file of the user's, which the credential refers to.
    #[default]
    File,
    /// Text pasted into the form, which ShellRS keeps in a file of its own.
    Paste,
    /// A new key pair, which ShellRS makes and keeps.
    Generate,
}

impl KeySource {
    const ALL: [KeySource; 3] = [KeySource::File, KeySource::Paste, KeySource::Generate];

    fn label(self) -> SharedString {
        match self {
            KeySource::File => t!("credential.key_source.file"),
            KeySource::Paste => t!("credential.key_source.paste"),
            KeySource::Generate => t!("credential.key_source.generate"),
        }
    }
}

/// The key the form is generating or has generated.
enum Generated {
    Working,
    Ready(Arc<GeneratedKey>),
    Failed(SharedString),
}

/// What the form shows as the key's public half.
enum PublicKeyView {
    Line(SharedString),
    Generating,
    Failed(SharedString),
    Unknown,
}

/// The body of the new/edit credential dialog.
pub struct CredentialForm {
    store: Entity<HostStore>,
    editing: Option<CredentialId>,
    kind: CredentialKind,
    name: Entity<InputState>,
    user: Entity<InputState>,
    /// The password, key file and passphrase, shared with the host form.
    fields: Entity<SecretFields>,
    source: KeySource,
    key_text: Entity<TextareaState>,
    /// What the pasted text is, read again whenever it changes.
    pasted: Option<Result<PastedKey, PastedKeyError>>,
    algorithm: KeyAlgorithm,
    /// The key made for the algorithm it names. A request is numbered, so
    /// the answer to an earlier choice of algorithm is dropped.
    generated: Option<(KeyAlgorithm, Generated)>,
    generation: u64,
    /// The key file the form points at and its public half, which is read
    /// off the UI thread.
    file_public_key: Option<(String, Option<SharedString>)>,
    /// Where ShellRS keeps keys. Without it nothing can be pasted or
    /// generated.
    key_dir: Option<PathBuf>,
    /// The key file of the credential being edited, when ShellRS keeps it:
    /// saving the credential with any other key deletes it.
    kept_key: Option<SharedString>,
    /// How many hosts use the credential being edited, and how many of them
    /// are connected right now.
    hosts: usize,
    connected: usize,
    /// A commit is encrypting the new key with its passphrase.
    saving: bool,
    _subscriptions: Vec<Subscription>,
}

impl CredentialForm {
    pub fn new(
        dialog: CredentialDialog,
        store: Entity<HostStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editing = match dialog {
            CredentialDialog::Edit(id) => Some(id),
            CredentialDialog::New | CredentialDialog::GenerateKey => None,
        };
        let (credential, secrets, key_dir, kept_key, hosts, connected) = {
            let read = store.read(cx);
            let credential = editing.and_then(|id| read.credential(id)).cloned();
            let using: Vec<_> = editing
                .map(|id| read.hosts_using(id).collect())
                .unwrap_or_default();
            let connected = using
                .iter()
                .filter(|host| host.state.is_connected())
                .count();
            let kept_key = credential
                .as_ref()
                .filter(|credential| credential.keeps_key_in(read.key_dir()))
                .and_then(|credential| credential.key_path.clone());
            (
                credential,
                read.secrets(),
                read.key_dir().map(Path::to_path_buf),
                kept_key,
                using.len(),
                connected,
            )
        };
        let draft = credential
            .as_ref()
            .map(|credential| credential.draft())
            .unwrap_or_else(|| CredentialDraft::new("", CredentialKind::default(), DEFAULT_USER));
        // Without a place to keep it, 「生成密钥…」 still opens at a key,
        // one that has to be a file.
        let generate = dialog == CredentialDialog::GenerateKey;
        let source = if generate && key_dir.is_some() {
            KeySource::Generate
        } else {
            KeySource::File
        };
        let kind = if generate {
            CredentialKind::Key
        } else {
            draft.kind
        };

        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("credential.dialog.name_placeholder"))
                .default_value(draft.name.clone())
        });
        let user = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(DEFAULT_USER)
                .default_value(draft.user.clone())
        });
        let fields = cx.new(|cx| SecretFields::new(secrets, draft.key_path.clone(), window, cx));
        if let Some(credential) = &credential {
            fields.update(cx, |fields, cx| {
                fields.load_saved(
                    (credential.kind == CredentialKind::Password)
                        .then(|| credential.password_secret()),
                    credential.key_path.clone(),
                    cx,
                )
            });
        }
        if source == KeySource::Generate {
            fields.update(cx, |fields, cx| {
                fields.set_passphrase_placeholder(passphrase_placeholder(source), window, cx)
            });
        }
        let key_text = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(4, 8)
                .placeholder(t!("credential.dialog.key_text_placeholder"))
        });
        let subscriptions = vec![
            cx.observe(&fields, |this, _, cx| {
                this.look_up_file_public_key(cx);
                cx.notify();
            }),
            cx.subscribe(&key_text, |this, state, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = state.read(cx).value();
                    this.pasted = (!text.trim().is_empty()).then(|| PastedKey::parse(&text));
                    cx.notify();
                }
            }),
            // The generated key's comment is the credential's name.
            cx.subscribe(&name, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        let mut form = Self {
            store,
            editing,
            kind,
            name,
            user,
            fields,
            source,
            key_text,
            pasted: None,
            algorithm: KeyAlgorithm::default(),
            generated: None,
            generation: 0,
            file_public_key: None,
            key_dir,
            kept_key,
            hosts,
            connected,
            saving: false,
            _subscriptions: subscriptions,
        };
        form.look_up_file_public_key(cx);
        if source == KeySource::Generate {
            form.generate(cx);
        }
        form
    }

    /// Put the keyboard where a new credential starts: its name.
    fn focus_name(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.name.update(cx, |input, cx| input.focus(window, cx));
    }

    fn set_kind(&mut self, kind: CredentialKind, cx: &mut Context<Self>) {
        if self.kind != kind {
            self.kind = kind;
            cx.notify();
        }
    }

    fn set_source(&mut self, source: KeySource, window: &mut Window, cx: &mut Context<Self>) {
        if self.source == source {
            return;
        }
        self.source = source;
        // A passphrase belongs to one key; whatever the field held was for
        // the key the form showed before.
        self.fields.update(cx, |fields, cx| {
            fields.clear_passphrase(window, cx);
            fields.set_passphrase_placeholder(passphrase_placeholder(source), window, cx);
        });
        if source == KeySource::Generate {
            self.generate(cx);
        }
        cx.notify();
    }

    fn set_algorithm(&mut self, algorithm: KeyAlgorithm, cx: &mut Context<Self>) {
        if self.algorithm != algorithm {
            self.algorithm = algorithm;
            self.generate(cx);
            cx.notify();
        }
    }

    /// Make a key for the chosen algorithm, unless one is made or being
    /// made already. Off the UI thread: an RSA key takes a moment.
    fn generate(&mut self, cx: &mut Context<Self>) {
        let algorithm = self.algorithm;
        if self
            .generated
            .as_ref()
            .is_some_and(|(made_for, generated)| {
                *made_for == algorithm && !matches!(generated, Generated::Failed(_))
            })
        {
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        self.generated = Some((algorithm, Generated::Working));
        cx.spawn(async move |this, cx| {
            let made = cx
                .background_executor()
                .spawn(async move { GeneratedKey::generate(algorithm) })
                .await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                let generated = match made {
                    Ok(key) => Generated::Ready(Arc::new(key)),
                    Err(error) => {
                        Generated::Failed(t!("credential.dialog.generate_failed", error = error))
                    }
                };
                this.generated = Some((algorithm, generated));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Read the public half of the key file the form points at, if it
    /// changed since the last look. The file system is read off the UI
    /// thread.
    fn look_up_file_public_key(&mut self, cx: &mut Context<Self>) {
        let path = self.fields.read(cx).key_path(cx);
        if self
            .file_public_key
            .as_ref()
            .is_some_and(|(looked_up, _)| *looked_up == path)
        {
            return;
        }
        self.file_public_key = Some((path.clone(), None));
        if path.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let file = path.clone();
            let line = cx
                .background_executor()
                .spawn(async move { read_public_key(Path::new(&file)) })
                .await;
            this.update(cx, |this, cx| {
                if let Some((looked_up, found)) = &mut this.file_public_key
                    && *looked_up == path
                {
                    *found = line.map(Into::into);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Validate and write to the store. Returns whether the dialog may
    /// close; a key that is still being encrypted closes it when done.
    pub fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.saving {
            return false;
        }
        let key_from_text = self.kind == CredentialKind::Key && self.source != KeySource::File;
        let (key_file, password_change, passphrase_change, passphrase) = {
            let fields = self.fields.read(cx);
            (
                fields.key_path(cx),
                fields.password_change(cx),
                fields.passphrase_change(cx),
                Zeroizing::new(fields.passphrase(cx)),
            )
        };
        // A pasted or generated key's file is only written once everything
        // else checks out; until then the draft stands in for it.
        let key_path = if key_from_text {
            // A stand-in, never shown or kept.
            "（新私钥）".to_string() // i18n: keep
        } else {
            key_file
        };
        let draft = CredentialDraft::new(
            self.name.read(cx).value().to_string(),
            self.kind,
            self.user.read(cx).value().to_string(),
        )
        .with_key_path(key_path)
        .validated();
        let draft = match draft {
            Ok(draft) => draft,
            Err(error) => return self.fail(error.to_string(), window, cx),
        };
        // Secrets never ride along in the draft, which derives `Debug`.
        let password_change = password_change.filter(|_| draft.kind == CredentialKind::Password);
        if !key_from_text {
            let passphrase_change = passphrase_change.filter(|_| draft.key_path.is_some());
            return self.save(draft, None, password_change, passphrase_change, window, cx);
        }

        if self.source == KeySource::Paste {
            let pasted = match PastedKey::parse(&self.key_text.read(cx).value()) {
                Ok(pasted) => pasted,
                Err(error) => return self.fail(error.to_string(), window, cx),
            };
            // Only an encrypted key has a passphrase to keep; an empty one
            // clears what a key replaced in place had.
            let passphrase =
                (pasted.is_encrypted() && !passphrase.is_empty()).then(|| passphrase.to_string());
            let text = Zeroizing::new(pasted.text().to_string());
            return self.save(draft, Some(text), None, Some(passphrase), window, cx);
        }

        let key = match &self.generated {
            Some((algorithm, Generated::Ready(key))) if *algorithm == self.algorithm => key.clone(),
            Some((_, Generated::Failed(error))) => return self.fail(error.clone(), window, cx),
            _ => return self.fail(t!("credential.dialog.still_generating"), window, cx),
        };
        let comment = draft.name.clone();
        if passphrase.is_empty() {
            return match key.encode(&comment, "") {
                Ok(text) => self.save(draft, Some(text), None, Some(None), window, cx),
                Err(error) => self.fail(
                    t!("credential.dialog.encode_failed", error = error),
                    window,
                    cx,
                ),
            };
        }
        // Deriving the encryption key from a passphrase is slow on purpose,
        // so it happens off the UI thread with the dialog kept open.
        self.saving = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let secret = passphrase.clone();
            let encoded = cx
                .background_executor()
                .spawn(async move { key.encode(&comment, &secret) })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.saving = false;
                let saved = match encoded {
                    Ok(text) => {
                        let passphrase = Some(Some(passphrase.to_string()));
                        this.save(draft, Some(text), None, passphrase, window, cx)
                    }
                    Err(error) => this.fail(
                        t!("credential.dialog.encrypt_failed", error = error),
                        window,
                        cx,
                    ),
                };
                if saved {
                    // Closing it from here skips the dialog's `on_close`.
                    dismiss_form_error(window, cx);
                    window.close_dialog(cx);
                }
            })
            .ok();
        })
        .detach();
        false
    }

    /// Say what stands in the way of saving. Returns `false`, so the
    /// dialog stays open.
    fn fail(&self, error: impl Into<SharedString>, window: &mut Window, cx: &mut App) -> bool {
        window.push_notification(form_error_notification(error), cx);
        false
    }

    /// Write the key file for a pasted or generated key, then the
    /// credential, then its secrets. Returns whether the dialog may close.
    fn save(
        &mut self,
        mut draft: CredentialDraft,
        key_text: Option<Zeroizing<String>>,
        password_change: Option<Option<String>>,
        passphrase_change: Option<Option<String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let editing = self.editing;
        if let Some(text) = key_text {
            match self.store.read(cx).save_private_key(editing, &text) {
                Ok(path) => draft.key_path = Some(path),
                Err(error) => {
                    return self.fail(
                        t!("credential.dialog.key_not_saved", error = error),
                        window,
                        cx,
                    );
                }
            }
        }
        let passphrase_change = passphrase_change
            .and_then(|change| Some((SecretRef::passphrase(draft.key_path.as_deref()?), change)));
        self.store.update(cx, |store, cx| {
            // The credential first: an edit that reconnects its hosts reads
            // the keychain later, on the connection's own thread.
            let id = match editing {
                Some(id) => store.update_credential(id, draft, cx).then_some(id),
                None => Some(store.insert_credential(draft, cx)),
            };
            let secret = id
                .and_then(|id| store.credential(id))
                .map(|credential| credential.password_secret());
            if let (Some(secret), Some(change)) = (secret, password_change) {
                store.save_secret(secret, change, cx);
            }
            if let Some((secret, change)) = passphrase_change {
                store.save_secret(secret, change, cx);
            }
        });
        // The dialog is about to close and take the form with it; drop the
        // plaintext now rather than waiting for the entity.
        self.fields
            .update(cx, |fields, cx| fields.clear(window, cx));
        self.key_text
            .update(cx, |text, cx| text.set_value("", window, cx));
        self.pasted = None;
        self.generated = None;
        true
    }

    /// What editing the credential does to the hosts using it.
    fn usage_note(&self) -> Option<SharedString> {
        if self.hosts == 0 {
            return None;
        }
        let mut note = vec![tn!("credential.dialog.usage", self.hosts)];
        // A new key is no use to them until their servers know it.
        if self.kind == CredentialKind::Key && self.source != KeySource::File {
            note.push(t!("credential.dialog.usage_new_key"));
        }
        if self.connected > 0 {
            note.push(t!("credential.dialog.usage_reconnects"));
        }
        join_sentences(note)
    }

    /// Whether saving now would delete the key ShellRS keeps for the
    /// credential being edited: it is set to log in some other way, or with
    /// another file. Pasting or generating writes over the kept file instead.
    fn drops_kept_key(&self, cx: &App) -> bool {
        let Some(kept) = &self.kept_key else {
            return false;
        };
        match (self.kind, self.source) {
            (CredentialKind::Key, KeySource::File) => {
                self.fields.read(cx).key_path(cx) != kept.as_ref()
            }
            (CredentialKind::Key, KeySource::Paste | KeySource::Generate) => false,
            (CredentialKind::Password | CredentialKind::Agent, _) => true,
        }
    }

    /// The public half of the key the form shows.
    fn public_key(&self, cx: &App) -> PublicKeyView {
        match self.source {
            KeySource::File => {
                let path = self.fields.read(cx).key_path(cx);
                match &self.file_public_key {
                    Some((looked_up, Some(line))) if *looked_up == path => {
                        PublicKeyView::Line(line.clone())
                    }
                    _ => PublicKeyView::Unknown,
                }
            }
            KeySource::Paste => match &self.pasted {
                Some(Ok(pasted)) => pasted
                    .public_key_line()
                    .map_or(PublicKeyView::Unknown, |line| {
                        PublicKeyView::Line(line.into())
                    }),
                _ => PublicKeyView::Unknown,
            },
            KeySource::Generate => match &self.generated {
                Some((algorithm, Generated::Ready(key))) if *algorithm == self.algorithm => {
                    let name = self.name.read(cx).value();
                    PublicKeyView::Line(key.public_key_line(&name).into())
                }
                Some((_, Generated::Failed(error))) => PublicKeyView::Failed(error.clone()),
                _ => PublicKeyView::Generating,
            },
        }
    }

    /// The key fields: where the key comes from, the key itself, its public
    /// half and its passphrase.
    fn key_fields(&self, form: Form, cx: &Context<Self>) -> Form {
        let source = self.source;
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let can_keep = self.key_dir.is_some();
        let fields = self.fields.read(cx);
        let kept_here = source == KeySource::File
            && self
                .kept_key
                .as_ref()
                .is_some_and(|kept| fields.key_path(cx) == kept.as_ref());

        let sources = SegmentedControl::new("credential-key-source")
            .selected_index(KeySource::ALL.iter().position(|each| *each == source))
            .on_change(cx.listener(|this, ix: &usize, window, cx| {
                if let Some(source) = KeySource::ALL.get(*ix) {
                    this.set_source(*source, window, cx);
                }
            }))
            .segments(KeySource::ALL.map(|each| {
                Segment::new(each.label()).when(each != KeySource::File && !can_keep, |segment| {
                    segment
                        .disabled(true)
                        .tooltip(t!("credential.dialog.no_key_dir"))
                })
            }));
        // A generated key has nothing to fill in under the choice.
        let key = match source {
            KeySource::File => Some(
                SecretFields::key_path_input(
                    &self.fields,
                    "credential-key-path",
                    "choose-credential-key",
                    cx,
                )
                .into_any_element(),
            ),
            KeySource::Paste => Some(
                div()
                    .id("credential-key-text")
                    .test_support()
                    .w_full()
                    .child(
                        Textarea::new(&self.key_text)
                            .font_family(theme.mono_font_family.clone())
                            .text_xs(),
                    )
                    .into_any_element(),
            ),
            KeySource::Generate => None,
        };
        let mut form = form.child(
            Field::new()
                .label(t!("credential.dialog.private_key"))
                .required(true)
                .child(v_flex().w_full().gap_2().child(sources).children(key))
                .when(kept_here, |field| {
                    field.description(t!("credential.dialog.kept_key"))
                }),
        );
        if source == KeySource::Generate {
            form = form.child(
                Field::new()
                    .label(t!("credential.dialog.algorithm"))
                    .child(
                        SegmentedControl::new("credential-key-algorithm")
                            .selected_index(
                                KeyAlgorithm::ALL
                                    .iter()
                                    .position(|each| *each == self.algorithm),
                            )
                            .on_change(cx.listener(|this, ix: &usize, _, cx| {
                                if let Some(algorithm) = KeyAlgorithm::ALL.get(*ix) {
                                    this.set_algorithm(*algorithm, cx);
                                }
                            }))
                            .segments(KeyAlgorithm::ALL.map(|each| Segment::new(each.label()))),
                    )
                    .when(self.algorithm == KeyAlgorithm::Rsa, |field| {
                        field.description(t!("credential.dialog.rsa_note"))
                    }),
            );
        }
        form = match self.public_key(cx) {
            PublicKeyView::Line(line) => form.child(
                Field::new()
                    .label(t!("credential.dialog.public_key"))
                    .child(public_key_box(line, cx))
                    .when(source == KeySource::Generate, |field| {
                        field.description(t!("credential.dialog.public_key_note"))
                    }),
            ),
            PublicKeyView::Generating => form.child(
                Field::new()
                    .label(t!("credential.dialog.public_key"))
                    .child(
                        div()
                            .id("credential-public-key-pending")
                            .test_support()
                            .text_sm()
                            .text_color(muted)
                            .child(t!("credential.dialog.generating")),
                    ),
            ),
            PublicKeyView::Failed(error) => form.child(
                Field::new()
                    .label(t!("credential.dialog.public_key"))
                    .child(
                        div()
                            .id("credential-generate-error")
                            .test_support()
                            .aria_label(error.clone())
                            .text_sm()
                            .text_color(theme.danger)
                            .child(error),
                    ),
            ),
            PublicKeyView::Unknown => form,
        };
        // A pasted key without a passphrase has nothing to ask for.
        let encrypted_paste = matches!(&self.pasted, Some(Ok(pasted)) if pasted.is_encrypted());
        if source != KeySource::Paste || encrypted_paste {
            form = form.child(
                Field::new()
                    .label(t!("credential.dialog.passphrase"))
                    .child(fields.passphrase_input("credential-passphrase")),
            );
        }
        form
    }
}

/// The public half of a key, cut short to fit, with a button that copies
/// all of it.
fn public_key_box(line: SharedString, cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    h_flex()
        .w_full()
        .gap_1()
        .child(
            div()
                .id("credential-public-key")
                .test_support()
                .aria_label(line.clone())
                .flex_1()
                .min_w_0()
                .h_6()
                .px_2()
                .flex()
                .items_center()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .bg(theme.muted)
                .font_family(theme.mono_font_family.clone())
                .text_xs()
                .child(div().w_full().truncate().child(line.clone())),
        )
        .child(
            Clipboard::new("copy-public-key")
                .value(line)
                .tooltip(t!("credential.dialog.copy_public_key")),
        )
}

impl Render for CredentialForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let kind = self.kind;
        let muted = cx.theme().muted_foreground;
        let keychain = self.fields.read(cx).keychain_available();
        let secret_note = match kind {
            CredentialKind::Agent => None,
            _ if keychain => Some(t!("credential.dialog.keychain_note")),
            _ => Some(t!("credential.dialog.no_keychain_note")),
        };
        let drops_kept_key = self.drops_kept_key(cx);
        let form = Form::new()
            .child(
                Field::new().label(t!("credential.dialog.kind")).child(
                    SegmentedControl::new("credential-kind")
                        .selected_index(CredentialKind::ALL.iter().position(|each| *each == kind))
                        .on_change(cx.listener(|this, ix: &usize, _, cx| {
                            if let Some(kind) = CredentialKind::ALL.get(*ix) {
                                this.set_kind(*kind, cx);
                            }
                        }))
                        .segments(CredentialKind::ALL.map(|each| Segment::new(each.label()))),
                ),
            )
            .child(
                Field::new()
                    .label(t!("credential.dialog.name"))
                    .required(true)
                    .child(Input::new(&self.name).id("credential-name").small()),
            )
            .child(
                Field::new()
                    .label(t!("credential.dialog.user"))
                    .child(Input::new(&self.user).id("credential-user").small()),
            );
        let form = match kind {
            CredentialKind::Password => form.child(
                Field::new()
                    .label(t!("credential.dialog.password"))
                    .child(self.fields.read(cx).password_input("credential-password")),
            ),
            CredentialKind::Key => self.key_fields(form, cx),
            CredentialKind::Agent => form,
        };
        v_flex()
            .gap_3()
            .w_full()
            .child(form)
            .when(kind == CredentialKind::Agent, |view| {
                view.child(
                    div()
                        .id("credential-agent-note")
                        .test_support()
                        .aria_label(agent_note())
                        .text_sm()
                        .text_color(muted)
                        .child(agent_note()),
                )
            })
            .when_some(secret_note, |view, note| {
                view.child(div().text_sm().text_color(muted).child(note))
            })
            .when(drops_kept_key, |view| {
                let note = t!("credential.dialog.drops_kept_key");
                view.child(
                    div()
                        .id("credential-kept-key-note")
                        .test_support()
                        .aria_label(note.clone())
                        .text_sm()
                        .text_color(cx.theme().warning)
                        .child(note),
                )
            })
            .when_some(self.usage_note(), |view, note| {
                view.child(
                    div()
                        .id("credential-usage")
                        .test_support()
                        .aria_label(note.clone())
                        .text_sm()
                        .text_color(muted)
                        .child(note),
                )
            })
            .when(self.saving, |view| {
                view.child(
                    div()
                        .id("credential-saving")
                        .test_support()
                        .text_sm()
                        .text_color(muted)
                        .child(t!("credential.dialog.encrypting")),
                )
            })
    }
}

/// Open a credential dialog: a new credential, one at a generated key, or
/// the edit dialog of an existing one.
pub fn open_credential_dialog(
    dialog: CredentialDialog,
    store: Entity<HostStore>,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| CredentialForm::new(dialog, store, window, cx));
    let editing = matches!(dialog, CredentialDialog::Edit(_));
    let title = if editing {
        t!("credential.dialog.title_edit")
    } else {
        t!("credential.dialog.title_new")
    };
    let commit_label = if editing {
        t!("common.save")
    } else {
        t!("credential.dialog.create")
    };
    window.open_dialog(cx, {
        let form = form.clone();
        move |dialog, _, _| {
            dialog
                .title(title.clone())
                // Closed by its buttons or Escape, not by a click beside it.
                .overlay_closable(false)
                .child(form.clone())
                .footer(commit_footer("commit", commit_label.clone()))
                .on_ok({
                    let form = form.clone();
                    move |_, window, cx| form.update(cx, |form, cx| form.commit(window, cx))
                })
                .on_close(|_, window, cx| dismiss_form_error(window, cx))
        }
    });
    // Focused in the same update that opened the dialog, which is the one
    // that sticks: a later request loses to the dialog's own focus.
    if !editing {
        form.update(cx, |form, cx| form.focus_name(window, cx));
    }
}
