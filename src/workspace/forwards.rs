//! The workspace's share of port forwarding: the sidebar's switch, the
//! commands of the forward list, and what a running forward reports.

use std::rc::Rc;

use gpui_kit::component::{WindowExt as _, dock::DockPlacement, notification::Notification};
use gpui_kit::*;

use crate::app::{
    DeleteForward, EditForward, NewForward, ShowForwards, ShowHosts, StartForward, StopForward,
};
use crate::forward::{ForwardManagerEvent, open_forward_dialog};
use crate::host::ForwardId;
use crate::shared::confirm_delete;

use super::{
    sidebar::SidebarMode,
    workspace_view::{PromptOwner, Workspace},
};

impl Workspace {
    /// What the left dock is showing, or `None` while it is hidden.
    pub(super) fn sidebar_showing(&self, cx: &App) -> Option<SidebarMode> {
        self.dock_area
            .read(cx)
            .is_dock_open(DockPlacement::Left)
            .then(|| self.sidebar.read(cx).mode())
    }

    /// Show one of the sidebar's lists, opening the sidebar if it is hidden.
    pub(super) fn show_sidebar(
        &mut self,
        mode: SidebarMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.dock_area.read(cx).is_dock_open(DockPlacement::Left) {
            self.dock_area.update(cx, |area, cx| {
                area.toggle_dock(DockPlacement::Left, window, cx);
            });
        }
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.show(mode, window, cx));
        // The title bar marks the list being shown.
        cx.notify();
    }

    pub(super) fn on_show_hosts(
        &mut self,
        _: &ShowHosts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_sidebar(SidebarMode::Hosts, window, cx);
    }

    pub(super) fn on_show_forwards(
        &mut self,
        _: &ShowForwards,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_sidebar(SidebarMode::Forwards, window, cx);
    }

    pub(super) fn on_new_forward(
        &mut self,
        _: &NewForward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = self.store.read(cx);
        if store.hosts().is_empty() {
            window.push_notification(
                Notification::warning("端口转发经由一台主机的 SSH 连接，请先新建主机。"),
                cx,
            );
            return;
        }
        open_forward_dialog(None, false, self.store.clone(), window, cx);
    }

    pub(super) fn on_edit_forward(
        &mut self,
        action: &EditForward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0;
        if self.store.read(cx).forward(id).is_none() {
            return;
        }
        let active = self.forwards.read(cx).is_active(id);
        open_forward_dialog(Some(id), active, self.store.clone(), window, cx);
    }

    pub(super) fn on_delete_forward(
        &mut self,
        action: &DeleteForward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0;
        let Some(title) = self.store.read(cx).forward(id).map(|rule| rule.title()) else {
            return;
        };
        let description = self
            .forwards
            .read(cx)
            .is_active(id)
            .then(|| SharedString::from("这条转发正在运行，会先停止。"));
        let store = self.store.clone();
        confirm_delete(
            &title,
            description,
            // The manager stops a rule that leaves the store.
            Rc::new(move |_, cx| {
                store.update(cx, |store, cx| {
                    store.remove_forward(id, cx);
                });
            }),
            window,
            cx,
        );
    }

    pub(super) fn on_start_forward(
        &mut self,
        action: &StartForward,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.forwards
            .update(cx, |forwards, cx| forwards.start(action.0, cx));
    }

    pub(super) fn on_stop_forward(
        &mut self,
        action: &StopForward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cancel_forward_prompts(action.0, None, window, cx);
        self.forwards
            .update(cx, |forwards, cx| forwards.stop(action.0, cx));
    }

    pub(super) fn on_forward_event(
        &mut self,
        event: &ForwardManagerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ForwardManagerEvent::StatusChanged(id) => {
                // A question of a run that is over has nobody to answer to.
                let current = self.forwards.read(cx).generation(*id);
                self.cancel_forward_prompts(*id, current, window, cx);
                // The title bar counts the forwards that are running.
                cx.notify();
            }
            ForwardManagerEvent::PromptRequested(id, host, generation, prompt) => self
                .enqueue_prompt(
                    PromptOwner::Forward(*id, *generation),
                    *host,
                    prompt.clone(),
                    window,
                    cx,
                ),
            // The list may not be showing, and a forward that stopped on its
            // own is something the user was relying on.
            ForwardManagerEvent::Failed(id, reason) => {
                let title = self
                    .store
                    .read(cx)
                    .forward(*id)
                    .map(|rule| rule.title())
                    .unwrap_or_default();
                window.push_notification(
                    Notification::error(reason.clone()).title(format!("端口转发“{title}”已停止")),
                    cx,
                );
            }
        }
    }

    /// Drop the questions of a forward's runs other than `keep`: all of
    /// them when it is `None`.
    fn cancel_forward_prompts(
        &mut self,
        id: ForwardId,
        keep: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stale = |owner: &PromptOwner| {
            matches!(owner, PromptOwner::Forward(forward, generation)
                if *forward == id && Some(*generation) != keep)
        };
        self.prompt_queue.retain(|(owner, _, _)| !stale(owner));
        if let Some((owner, _, _)) = self.active_prompt
            && stale(&owner)
        {
            self.cancel_prompts_for_owner(owner, window, cx);
        }
    }

    /// Who is asking, for a question that a forward raised: unlike a
    /// terminal or an SFTP tab it may have started on its own, with nothing
    /// on screen to say which connection the dialog is about.
    pub(super) fn forward_prompt_origin(&self, id: ForwardId, cx: &App) -> Option<String> {
        let store = self.store.read(cx);
        let rule = store.forward(id)?;
        let host = store.host(rule.host)?;
        Some(format!(
            "端口转发“{}”正在通过 {} 连接。",
            rule.title(),
            host.name
        ))
    }
}
