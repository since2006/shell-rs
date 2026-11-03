//! The workspace's share of 同步: its commands, what it waits for the user
//! to settle, the folder kept in the settings, and the round before
//! quitting.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::{
    Sizable as _, WindowExt as _, button::Button, dialog::DialogButtonProps,
    notification::Notification,
};
use gpui_kit::*;

use crate::app::{
    ChangeSyncLogin, ChangeSyncPassword, RemoveSyncDevice, ReviewSync, SetUpS3Sync, SetUpSync,
    SetUpSyncIn, SetUpWebDavSync, SyncNow, TurnOffSync,
};
use crate::host::ErrorMessage as _;
use crate::i18n::{t, tn};
use crate::settings::SyncLocation;
use crate::shared::confirm_danger;
use crate::sync::{
    Attention, JoinReason, PasswordDialog, RemoteDialog, RemoteKind, SyncEvent,
    open_password_dialog, open_remote_dialog, open_turn_off_dialog,
};

use super::workspace_view::Workspace;

/// Marks the notification that says sync waits for the user, so a newer
/// one replaces it.
struct SyncAttentionNotification;

impl Workspace {
    pub(super) fn on_set_up_sync(
        &mut self,
        _: &SetUpSync,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(t!("workspace.sync.choose_folder")),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(folder) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                this.set_up_sync_in(folder, window, cx)
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn on_set_up_sync_in(
        &mut self,
        action: &SetUpSyncIn,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_up_sync_in(action.0.clone(), window, cx);
    }

    /// Make sync in `folder`, or join the one there.
    fn set_up_sync_in(&mut self, folder: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let location = SyncLocation::Folder { path: folder };
        let inspect = self
            .sync
            .update(cx, |sync, cx| sync.inspect(location.clone(), None, cx));
        let sync = self.sync.clone();
        cx.spawn_in(window, async move |_, cx| {
            let found = inspect.await;
            cx.update(|window, cx| match found {
                Ok(present) => {
                    let dialog = if present {
                        PasswordDialog::Join {
                            location,
                            reason: JoinReason::First,
                        }
                    } else {
                        PasswordDialog::Create { location }
                    };
                    open_password_dialog(sync, dialog, None, window, cx);
                }
                Err(error) => window.push_notification(
                    Notification::error(error.message()).title(t!("workspace.sync.folder_failed")),
                    cx,
                ),
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn on_set_up_webdav_sync(
        &mut self,
        _: &SetUpWebDavSync,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dialog = RemoteDialog::SetUp(RemoteKind::WebDav);
        open_remote_dialog(self.sync.clone(), dialog, window, cx);
    }

    pub(super) fn on_set_up_s3_sync(
        &mut self,
        _: &SetUpS3Sync,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dialog = RemoteDialog::SetUp(RemoteKind::S3);
        open_remote_dialog(self.sync.clone(), dialog, window, cx);
    }

    pub(super) fn on_change_sync_login(
        &mut self,
        _: &ChangeSyncLogin,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_login_dialog(window, cx);
    }

    /// Sign in anew to the server or bucket sync goes through.
    fn open_login_dialog(&self, window: &mut Window, cx: &mut Context<Self>) {
        let location = self.sync.read(cx).snapshot(cx).location;
        if let Some(location) = location.filter(SyncLocation::signs_in) {
            open_remote_dialog(self.sync.clone(), RemoteDialog::Login(location), window, cx);
        }
    }

    pub(super) fn on_sync_now(&mut self, _: &SyncNow, _: &mut Window, cx: &mut Context<Self>) {
        self.sync.update(cx, |sync, cx| sync.sync_now(cx));
    }

    /// The dialog for what sync waits for.
    pub(super) fn on_review_sync(
        &mut self,
        _: &ReviewSync,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.sync.read(cx).snapshot(cx);
        let (Some(attention), Some(location)) = (snapshot.attention, snapshot.location) else {
            return;
        };
        let sync = self.sync.clone();
        match attention {
            Attention::NeedsPassword | Attention::VaultReplaced => {
                let reason = if attention == Attention::NeedsPassword {
                    JoinReason::NewPassword
                } else {
                    JoinReason::Reset
                };
                let dialog = PasswordDialog::Join { location, reason };
                open_password_dialog(sync, dialog, None, window, cx);
            }
            Attention::Removed => {
                window.open_alert_dialog(cx, move |alert, _, _| {
                    let sync = sync.clone();
                    alert
                        .title(t!("workspace.sync.removed_title"))
                        .description(t!("workspace.sync.removed"))
                        .button_props(
                            DialogButtonProps::default()
                                .ok_text(t!("workspace.sync.rejoin"))
                                .cancel_text(t!("common.cancel")),
                        )
                        .show_cancel(true)
                        .on_ok(move |_, _, cx| {
                            sync.update(cx, |sync, cx| sync.accept_removed(cx));
                            true
                        })
                });
            }
            Attention::LoginRefused => self.open_login_dialog(window, cx),
            Attention::MassDelete { count, of } => confirm_danger(
                tn!("workspace.sync.mass_delete_title", count as usize),
                Some(t!("workspace.sync.mass_delete", of = of)),
                t!("common.delete"),
                Rc::new(move |_, cx| sync.update(cx, |sync, cx| sync.allow_mass_delete(cx))),
                window,
                cx,
            ),
        }
    }

    pub(super) fn on_change_sync_password(
        &mut self,
        _: &ChangeSyncPassword,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        open_password_dialog(self.sync.clone(), PasswordDialog::Change, None, window, cx);
    }

    pub(super) fn on_turn_off_sync(
        &mut self,
        _: &TurnOffSync,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        open_turn_off_dialog(self.sync.clone(), window, cx);
    }

    pub(super) fn on_remove_sync_device(
        &mut self,
        action: &RemoveSyncDevice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.sync.read(cx).snapshot(cx);
        let Some(device) = snapshot
            .devices
            .into_iter()
            .find(|device| device.id == action.0)
        else {
            return;
        };
        let (sync, id, workspace) = (self.sync.clone(), device.id, cx.weak_entity());
        confirm_danger(
            t!("workspace.sync.remove_title", name = device.info.name),
            Some(t!("workspace.sync.remove")),
            t!("workspace.sync.remove_device"),
            Rc::new(move |window, cx| {
                let removed = sync.update(cx, |sync, cx| sync.remove_device(id.clone(), cx));
                let workspace = workspace.clone();
                window
                    .spawn(cx, async move |cx| {
                        if let Err(error) = removed.await {
                            workspace
                                .update_in(cx, |_, window, cx| {
                                    window.push_notification(
                                        Notification::error(error.message())
                                            .title(t!("workspace.sync.remove_failed")),
                                        cx,
                                    );
                                })
                                .ok();
                        }
                    })
                    .detach();
            }),
            window,
            cx,
        );
    }

    pub(super) fn on_sync_event(
        &mut self,
        event: &SyncEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SyncEvent::NeedsAttention(attention) => {
                let text = match attention {
                    Attention::NeedsPassword => t!("workspace.sync.needs_password"),
                    Attention::VaultReplaced => t!("workspace.sync.vault_replaced"),
                    Attention::Removed => t!("workspace.sync.removed_notice"),
                    Attention::LoginRefused => t!("workspace.sync.login_refused"),
                    Attention::MassDelete { count, .. } => {
                        tn!("workspace.sync.mass_delete_notice", *count as usize)
                    }
                };
                let dispatch = self.focus_handle.clone();
                window.push_notification(
                    Notification::warning(text)
                        .id::<SyncAttentionNotification>()
                        .title(t!("workspace.sync.title"))
                        .action(move |_, _, cx| {
                            let (notification, dispatch) =
                                (cx.entity().downgrade(), dispatch.clone());
                            Button::new("review-sync-notice")
                                .outline()
                                .small()
                                .label(t!("workspace.sync.review"))
                                .on_click(move |_, window, cx| {
                                    notification
                                        .update(cx, |notification, cx| {
                                            notification.dismiss(window, cx)
                                        })
                                        .ok();
                                    dispatch.dispatch_action(&ReviewSync, window, cx);
                                })
                        }),
                    cx,
                );
            }
            SyncEvent::LocationSet(location) => {
                let location = location.clone();
                self.settings().update(cx, |settings, cx| {
                    settings.update(|settings| settings.sync.location = Some(location), cx)
                });
            }
            SyncEvent::TurnedOff => {
                self.settings().update(cx, |settings, cx| {
                    settings.update(|settings| settings.sync.location = None, cx)
                });
            }
        }
    }

    /// The location from the settings.
    pub(super) fn follow_sync_settings(&self, cx: &mut App) {
        let location = self.settings().read(cx).settings().sync.location.clone();
        self.sync
            .update(cx, |sync, cx| sync.set_location(location, cx));
    }

    /// Part of the quit guard: with something not synced yet, hold the quit
    /// back (true) for a last round, then quit.
    pub(super) fn sync_before_quit(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(wait) = self.sync.update(cx, |sync, cx| sync.sync_before_quit(cx)) else {
            return false;
        };
        cx.spawn(async move |_, cx| {
            wait.await;
            cx.update(|cx| cx.quit());
        })
        .detach();
        true
    }
}
