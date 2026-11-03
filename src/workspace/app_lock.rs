use super::Workspace;
use crate::app::{ChangeAppLockPassword, DisableAppLock, EnableAppLock, LockApp};
use crate::app_lock::{AppLock, PasswordOperation, open_password_dialog};
use gpui_kit::*;
use std::time::Duration;

impl Workspace {
    fn configure_app_lock(
        &self,
        operation: PasswordOperation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(lock) = AppLock::global(cx) {
            open_password_dialog(lock, operation, window, cx);
        }
    }
    pub(super) fn on_enable_app_lock(
        &mut self,
        _: &EnableAppLock,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.configure_app_lock(PasswordOperation::Enable, window, cx);
    }
    pub(super) fn on_change_app_lock_password(
        &mut self,
        _: &ChangeAppLockPassword,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.configure_app_lock(PasswordOperation::Change, window, cx);
    }
    pub(super) fn on_disable_app_lock(
        &mut self,
        _: &DisableAppLock,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.configure_app_lock(PasswordOperation::Disable, window, cx);
    }
    pub(super) fn on_lock_app(&mut self, _: &LockApp, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(lock) = AppLock::global(cx) {
            self.lock_app(&lock, cx);
        }
    }

    fn lock_app(&mut self, lock: &Entity<AppLock>, cx: &mut Context<Self>) {
        if !lock.read(cx).is_enabled() || lock.read(cx).is_busy() {
            return;
        }
        for explorer in self.explorers.values() {
            explorer.update(cx, |explorer, cx| explorer.close_find(cx));
        }
        lock.update(cx, |lock, cx| lock.lock(cx));
    }

    pub(super) fn watch_idle_lock(
        lock: Entity<AppLock>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this
                    .update(cx, |this, cx| {
                        let minutes = this.settings().read(cx).settings().app_lock_idle_minutes;
                        if lock
                            .read(cx)
                            .is_idle(Duration::from_secs(u64::from(minutes) * 60), cx)
                        {
                            this.lock_app(&lock, cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }
}
