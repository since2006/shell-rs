//! The host data as a GPUI entity: what the workspace creates once and
//! every panel and dialog observes. [`HostData`] holds the data and writes
//! it through to the database; this says what each change did, as
//! notifications and [`HostStoreEvent`]s, and runs the keychain writes on
//! the background executor.

use std::ops::{Deref, DerefMut};
use std::path::PathBuf;

use gpui_kit::{Context, EventEmitter, SharedString, Task};
use zeroize::Zeroizing;

use crate::i18n::t;
use crate::secrets::{SecretRef, SharedSecretStore};

use super::{
    ApplyError, BookmarkSide, CredentialDraft, CredentialId, ErrorMessage as _, ForwardDraft,
    ForwardId, GroupDraft, GroupId, GroupPlan, HostData, HostDatabase, HostDraft, HostEvent,
    HostId, HostNode, HostOs, NodeDrop, SavedTerminalId, SavedTerminalTab, SecretJob, SecretWork,
    SecretsWritten, SnippetCategoryId, SnippetDraft, SnippetId, SshLink, SyncExport, TerminalDrop,
    TerminalGroupId, TerminalNode,
};

/// The single source of truth for hosts and everything kept with them (see
/// [`HostData`]), shared with every panel and dialog; consumers observe it.
///
/// Its mutators take a `Context`: they change the data, which writes the
/// change through to the database, then notify observers and pass on what
/// could not be kept as `PersistFailed`. The data's `*_unnotified` variants
/// and everything that only reads are reached through `Deref`.
pub struct HostStore {
    data: HostData,
}

/// What the store tells the workspace about, beyond plain change notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostStoreEvent {
    /// A change stands in memory but could not be kept: the database or
    /// the keychain refused it. Already worded for the user.
    PersistFailed(SharedString),
    /// A port forward's rule changed in a way its running forward has to
    /// restart for.
    ForwardSettingsChanged(ForwardId),
}

impl EventEmitter<HostStoreEvent> for HostStore {}

impl Deref for HostStore {
    type Target = HostData;

    fn deref(&self) -> &HostData {
        &self.data
    }
}

impl DerefMut for HostStore {
    fn deref_mut(&mut self) -> &mut HostData {
        &mut self.data
    }
}

/// The data's mutators, each with the `cx` to settle what it did.
macro_rules! settled {
    ($(fn $name:ident($($arg:ident: $ty:ty),*) $(-> $ret:ty)?;)*) => {
        $(
            pub fn $name(&mut self, $($arg: $ty,)* cx: &mut Context<Self>) $(-> $ret)? {
                let result = self.data.$name($($arg),*);
                self.settle(cx);
                result
            }
        )*
    };
}

impl HostStore {
    pub fn new(data: HostData) -> Self {
        Self { data }
    }

    /// An empty memory-only store.
    pub fn empty() -> Self {
        Self::new(HostData::empty())
    }

    /// Read everything back from `database` and keep writing to it.
    pub fn load(database: HostDatabase) -> rusqlite::Result<Self> {
        HostData::load(database).map(Self::new)
    }

    /// Sample data for tests; see [`HostData::seed`].
    pub fn seed() -> Self {
        Self::new(HostData::seed())
    }

    /// Attach the system keychain. `main` does this once at startup; the UI
    /// tests attach an in-memory store instead.
    pub fn with_secrets(self, secrets: SharedSecretStore) -> Self {
        Self::new(self.data.with_secrets(secrets))
    }

    /// Keep pasted and generated private keys in `dir`.
    pub fn with_key_dir(self, dir: impl Into<PathBuf>) -> Self {
        Self::new(self.data.with_key_dir(dir))
    }

    /// Keep the saved terminals' output in `dir`.
    pub fn with_terminal_output_dir(self, dir: impl Into<PathBuf>) -> Self {
        Self::new(self.data.with_terminal_output_dir(dir))
    }

    /// See [`HostData::adopt_moved_keys`]; says what could not be saved.
    pub fn adopt_moved_keys(&mut self) -> Option<SharedString> {
        self.data
            .adopt_moved_keys()
            .map(|failure| failure.message())
    }

