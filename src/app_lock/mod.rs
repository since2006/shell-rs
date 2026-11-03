//! Application password lock. Independent of settings and workspace, with
//! one process-wide entity controlling both the UI gate and CLI access.
mod activity;
mod password;
mod password_form;
mod storage;
pub use storage::{InMemoryLockStore, KeychainLockStore, LockRecordStore};

pub use activity::activity_tracker;
use gpui_kit::*;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

use crate::i18n::t;
use password::Verifier;
pub use password_form::{UnlockScreen, open_password_dialog};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PasswordOperation {
    Enable,
    Change,
    Disable,
    Unlock,
}

enum PasswordFailure {
    Incorrect,
    Verify(String),
    Save(String),
}

impl PasswordFailure {
    fn message(self) -> SharedString {
        match self {
            Self::Incorrect => t!("app_lock.wrong_password"),
            Self::Verify(error) => t!("app_lock.failed", error = error),
            Self::Save(error) => t!("app_lock.save_failed", error = error),
        }
    }
}

pub struct AppLock {
    store: Arc<dyn LockRecordStore>,
    loading: bool,
    verifier: Option<Verifier>,
    load_error: Option<String>,
    locked: bool,
    busy: bool,
    last_activity: Option<Instant>,
    _keyboard_activity: Option<Subscription>,
}

#[derive(Clone)]
struct LockGlobal(Entity<AppLock>);
impl Global for LockGlobal {}

