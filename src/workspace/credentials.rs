//! The workspace's share of credentials: the sidebar's switch and the
//! commands of the credential list.

use std::rc::Rc;

use gpui_kit::*;

use crate::app::{DeleteCredential, EditCredential, NewCredential, ShowCredentials};
use crate::credential::open_credential_dialog;
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
        open_credential_dialog(None, self.store.clone(), window, cx);
    }

    pub(super) fn on_edit_credential(
        &mut self,
        action: &EditCredential,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.store.read(cx).credential(action.0).is_some() {
            open_credential_dialog(Some(action.0), self.store.clone(), window, cx);
        }
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
        let Some(name) = store
            .credential(id)
            .map(|credential| credential.name.clone())
        else {
            return;
        };
        let hosts = store.sessions_using(id).count();
        let store = self.store.clone();
        confirm_delete(
            &name,
            describe_credential_delete(hosts),
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
fn describe_credential_delete(hosts: usize) -> Option<SharedString> {
    (hosts > 0).then(|| {
        format!(
            "有 {hosts} 台主机正在使用此凭据。删除后它们改为手动输入，认证类型为「自动」，用户名不变。"
        )
        .into()
    })
}

#[cfg(test)]
mod tests {
    use super::describe_credential_delete;

    #[test]
    fn deleting_a_credential_says_how_many_hosts_and_what_they_become() {
        assert_eq!(describe_credential_delete(0), None);
        assert_eq!(
            describe_credential_delete(3).as_deref(),
            Some(
                "有 3 台主机正在使用此凭据。删除后它们改为手动输入，认证类型为「自动」，用户名不变。"
            )
        );
    }
}