    /// Pass on what the data did: what could not be kept as
    /// `PersistFailed`, a forward to restart, and one notification for
    /// whatever changed. The keychain writes it queued go to the background.
    pub fn settle(&mut self, cx: &mut Context<Self>) {
        for job in self.data.take_secret_jobs() {
            self.run_secret_job(job, cx).detach();
        }
        let mut changed = false;
        for event in self.data.take_events() {
            match event {
                HostEvent::Failed(failure) => {
                    cx.emit(HostStoreEvent::PersistFailed(failure.message()));
                }
                HostEvent::ForwardSettingsChanged(id) => {
                    cx.emit(HostStoreEvent::ForwardSettingsChanged(id));
                }
                HostEvent::Changed => changed = true,
            }
        }
        if changed {
            cx.notify();
        }
    }

    settled! {
        fn insert_temporary(draft: HostDraft, password: Option<Zeroizing<String>>) -> HostId;
        fn remove_temporary(id: HostId);
        fn place_group(plan: GroupPlan) -> Option<GroupId>;
        fn insert(draft: HostDraft) -> HostId;
        fn update(id: HostId, draft: HostDraft) -> bool;
        fn remove(id: HostId) -> bool;
        fn insert_group(draft: GroupDraft) -> GroupId;
        fn update_group(id: GroupId, draft: GroupDraft) -> bool;
        fn add_bookmark(id: HostId, side: BookmarkSide, path: &str) -> bool;
        fn remove_bookmark(id: HostId, side: BookmarkSide, path: &str) -> bool;
        fn move_bookmark(id: HostId, side: BookmarkSide, path: &str, to: usize) -> bool;
        fn insert_forward(draft: ForwardDraft) -> Option<ForwardId>;
        fn update_forward(id: ForwardId, draft: ForwardDraft) -> bool;
        fn remove_forward(id: ForwardId) -> bool;
        fn insert_credential(draft: CredentialDraft) -> CredentialId;
        fn update_credential(id: CredentialId, draft: CredentialDraft) -> bool;
        fn remove_credential(id: CredentialId) -> Vec<HostId>;
        fn set_group_expanded(id: GroupId, expanded: bool) -> bool;
        fn set_all_groups_expanded(expanded: bool) -> bool;
        fn move_node(source: HostNode, drop: NodeDrop) -> bool;
        fn remove_group(id: GroupId) -> Vec<HostId>;
        fn record_connection(id: HostId);
        fn set_host_os(id: HostId, os: Option<HostOs>);
        fn set_host_sidebar_tool(id: HostId, tool: Option<SharedString>);
        fn save_secret(secret: SecretRef, value: Option<String>);
        fn insert_terminal_group(name: SharedString) -> TerminalGroupId;
        fn rename_terminal_group(id: TerminalGroupId, name: SharedString) -> bool;
        fn set_terminal_group_expanded(id: TerminalGroupId, expanded: bool) -> bool;
        fn remove_terminal_group(id: TerminalGroupId) -> Vec<SavedTerminalId>;
        fn set_saved_terminal_directory(id: SavedTerminalId, directory: String) -> bool;
        fn set_saved_terminal_title(id: SavedTerminalId, title: SharedString) -> bool;
        fn remove_saved_terminal(id: SavedTerminalId) -> bool;
        fn move_terminal_node(source: TerminalNode, drop: TerminalDrop) -> bool;
        fn set_saved_terminal_tabs(tabs: Vec<SavedTerminalTab>);
        fn insert_snippet(draft: SnippetDraft) -> SnippetId;
        fn update_snippet(id: SnippetId, draft: SnippetDraft) -> bool;
        fn remove_snippet(id: SnippetId) -> bool;
        fn insert_snippet_category(name: impl Into<SharedString>) -> SnippetCategoryId;
        fn rename_snippet_category(id: SnippetCategoryId, name: impl Into<SharedString>) -> bool;
        fn remove_snippet_category(id: SnippetCategoryId) -> bool;
        fn apply_sync(
            changes: &[shellrs_sync::Change],
            state: &[u8],
            export: &SyncExport
        ) -> Result<SecretWork, ApplyError>;
        fn adopt_synced_secrets(written: &SecretsWritten);
    }

    /// Connect to what an `ssh://` or `sftp://` link from outside names
    /// without saving it (外部连接); see [`HostData::insert_external`].
    pub fn insert_external(&mut self, link: SshLink, cx: &mut Context<Self>) -> HostId {
        let id = self.data.insert_external(link.draft(), link.password);
        self.settle(cx);
        id
    }

