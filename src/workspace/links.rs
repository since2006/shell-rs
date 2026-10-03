//! The workspace's share of the links ShellRS is opened with (临时连接):
//! a bastion host runs `ShellRS ssh://user@host:port`, the way it runs
//! Xshell, and ShellRS connects without saving the host. The host goes with
//! its last tab.

use gpui_kit::component::{WindowExt as _, notification::Notification};
use gpui_kit::*;

use crate::cli::OpenLink;
use crate::host::{HostId, SshLink};

use super::workspace_view::Workspace;

impl Workspace {
    /// Connect to what `link` names in a terminal tab of its own, or say
    /// what is wrong with it.
    pub fn open_link(&mut self, link: OpenLink, window: &mut Window, cx: &mut Context<Self>) {
        match SshLink::parse(&link.url, link.tab.as_deref()) {
            Ok(link) => {
                let host = self
                    .store
                    .update(cx, |store, cx| store.insert_transient(link, cx));
                self.connect_host(host, window, cx);
            }
            Err(problem) => window
                .push_notification(Notification::error(problem).title("无法打开 SSH 链接"), cx),
        }
    }

    /// Forget a host opened from a link once its last tab has closed,
    /// with whatever it was still asking.
    pub(super) fn forget_unused_link_host(
        &mut self,
        host: HostId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.store.read(cx).is_transient(host) || self.has_tabs(host, cx) {
            return;
        }
        self.cancel_prompts_for_host(host, window, cx);
        self.store
            .update(cx, |store, cx| store.remove_transient(host, cx));
    }
}

/// Open the link ShellRS was started with, once the window is there: an
/// error notification needs the window's `Root`, which is only in place
/// after the closure building the window returns (see
/// [`super::notify_once_open`]).
pub fn open_link_once_open(
    workspace: &Entity<Workspace>,
    link: OpenLink,
    window: &mut Window,
    cx: &mut App,
) {
    let workspace = workspace.downgrade();
    window.defer(cx, move |window, cx| {
        workspace
            .update(cx, |workspace, cx| workspace.open_link(link, window, cx))
            .ok();
    });
}
