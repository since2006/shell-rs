//! 目录跟随: the right sidebar's 文件 going where the terminal's shell says
//! it is (OSC 7, see `terminal::ReportedDirectory`).
//!
//! When 文件 shows a terminal, a check run on the terminal's connection
//! learns the host's name, which the reports must carry to count, and
//! whether the login shell reports at all. If it does not, and the setting
//! says to, the user is asked once per connection whether to set it up:
//! a few lines appended to the shell's startup file, then sourced in the
//! terminal so the shell running there reports at once.

use gpui_kit::component::{WindowExt as _, dialog::DialogButtonProps, notification::Notification};
use gpui_kit::*;

use crate::app::{InstallDirectoryFollow, ToggleDirectoryFollow, ToolKind};
use crate::i18n::t;
use crate::terminal::{
    ExecResult, FollowCheck, RemoteTerminalId, ReportedDirectory, TerminalLifecycle, TerminalView,
    directory_check_command, directory_install_command, directory_installed, exec_answer,
    parse_directory_check,
};

use super::Workspace;

/// What 目录跟随 knows about one connection of a terminal: forgotten when
/// the terminal connects again, which may be to another machine.
pub(super) struct FollowState {
    generation: u64,
    check: Check,
    /// 「启用目录跟随？」 was asked on this connection.
    asked: bool,
}

enum Check {
    Running,
    Done(FollowCheck),
    /// No answer that reads as one: no `sh` there, or the host refused.
    Failed,
}

impl Workspace {
    fn terminal_view(&self, terminal: RemoteTerminalId, cx: &App) -> Option<Entity<TerminalView>> {
        Some(self.terminals.get(&terminal)?.read(cx).terminal().clone())
    }

    /// The terminal's state for the connection it is on now.
    fn follow_state(&self, terminal: RemoteTerminalId, cx: &App) -> Option<&FollowState> {
        let generation = self.terminal_view(terminal, cx)?.read(cx).generation(cx);
        self.directory_follow
            .get(&terminal)
            .filter(|state| state.generation == generation)
    }

    /// What the check on this connection found, once it has.
    fn follow_check(&self, terminal: RemoteTerminalId, cx: &App) -> Option<&FollowCheck> {
        match &self.follow_state(terminal, cx)?.check {
            Check::Done(check) => Some(check),
            _ => None,
        }
    }

    /// Whether the terminal's 文件 follows: its switch, or what the switch
    /// starts at for one not made yet.
    fn follows(&self, terminal: RemoteTerminalId, cx: &App) -> bool {
        match self.file_sidebar(terminal) {
            Some(panel) => panel.read(cx).follows(),
            None => self.initial_follow(terminal, cx),
        }
    }

    /// What the 目录跟随 switch of a terminal's new 文件 starts at: as the
    /// user last switched it on the terminal's host, else the setting.
    pub(super) fn initial_follow(&self, terminal: RemoteTerminalId, cx: &App) -> bool {
        self.terminals
            .get(&terminal)
            .and_then(|panel| self.store.read(cx).host(panel.read(cx).host().id))
            .and_then(|host| host.directory_follow)
            .unwrap_or_else(|| self.directory_follow_setting(cx))
    }

    pub(super) fn directory_follow_setting(&self, cx: &App) -> bool {
        self.settings()
            .read(cx)
            .settings()
            .file_sidebar
            .detect_directory_follow
    }