    pub fn insert_external_unnotified(&mut self, link: SshLink) -> HostId {
        self.data
            .insert_external_unnotified(link.draft(), link.password)
    }

    /// Copy a host as `<name> 副本` (`<name> copy`), placed right after the
    /// original. The name is the copy's own from then on, in the language it
    /// was made in.
    pub fn duplicate(&mut self, id: HostId, cx: &mut Context<Self>) -> Option<HostId> {
        let name = t!("host.store.copy_name", name = self.host(id)?.name);
        let copy = self.data.duplicate(id, name);
        self.settle(cx);
        copy
    }

    /// Keep a new terminal in `group`, or at the top, named after the group
    /// (`shellrs 2`, or `终端 2` at the top) and starting in `directory`.
    /// `None` when the group is gone.
    pub fn insert_saved_terminal(
        &mut self,
        group: Option<TerminalGroupId>,
        directory: Option<String>,
        cx: &mut Context<Self>,
    ) -> Option<SavedTerminalId> {
        let id = self
            .data
            .insert_saved_terminal(group, directory, &t!("host.saved_terminal.name"));
        self.settle(cx);
        id
    }

    pub fn insert_saved_terminal_unnotified(
        &mut self,
        group: Option<TerminalGroupId>,
        directory: Option<String>,
    ) -> Option<SavedTerminalId> {
        self.data.insert_saved_terminal_unnotified(
            group,
            directory,
            &t!("host.saved_terminal.name"),
        )
    }

    /// Name a terminal by hand, or with `None` give it a name of its group's
    /// again (`shellrs 3`), which gives way to the titles of its tab.
    pub fn rename_saved_terminal(
        &mut self,
        id: SavedTerminalId,
        name: Option<SharedString>,
        cx: &mut Context<Self>,
    ) -> bool {
        let renamed = self
            .data
            .rename_saved_terminal(id, name, &t!("host.saved_terminal.name"));
        self.settle(cx);
        renamed
    }

    pub fn rename_saved_terminal_unnotified(
        &mut self,
        id: SavedTerminalId,
        name: Option<SharedString>,
    ) -> bool {
        self.data
            .rename_saved_terminal_unnotified(id, name, &t!("host.saved_terminal.name"))
    }

    /// [`HostData::save_secret`], for a caller that has to know how it went:
    /// the failure, already reported as `PersistFailed`.
    pub fn write_secret(
        &mut self,
        secret: SecretRef,
        value: Option<String>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), SharedString>> {
        let job = self.data.secret_job(secret, value);
        self.run_secret_job(job, cx)
    }

