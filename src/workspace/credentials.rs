//! The workspace's share of credentials: the sidebar's switch and the
//! commands of the credential list.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::{WindowExt as _, notification::Notification};
use gpui_kit::*;

use crate::app::{
    CopyCredentialPublicKey, DeleteCredential, EditCredential, GenerateCredentialKey,
    NewCredential, ShowCredentials,
};
use crate::credential::{CredentialDialog, open_credential_dialog};
use crate::host::{AuthKind, CredentialKind, read_public_key};
use crate::i18n::{t, tn};
use crate::shared::confirm_delete;

use super::{sidebar::SidebarMode, workspace_view::Workspace};

impl Workspace {
    pub(super) fn on_show_credentials(
        &mut self,
        _: &ShowCredentials,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_sidebar(SidebarMode::Credentials, window, cx);
    }

    pub(super) fn on_new_credential(
        &mut self,
        _: &NewCredential,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        open_credential_dialog(CredentialDialog::New, self.store.clone(), window, cx);
    }

    pub(super) fn on_generate_credential_key(
        &mut self,
        _: &GenerateCredentialKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        open_credential_dialog(
            CredentialDialog::GenerateKey,
            self.store.clone(),
            window,
            cx,
        );
    }

    pub(super) fn on_edit_credential(
        &mut self,
        action: &EditCredential,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.store.read(cx).credential(action.0).is_some() {
            open_credential_dialog(
                CredentialDialog::Edit(action.0),
                self.store.clone(),
                window,
                cx,
            );
        }
    }

    /// Copy the public half of a key credential's key, read from its file
    /// off the UI thread, and say whether that worked.
    pub(super) fn on_copy_credential_public_key(
        &mut self,
        action: &CopyCredentialPublicKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self
            .store
            .read(cx)
            .credential(action.0)
            .and_then(|credential| credential.key_path.clone())
        else {
            return;
        };
        cx.spawn_in(window, async move |_, cx| {
            let file = PathBuf::from(path.as_ref());
            let line = cx
                .background_executor()
                .spawn(async move { read_public_key(&file) })
                .await;
            cx.update(|window, cx| match line {
                Some(line) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(line));
                    window.push_notification(
                        Notification::success(t!("workspace.credential.public_key_copied")),
                        cx,
                    );
                }
                None => window.push_notification(
                    Notification::error(t!(
                        "workspace.credential.public_key_unreadable",
                        path = path
                    )),
                    cx,
                ),
            })
            .ok();
        })
        .detach();
    }

    /// Ask, naming the hosts that use the credential, then delete it. Those
    /// hosts keep their connections; they log in on their own next time.
    pub(super) fn on_delete_credential(
        &mut self,
        action: &DeleteCredential,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0;
        let store = self.store.read(cx);
        let Some((name, kind)) = store
            .credential(id)
            .map(|credential| (credential.name.clone(), credential.kind))
        else {
            return;
        };
        let hosts = store.hosts_using(id).count();
        let store = self.store.clone();
        confirm_delete(
            &name,
            describe_credential_delete(hosts, kind),
            Rc::new(move |_, cx| {
                store.update(cx, |store, cx| {
                    store.remove_credential(id, cx);
                });
            }),
            window,
            cx,
        );
    }
}

/// What the delete dialog says happens to the hosts using the credential.
/// `None` when no host uses it.
fn describe_credential_delete(hosts: usize, kind: CredentialKind) -> Option<SharedString> {
    (hosts > 0).then(|| match kind.without_credential() {
        AuthKind::Password => tn!("workspace.credential.in_use_then_password", hosts),
        AuthKind::NoPassword => tn!("workspace.credential.in_use_then_no_password", hosts),
    })
}

#[cfg(test)]
mod tests {
    use super::describe_credential_delete;
    use crate::host::CredentialKind;

    #[test]
    fn deleting_a_credential_says_how_many_hosts_and_what_they_become() {
        assert_eq!(
            describe_credential_delete(0, CredentialKind::Password),
            None
        );
        assert_eq!(
            describe_credential_delete(3, CredentialKind::Password).as_deref(),
            Some("有 3 台主机正在使用此凭据。删除后它们改为「密码」，连接时询问密码，用户名不变。")
        );
        assert_eq!(
            describe_credential_delete(1, CredentialKind::Agent).as_deref(),
            Some(
                "有 1 台主机正在使用此凭据。删除后它们改为「无密码」，用 SSH Agent 和 ~/.ssh 中的私钥登录，用户名不变。"
            )
        );
    }
}