    /// Learn, once per connection, what the terminal's shell is and whether
    /// it reports its folder; then offer to set it up if it does not. Run
    /// as 文件 shows the terminal, as the terminal connects again while it
    /// does, and as 目录跟随 is turned on.
    pub(super) fn check_directory_follow(
        &mut self,
        terminal: RemoteTerminalId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.follows(terminal, cx) {
            return;
        }
        match self.follow_state(terminal, cx).map(|state| &state.check) {
            Some(Check::Done(_)) => return self.offer_directory_follow_soon(terminal, window, cx),
            Some(Check::Running | Check::Failed) => return,
            None => {}
        }
        let Some(view) = self.terminal_view(terminal, cx) else {
            return;
        };
        let generation = view.read(cx).generation(cx);
        // Not running: the next connection asks again.
        let Some(reply) = view.read(cx).exec(directory_check_command(), cx) else {
            return;
        };
        self.directory_follow.insert(
            terminal,
            FollowState {
                generation,
                check: Check::Running,
                asked: false,
            },
        );
        cx.spawn_in(window, async move |this, cx| {
            let answer = exec_answer(reply, cx).await;
            this.update_in(cx, |this, window, cx| {
                this.directory_checked(terminal, generation, answer, window, cx)
            })
            .ok();
        })
        .detach();
    }

    fn directory_checked(
        &mut self,
        terminal: RemoteTerminalId,
        generation: u64,
        answer: Option<ExecResult>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self
            .directory_follow
            .get_mut(&terminal)
            .filter(|state| state.generation == generation)
        else {
            return;
        };
        state.check = match answer
            .and_then(Result::ok)
            .as_deref()
            .and_then(parse_directory_check)
        {
            Some(check) => Check::Done(check),
            None => Check::Failed,
        };
        // The host's name is known now: what the shell reported counts.
        self.follow_current_directory(terminal, window, cx);
        self.offer_directory_follow_soon(terminal, window, cx);
    }

    fn offer_directory_follow_soon(
        &mut self,
        terminal: RemoteTerminalId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.defer_in(window, move |this, window, cx| {
            this.offer_directory_follow(terminal, window, cx)
        });
    }

