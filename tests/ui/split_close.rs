//! Closing a split must keep commands and input usable without a repair click.

use crate::support::*;
use gpui_kit::Focusable as _;
use gpui_kit::component::{
    Placement,
    dock::{DockPlacement, InsertTarget, PanelId},
};
use shellrs::app::CloseActiveTab;

fn split_seeded_terminals(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    workspace: &Entity<Workspace>,
) {
    in_frame(cx, handle, |window, cx| {
        window.activate_window();
        let panel = PanelId::from(
            workspace
                .read(cx)
                .remote_terminal(RemoteTerminalId(INITIAL_STAGING_TERMINAL))
                .unwrap()
                .entity_id(),
        );
        let area = workspace.read(cx).dock_area().clone();
        area.update(cx, |area, cx| {
            let node = area
                .layout(DockPlacement::Center)
                .unwrap()
                .find_panel_node(panel)
                .unwrap();
            area.move_panel(
                panel,
                InsertTarget::Split {
                    node,
                    placement: Placement::Right,
                    size: None,
                },
                window,
                cx,
            );
        });
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", INITIAL_STAGING_TERMINAL), cx)
    });
}

#[gpui_kit::test]
fn closing_the_last_tab_of_a_split_allows_the_next_close_click(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    split_seeded_terminals(cx, handle, &workspace);
    in_frame(cx, handle, |window, cx| {
        window.click(("close-terminal", INITIAL_STAGING_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert!(
            window
                .try_find(("close-terminal", INITIAL_STAGING_TERMINAL))
                .is_none()
        );
        window.click(("close-terminal", INITIAL_WEB_TERMINAL), cx);
    });
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .try_find(("close-terminal", INITIAL_WEB_TERMINAL))
                .is_none()
        );
        assert_eq!(window.find("recent-hosts").focused(), Some(true));
    });
}

#[gpui_kit::test]
fn closing_a_split_hands_focus_to_a_surviving_terminal(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    split_seeded_terminals(cx, handle, &workspace);
    in_frame(cx, handle, |window, cx| {
        window.click(("close-terminal", INITIAL_STAGING_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            workspace.read(cx).active_tab(),
            Some(CenterTab::Terminal(RemoteTerminalId(INITIAL_WEB_TERMINAL)))
        );
        let panel = workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(INITIAL_WEB_TERMINAL))
            .unwrap();
        assert!(panel.read(cx).focus_handle(cx).contains_focused(window, cx));
        window.dispatch_action(Box::new(CloseActiveTab), cx);
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.find("recent-hosts").visible())
    });
}

#[gpui_kit::test]
fn closing_a_background_split_preserves_the_focused_terminal(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    split_seeded_terminals(cx, handle, &workspace);
    in_frame(cx, handle, |window, cx| {
        window.click(("close-terminal", INITIAL_WEB_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            workspace.read(cx).active_tab(),
            Some(CenterTab::Terminal(RemoteTerminalId(
                INITIAL_STAGING_TERMINAL
            )))
        );
        let panel = workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(INITIAL_STAGING_TERMINAL))
            .unwrap();
        assert!(panel.read(cx).focus_handle(cx).contains_focused(window, cx));
    });
}

#[gpui_kit::test]
fn close_title_commands_work_without_a_focused_descendant(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    split_seeded_terminals(cx, handle, &workspace);
    in_frame(cx, handle, |window, cx| {
        window.blur(cx);
        window.click(("close-terminal", INITIAL_STAGING_TERMINAL), cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(
            window
                .try_find(("close-terminal", INITIAL_STAGING_TERMINAL))
                .is_none()
        );
        window.blur(cx);
        let position = window
            .find(("terminal-tab", INITIAL_WEB_TERMINAL))
            .bounds()
            .center();
        button_click_at(
            window,
            position,
            MouseButton::Middle,
            gpui_kit::Modifiers::default(),
            cx,
        );
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.find("recent-hosts").visible())
    });
}