    /// Run a keychain write on the background executor, and settle how it
    /// went.
    fn run_secret_job(
        &self,
        job: SecretJob,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), SharedString>> {
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { job.run() })
                .await;
            let failed = outcome.failure().map(|failure| failure.message());
            this.update(cx, |store, cx| {
                store.data.secret_done(outcome);
                store.settle(cx);
            })
            .ok();
            failed.map_or(Ok(()), Err)
        })
    }

    /// Run `change` with every database write it makes in one transaction
    /// (see [`HostData::begin_batch`]). Nested calls join the outer one.
    pub fn in_one_transaction<R>(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut Self, &mut Context<Self>) -> R,
    ) -> R {
        if !self.data.begin_batch() {
            return change(self, cx);
        }
        let result = change(self, cx);
        self.data.end_batch();
        self.settle(cx);
        result
    }

    /// Run `change` and say what of it could not be written to the
    /// database (or a key file deleted), for a caller that answers someone
    /// who cannot see the notification: the external CLI. The failures are
    /// reported as `PersistFailed` all the same, and the change stands in
    /// memory as it always does. Keychain writes are not among them: they
    /// finish later, and [`Self::write_secret`] says how.
    pub fn capture_failures<R>(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut Self, &mut Context<Self>) -> R,
    ) -> (R, Vec<SharedString>) {
        let outer = self.data.begin_capture();
        let result = change(self, cx);
        let failures = self.data.end_capture(outer);
        (
            result,
            failures.iter().map(|failure| failure.message()).collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::AuthKind;

    fn draft(name: &str, group: Option<GroupId>) -> HostDraft {
        HostDraft::new(name, "10.0.0.1", 22, "root", AuthKind::Password, group)
    }

    #[gpui_kit::test]
    fn one_transaction_keeps_every_write_and_reports_a_repeated_failure_once(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use gpui_kit::AppContext as _;
        use std::{cell::RefCell, rc::Rc};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shellrs.db");
        let store = cx.new(|_| HostStore::load(HostDatabase::open(&path).unwrap()).unwrap());
        let events = Rc::new(RefCell::new(Vec::new()));
        let _subscription = cx.update(|cx| {
            let events = events.clone();
            cx.subscribe(&store, move |_, event: &HostStoreEvent, _| {
                events.borrow_mut().push(event.clone());
            })
        });

        let (_, failures) = store.update(cx, |store, cx| {
            store.capture_failures(cx, |store, cx| {
                store.in_one_transaction(cx, |store, cx| {
                    let group = store.insert_group(GroupDraft::new("生产", None), cx);
                    store.insert(draft("web", Some(group)), cx);
                    // Nested: joins the outer transaction.
                    store.in_one_transaction(cx, |store, cx| store.insert(draft("db", None), cx));
                    // Three hosts in a group the database never heard of.
                    for name in ["a", "b", "c"] {
                        store.insert(draft(name, Some(GroupId(99))), cx);
                    }
                })
            })
        });
        assert_eq!(failures.len(), 3, "{failures:?}");
        cx.run_until_parked();
        assert_eq!(
            *events.borrow(),
            [HostStoreEvent::PersistFailed(failures[0].clone())]
        );

        let data = HostDatabase::open(&path).unwrap().load().unwrap();
        let names: Vec<&str> = data.hosts.iter().map(|host| host.name.as_ref()).collect();
        assert_eq!(names, ["web", "db"]);
        assert_eq!(data.groups.len(), 1);
    }

    #[gpui_kit::test]
    fn a_change_says_what_could_not_be_kept_and_still_reports_it(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use gpui_kit::AppContext as _;
        use std::{cell::RefCell, rc::Rc};

        let database = HostDatabase::in_memory().unwrap();
        let store = cx.new(|_| HostStore::load(database).unwrap());
        let events = Rc::new(RefCell::new(Vec::new()));
        let _subscription = cx.update(|cx| {
            let events = events.clone();
            cx.subscribe(&store, move |_, event: &HostStoreEvent, _| {
                events.borrow_mut().push(event.clone());
            })
        });

        // A group the database has never heard of: the write fails, the
        // host stands in memory.
        let ((id, kept), failures) = store.update(cx, |store, cx| {
            store.capture_failures(cx, |store, cx| {
                let kept = store.insert(draft("web", None), cx);
                (store.insert(draft("db", Some(GroupId(99))), cx), kept)
            })
        });
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].starts_with("新建主机未能保存到本地数据库"));
        cx.run_until_parked();
        assert_eq!(
            *events.borrow(),
            [HostStoreEvent::PersistFailed(failures[0].clone())]
        );
        store.read_with(cx, |store, _| {
            assert!(store.host(id).is_some() && store.host(kept).is_some());
        });
        // Nothing left listening afterwards.
        let (_, failures) = store.update(cx, |store, cx| {
            store.capture_failures(cx, |store, cx| store.remove(kept, cx))
        });
        assert!(failures.is_empty());

        // A keychain that keeps nothing: the write says so when it is done.
        let secret = SecretRef::password("root", "10.0.0.1", 22);
        let write = store.update(cx, |store, cx| {
            store.write_secret(secret, Some("hunter2".into()), cx)
        });
        let failure = cx.foreground_executor.block_test(write).unwrap_err();
        assert!(failure.starts_with("密码未能写入系统钥匙串"), "{failure}");
    }

    #[gpui_kit::test]
    fn a_copy_is_named_after_the_original_in_the_language_it_was_made_in(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use gpui_kit::AppContext as _;

        crate::i18n::isolate_thread();
        let store = cx.new(|_| HostStore::seed());
        let copy = store.update(cx, |store, cx| {
            let web01 = store.hosts()[0].id;
            store.duplicate(web01, cx).unwrap()
        });
        crate::i18n::set_locale("en");
        let english = store.update(cx, |store, cx| {
            let web01 = store.hosts()[0].id;
            store.duplicate(web01, cx).unwrap()
        });
        store.read_with(cx, |store, _| {
            assert_eq!(store.host(copy).unwrap().name.as_ref(), "web-01 副本");
            assert_eq!(store.host(english).unwrap().name.as_ref(), "web-01 copy");
        });
    }
}