    /// 「启用目录跟随？」, once per connection, while 文件 shows the terminal
    /// and its shell has not reported a folder from this host.
    fn offer_directory_follow(
        &mut self,
        terminal: RemoteTerminalId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.directory_follow_setting(cx)
            || !self.follows(terminal, cx)
            || self.tool_showing(cx) != Some(ToolKind::Files)
            || self.tool_terminal(cx).map(|shown| shown.id) != Some(terminal)
            || window.has_active_dialog(cx)
            || self
                .follow_state(terminal, cx)
                .is_none_or(|state| state.asked)
        {
            return;
        }
        let Some(check) = self.follow_check(terminal, cx) else {
            return;
        };
        let Some(setup) = check.setup else {
            return;
        };
        let Some(view) = self.terminal_view(terminal, cx) else {
            return;
        };
        let reports = view
            .read(cx)
            .directory(cx)
            .is_some_and(|directory| directory.is_from(Some(&check.hostname)));
        if reports || view.read(cx).lifecycle(cx) != TerminalLifecycle::Running {
            return;
        }
        if let Some(state) = self.directory_follow.get_mut(&terminal) {
            state.asked = true;
        }
        let description = t!(
            "tools.follow.description",
            shell = setup.shell_name(),
            file = setup.file_label()
        );
        let dispatch = self.focus_handle.clone();
        let focus = window.focused(cx);
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let restore = {
                let focus = focus.clone();
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    if let Some(focus) = &focus {
                        window.focus(focus, cx);
                    }
                    true
                }
            };
            dialog
                .title(t!("tools.follow.title"))
                .description(description.clone())
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(t!("tools.follow.set_up"))
                        .cancel_text(t!("common.cancel")),
                )
                .show_cancel(true)
                .on_cancel(restore.clone())
                .on_ok({
                    let (dispatch, restore) = (dispatch.clone(), restore.clone());
                    move |event, window, cx| {
                        let dispatch = dispatch.clone();
                        window.defer(cx, move |window, cx| {
                            dispatch.dispatch_action(&InstallDirectoryFollow(terminal), window, cx)
                        });
                        restore(event, window, cx)
                    }
                })
        });
    }

    /// 自动配置: append the setup to the startup file, then have the shell
    /// in the terminal read the file again.
    pub(super) fn on_install_directory_follow(
        &mut self,
        action: &InstallDirectoryFollow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let terminal = action.0;
        let Some(setup) = self
            .follow_check(terminal, cx)
            .and_then(|check| check.setup)
        else {
            return;
        };
        let Some(view) = self.terminal_view(terminal, cx) else {
            return;
        };
        let generation = view.read(cx).generation(cx);
        let Some(reply) = view.read(cx).exec(directory_install_command(setup), cx) else {
            let error = t!("tools.not_connected");
            window.push_notification(
                Notification::error(t!("tools.follow.failed", error = error)),
                cx,
            );
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let result = match exec_answer(reply, cx).await {
                None => Err(t!("tools.not_connected").to_string()),
                Some(Err(error)) => Err(error),
                Some(Ok(output)) => directory_installed(&output),
            };
            this.update_in(cx, |this, window, cx| {
                let file = setup.file_label();
                let notification = match result {
                    Err(error) => Notification::error(t!("tools.follow.failed", error = error)),
                    Ok(()) => {
                        if let Some(Check::Done(check)) = this
                            .directory_follow
                            .get_mut(&terminal)
                            .filter(|state| state.generation == generation)
                            .map(|state| &mut state.check)
                        {
                            check.setup = None;
                        }
                        // The shell there reads it again, unless it is
                        // another connection's by now or busy full screen.
                        let sourced = this.terminal_view(terminal, cx).is_some_and(|view| {
                            view.read(cx).generation(cx) == generation
                                && view
                                    .update(cx, |view, cx| {
                                        view.enter_command(&setup.source_line(), true, true, cx)
                                    })
                                    .is_ok()
                        });
                        if sourced {
                            Notification::success(t!("tools.follow.done", file = file))
                        } else {
                            Notification::info(t!("tools.follow.done_reconnect", file = file))
                        }
                    }
                };
                window.push_notification(notification, cx);
            })
            .ok();
        })
        .detach();
    }

    /// 目录跟随 in a terminal's 文件, switched by the user: kept on the host,
    /// for the host's next 文件, this launch and the next. Those open now
    /// keep their own.
    pub(super) fn on_toggle_directory_follow(
        &mut self,
        action: &ToggleDirectoryFollow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self.sidebar_terminal(action.0) else {
            return;
        };
        let Some(panel) = self.explorers.get(&action.0).cloned() else {
            return;
        };
        let follow = !panel.read(cx).follows();
        panel.update(cx, |panel, cx| panel.set_follow(follow, cx));
        let host = panel.read(cx).host_id();
        self.store.update(cx, |store, cx| {
            store.set_host_directory_follow(host, follow, cx)
        });
        if follow {
            // Learns the host's name if it is not known yet. A question
            // already asked on this connection is not asked again.
            self.check_directory_follow(terminal, window, cx);
            self.follow_current_directory(terminal, window, cx);
        }
    }

    /// Take the terminal's 文件 to the folder its shell reported last.
    pub(super) fn follow_current_directory(
        &mut self,
        terminal: RemoteTerminalId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let directory = self
            .terminal_view(terminal, cx)
            .and_then(|view| view.read(cx).directory(cx));
        if let Some(directory) = directory {
            self.follow_directory(terminal, &directory, window, cx);
        }
    }

    /// The terminal's shell reported a folder: its 文件 goes there, if it
    /// follows and the report comes from the host it is connected to, not
    /// one ssh'd to inside the terminal.
    pub(super) fn follow_directory(
        &mut self,
        terminal: RemoteTerminalId,
        directory: &ReportedDirectory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.file_sidebar(terminal).cloned() else {
            return;
        };
        let hostname = self
            .follow_check(terminal, cx)
            .map(|check| check.hostname.as_str());
        if !panel.read(cx).follows() || !directory.is_from(hostname) {
            return;
        }
        let path = directory.path.clone();
        panel.update(cx, |panel, cx| panel.follow_directory(path, window, cx));
    }
}