#[gpui_kit::test]
async fn input_reaches_the_surviving_terminal_without_an_extra_click(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) =
        open_workspace_with_remote_factory(cx, HostStore::seed(), factory.clone());
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        [INITIAL_WEB_TERMINAL, INITIAL_STAGING_TERMINAL]
            .iter()
            .all(|id| {
                workspace
                    .read(cx)
                    .remote_terminal(RemoteTerminalId(*id))
                    .unwrap()
                    .read(cx)
                    .terminal()
                    .read(cx)
                    .lifecycle(cx)
                    == TerminalLifecycle::Running
            })
    })
    .await;
    split_seeded_terminals(cx, handle, &workspace);
    in_frame(cx, handle, |window, cx| {
        window.click(("close-terminal", INITIAL_STAGING_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| window.input("after-close", cx));
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        factory.written_text().contains("after-close")
    })
    .await;
    assert_eq!(factory.starts(), 2);
    assert_eq!(factory.written_text(), "after-close");
}

#[gpui_kit::test]
fn closing_another_groups_selected_tab_preserves_input_focus(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(HostId(DB_01))), cx)
    });
    split_seeded_terminals(cx, handle, &workspace);
    in_frame(cx, handle, |window, cx| {
        window.click(("close-terminal", FIRST_NEW_TERMINAL), cx);
    });
    in_frame(cx, handle, |window, cx| {
        let panel = workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(INITIAL_STAGING_TERMINAL))
            .unwrap();
        assert!(panel.read(cx).focus_handle(cx).contains_focused(window, cx));
        assert_eq!(
            workspace.read(cx).active_tab(),
            Some(CenterTab::Terminal(RemoteTerminalId(
                INITIAL_STAGING_TERMINAL
            )))
        );
    });
}

#[gpui_kit::test]
fn a_modal_opened_during_close_keeps_its_focus(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    split_seeded_terminals(cx, handle, &workspace);
    in_frame(cx, handle, |window, cx| {
        window.click(("close-terminal", INITIAL_STAGING_TERMINAL), cx);
        window.open_dialog(cx, |dialog, _, _| dialog.title("A new question"));
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.has_active_dialog(cx));
        let panel = workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(INITIAL_WEB_TERMINAL))
            .unwrap();
        assert!(!panel.read(cx).focus_handle(cx).contains_focused(window, cx));
        window.close_dialog(cx);
    });
}

#[gpui_kit::test]
fn closing_a_split_returns_to_the_most_recent_visible_tab(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(HostId(DB_01))), cx)
    });
    split_seeded_terminals(cx, handle, &workspace);
    in_frame(cx, handle, |window, cx| {
        let panel = PanelId::from(
            workspace
                .read(cx)
                .remote_terminal(RemoteTerminalId(FIRST_NEW_TERMINAL))
                .unwrap()
                .entity_id(),
        );
        let area = workspace.read(cx).dock_area().clone();
        area.update(cx, |area, cx| {
            let node = area
                .layout(DockPlacement::Center)
                .unwrap()
                .find_panel_node(panel)
                .unwrap();
            area.move_panel(
                panel,
                InsertTarget::Split {
                    node,
                    placement: Placement::Bottom,
                    size: None,
                },
                window,
                cx,
            );
        });
    });
    for id in [FIRST_NEW_TERMINAL, INITIAL_STAGING_TERMINAL] {
        in_frame(cx, handle, |window, cx| window.click(("terminal", id), cx));
    }
    in_frame(cx, handle, |window, cx| {
        window.click(("close-terminal", INITIAL_STAGING_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            workspace.read(cx).active_tab(),
            Some(CenterTab::Terminal(RemoteTerminalId(FIRST_NEW_TERMINAL)))
        );
        let panel = workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(FIRST_NEW_TERMINAL))
            .unwrap();
        assert!(panel.read(cx).focus_handle(cx).contains_focused(window, cx));
    });
}

#[gpui_kit::test]
fn a_new_focus_choice_supersedes_a_pending_close_handoff(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(HostId(DB_01))), cx)
    });
    split_seeded_terminals(cx, handle, &workspace);
    in_frame(cx, handle, |window, cx| {
        window.click(("close-terminal", INITIAL_STAGING_TERMINAL), cx);
        // The second click belongs to a newer user operation in this frame.
        window.click("host-search", cx);
    });
    in_frame(cx, handle, |window, cx| {
        let panel = workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(FIRST_NEW_TERMINAL))
            .unwrap();
        assert!(!panel.read(cx).focus_handle(cx).contains_focused(window, cx));
        assert_eq!(window.find("host-search").focused(), Some(true));
    });
}