impl AppLock {
    /// Start locked while the keychain is checked on a worker. No host data or
    /// background CLI may be opened until this check has finished successfully.
    pub fn new(store: Arc<dyn LockRecordStore>, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            store,
            loading: false,
            verifier: None,
            load_error: None,
            locked: true,
            busy: false,
            last_activity: None,
            _keyboard_activity: None,
        };
        this.reload(cx);
        this
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    pub fn is_ready(&self) -> bool {
        !self.loading && self.load_error.is_none()
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.loading = true;
        self.busy = true;
        self.locked = true;
        self.load_error = None;
        cx.notify();
        let store = self.store.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { storage::load(store.as_ref()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                this.busy = false;
                match result {
                    Ok(verifier) => {
                        this.locked = verifier.is_some();
                        this.verifier = verifier;
                        this.record_activity(cx);
                    }
                    Err(error) => this.load_error = Some(format!("{error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn install(lock: Entity<Self>, cx: &mut App) {
        let weak = lock.downgrade();
        let subscription = cx.intercept_keystrokes(move |_, _, cx| {
            let _ = weak.update(cx, |lock, cx| lock.record_activity(cx));
        });
        lock.update(cx, |lock, cx| {
            lock._keyboard_activity = Some(subscription);
            lock.record_activity(cx);
        });
        cx.set_global(LockGlobal(lock));
    }
    pub fn global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<LockGlobal>().map(|g| g.0.clone())
    }
    pub fn is_locked(&self) -> bool {
        self.locked
    }
    pub fn is_enabled(&self) -> bool {
        self.loading || self.verifier.is_some() || self.load_error.is_some()
    }
    pub fn is_busy(&self) -> bool {
        self.busy
    }
    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    /// Input does not notify observers: activity must not redraw the app.
    pub fn record_activity(&mut self, cx: &App) {
        if !self.locked {
            self.last_activity = Some(cx.background_executor().now());
        }
    }

    pub fn is_idle(&self, timeout: Duration, cx: &App) -> bool {
        !timeout.is_zero()
            && self.is_enabled()
            && !self.locked
            && !self.busy
            && self.last_activity.is_some_and(|last| {
                cx.background_executor()
                    .now()
                    .saturating_duration_since(last)
                    >= timeout
            })
    }

    pub fn lock(&mut self, cx: &mut Context<Self>) {
        if self.is_enabled() && !self.busy {
            self.locked = true;
            cx.notify();
        }
    }

    fn submit(
        &mut self,
        operation: PasswordOperation,
        current: Zeroizing<String>,
        new: Zeroizing<String>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), SharedString>> {
        if self.busy
            || self.load_error.is_some()
            || (operation == PasswordOperation::Enable) == self.verifier.is_some()
        {
            return Task::ready(Err(t!("app_lock.unavailable")));
        }
        if operation != PasswordOperation::Unlock && self.locked {
            return Task::ready(Err(t!("app_lock.unavailable")));
        }
        if matches!(
            operation,
            PasswordOperation::Enable | PasswordOperation::Change
        ) && new.chars().count() < 8
        {
            return Task::ready(Err(t!("app_lock.too_short")));
        }
        self.busy = true;
        cx.notify();
        let verifier = self.verifier.clone();
        let store = self.store.clone();
        // Keep the service alive even if its dialog is dismissed during work.
        let service = cx.entity();
        cx.spawn(async move |_, cx| {
            let result = cx
                .background_spawn(async move {
                    if let Some(old) = &verifier {
                        match old.verify(&current) {
                            Ok(true) => {}
                            Ok(false) => return Err(PasswordFailure::Incorrect),
                            Err(error) => return Err(PasswordFailure::Verify(error.to_string())),
                        }
                    }
                    let next = match operation {
                        PasswordOperation::Enable | PasswordOperation::Change => Some(
                            Verifier::new(&new)
                                .map_err(|error| PasswordFailure::Verify(error.to_string()))?,
                        ),
                        PasswordOperation::Disable => None,
                        PasswordOperation::Unlock => verifier,
                    };
                    if operation != PasswordOperation::Unlock {
                        storage::save(store.as_ref(), next.as_ref())
                            .map_err(|error| PasswordFailure::Save(error.to_string()))?;
                    }
                    Ok(next)
                })
                .await;
            service.update(cx, |this, cx| {
                this.busy = false;
                let outcome = result.map_err(PasswordFailure::message).map(|verifier| {
                    this.verifier = verifier;
                    this.locked = false;
                    this.record_activity(cx);
                });
                cx.notify();
                outcome
            })
        })
    }
}

/// CLI and shell use the same gate; tests without an installed gate are unlocked.
pub fn is_locked(cx: &App) -> bool {
    AppLock::global(cx).is_some_and(|lock| lock.read(cx).is_locked())
}

#[cfg(test)]
mod tests {
    use super::storage::tests::FailingStore;
    use super::{AppLock, InMemoryLockStore, LockRecordStore, PasswordOperation};
    use gpui_kit::{AppContext as _, Entity, TestAppContext};
    use std::sync::{Arc, atomic::Ordering};
    use zeroize::Zeroizing;

    fn open(cx: &mut TestAppContext, store: Arc<dyn LockRecordStore>) -> Entity<AppLock> {
        let lock = cx.update(|cx| cx.new(|cx| AppLock::new(store, cx)));
        cx.run_until_parked();
        lock
    }

    fn submit(
        cx: &mut TestAppContext,
        lock: &Entity<AppLock>,
        operation: PasswordOperation,
        current: &str,
        new: &str,
    ) -> bool {
        let task = cx.update(|cx| {
            lock.update(cx, |lock, cx| {
                lock.submit(
                    operation,
                    Zeroizing::new(current.into()),
                    Zeroizing::new(new.into()),
                    cx,
                )
            })
        });
        cx.foreground_executor.block_test(task).is_ok()
    }

    #[gpui_kit::test]
    fn password_lifecycle_survives_restart_and_rejects_wrong_passwords(cx: &mut TestAppContext) {
        let store = Arc::new(InMemoryLockStore::default());
        let lock = open(cx, store.clone());
        assert!(!submit(cx, &lock, PasswordOperation::Enable, "", "short"));
        assert!(submit(
            cx,
            &lock,
            PasswordOperation::Enable,
            "",
            "first password"
        ));
        let lock = open(cx, store.clone());
        cx.update(|cx| assert!(lock.read(cx).is_locked()));
        assert!(!submit(
            cx,
            &lock,
            PasswordOperation::Unlock,
            "incorrect",
            ""
        ));
        cx.update(|cx| assert!(lock.read(cx).is_locked()));
        // Changing the verifier is impossible until the gate is open.
        assert!(!submit(
            cx,
            &lock,
            PasswordOperation::Change,
            "first password",
            "second password"
        ));
        assert!(submit(
            cx,
            &lock,
            PasswordOperation::Unlock,
            "first password",
            ""
        ));
        assert!(!submit(
            cx,
            &lock,
            PasswordOperation::Change,
            "incorrect",
            "second password"
        ));
        assert!(!submit(
            cx,
            &lock,
            PasswordOperation::Disable,
            "incorrect",
            ""
        ));
        assert!(submit(
            cx,
            &lock,
            PasswordOperation::Change,
            "first password",
            "second password"
        ));
        let lock = open(cx, store.clone());
        assert!(!submit(
            cx,
            &lock,
            PasswordOperation::Unlock,
            "first password",
            ""
        ));
        assert!(submit(
            cx,
            &lock,
            PasswordOperation::Unlock,
            "second password",
            ""
        ));
        assert!(submit(
            cx,
            &lock,
            PasswordOperation::Disable,
            "second password",
            ""
        ));
        let restarted = open(cx, store);
        cx.update(|cx| {
            assert!(!restarted.read(cx).is_enabled());
            assert!(!restarted.read(cx).is_locked());
        });
    }

    #[gpui_kit::test]
    fn failed_keychain_writes_keep_the_original_lock(cx: &mut TestAppContext) {
        let store = Arc::new(FailingStore::default());
        let lock = open(cx, store.clone());
        assert!(submit(
            cx,
            &lock,
            PasswordOperation::Enable,
            "",
            "first password"
        ));
        let saved = store.memory.read().unwrap().unwrap();
        store.fail_write.store(true, Ordering::SeqCst);
        assert!(!submit(
            cx,
            &lock,
            PasswordOperation::Disable,
            "first password",
            ""
        ));
        assert!(!submit(
            cx,
            &lock,
            PasswordOperation::Change,
            "first password",
            "second password"
        ));
        assert_eq!(
            store.memory.read().unwrap().unwrap().as_str(),
            saved.as_str()
        );
        cx.update(|cx| lock.update(cx, |lock, cx| lock.lock(cx)));
        assert!(submit(
            cx,
            &lock,
            PasswordOperation::Unlock,
            "first password",
            ""
        ));
        assert!(open(cx, store).read_with(cx, |lock, _| lock.is_locked()));
    }

    #[gpui_kit::test]
    fn loading_and_keychain_read_failures_remain_locked_until_retry(cx: &mut TestAppContext) {
        let store = Arc::new(FailingStore::default());
        store.fail_read.store(true, Ordering::SeqCst);
        let lock = cx.update(|cx| cx.new(|cx| AppLock::new(store.clone(), cx)));
        cx.update(|cx| {
            assert!(lock.read(cx).is_loading());
            assert!(lock.read(cx).is_locked());
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(!lock.read(cx).is_loading());
            assert!(lock.read(cx).is_locked());
            assert!(lock.read(cx).load_error().is_some());
        });
        assert!(!submit(
            cx,
            &lock,
            PasswordOperation::Enable,
            "",
            "new password"
        ));
        store.fail_read.store(false, Ordering::SeqCst);
        cx.update(|cx| lock.update(cx, |lock, cx| lock.reload(cx)));
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(lock.read(cx).load_error().is_none());
            assert!(!lock.read(cx).is_locked());
        });
    }
}