#[gpui_kit::test]
fn local_and_settings_close_buttons_keep_an_explicit_dispatch_target(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(NewLocalTerminal), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.blur(cx);
        window.click(("close-local-terminal", 1u64), cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find(("close-local-terminal", 1u64)).is_none());
        window.dispatch_action(Box::new(OpenSettings), cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.blur(cx);
        window.click("close-settings", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("close-settings").is_none())
    });
}

#[gpui_kit::test]
async fn closing_a_terminal_split_returns_keyboard_input_to_the_sftp_file_list(
    cx: &mut TestAppContext,
) {
    let (handle, workspace) = open_workspace_with_sftp(cx, Arc::new(FakeSftpProvider::default()));
    open_test_explorer(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        let panel = PanelId::from(
            workspace
                .read(cx)
                .remote_terminal(RemoteTerminalId(INITIAL_STAGING_TERMINAL))
                .unwrap()
                .entity_id(),
        );
        let area = workspace.read(cx).dock_area().clone();
        area.update(cx, |area, cx| {
            let node = area
                .layout(DockPlacement::Center)
                .unwrap()
                .find_panel_node(panel)
                .unwrap();
            area.move_panel(
                panel,
                InsertTarget::Split {
                    node,
                    placement: Placement::Right,
                    size: None,
                },
                window,
                cx,
            );
        });
    });
    in_frame(cx, handle, |window, cx| {
        window
            .within(("remote-pane", SFTP_TAB))
            .click("file:文件 甲.txt", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", INITIAL_STAGING_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("close-terminal", INITIAL_STAGING_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| {
        let explorer = workspace
            .read(cx)
            .explorer(ExplorerId(SFTP_TAB))
            .unwrap()
            .read(cx);
        assert!(
            explorer
                .remote()
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx)
        );
        assert_eq!(
            workspace.read(cx).active_tab(),
            Some(CenterTab::Explorer(ExplorerId(SFTP_TAB)))
        );
        window.press("f5", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("download-confirm").visible());
        window.press("escape", cx);
    });
}

#[gpui_kit::test]
async fn closing_all_tabs_in_a_split_hands_off_the_original_focus(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) =
        open_workspace_with_remote_factory(cx, HostStore::seed(), factory.clone());
    split_seeded_terminals(cx, handle, &workspace);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(HostId(STAGING_API))), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", INITIAL_WEB_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(HostId(WEB_01))), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        (1..=4).all(|id| {
            workspace
                .read(cx)
                .remote_terminal(RemoteTerminalId(id))
                .unwrap()
                .read(cx)
                .terminal()
                .read(cx)
                .lifecycle(cx)
                == TerminalLifecycle::Running
        })
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            workspace.read(cx).active_tab(),
            Some(CenterTab::Terminal(RemoteTerminalId(INITIAL_WEB_TERMINAL)))
        );
        window.dispatch_action(
            Box::new(CloseTabs {
                tab: CenterTab::Terminal(RemoteTerminalId(INITIAL_WEB_TERMINAL)),
                scope: CloseScope::All,
            }),
            cx,
        );
    });
    in_frame(cx, handle, |window, cx| {
        let workspace = workspace.read(cx);
        assert!(
            workspace
                .remote_terminal(RemoteTerminalId(INITIAL_WEB_TERMINAL))
                .is_none()
        );
        assert!(workspace.remote_terminal(RemoteTerminalId(4)).is_none());
        let survivor = workspace.remote_terminal(RemoteTerminalId(3)).unwrap();
        assert!(
            survivor
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx)
        );
        window.input("after-batch-close", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        factory.written_text().contains("after-batch-close")
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(CloseActiveTab), cx)
    });
    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert!(workspace.remote_terminal(RemoteTerminalId(3)).is_none());
        assert!(
            workspace
                .remote_terminal(RemoteTerminalId(INITIAL_STAGING_TERMINAL))
                .is_some()
        );
    });
}
