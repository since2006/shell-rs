//! New tabs follow the terminal or center group that opened them.

use crate::support::*;
use gpui_kit::component::dock::{
    DockLayout, DockPlacement, NodeId, PaneRef, PanelId, panel_handle,
};

fn four_groups(cx: &mut TestAppContext) -> (WindowHandle<Root>, Entity<Workspace>) {
    let provider = Arc::new(FakeSftpProvider::with_files(&[(
        "/home/tester/文件 甲.txt",
        b"split file",
    )]));
    let (handle, workspace) = open_workspace_with_sftp(cx, provider);
    in_frame(cx, handle, |window, _| window.activate_window());
    for _ in 2..8 {
        in_frame(cx, handle, |window, cx| {
            window.dispatch_action(Box::new(ConnectHost(HostId(WEB_01))), cx);
        });
    }
    in_frame(cx, handle, |window, cx| {
        let panels: Vec<_> = (1..=8)
            .map(|id| {
                workspace
                    .read(cx)
                    .remote_terminal(RemoteTerminalId(id))
                    .unwrap()
                    .clone()
            })
            .collect();
        let group = |ix: usize| {
            DockLayout::tabs()
                .panel_view(panel_handle(panels[ix].clone()), cx)
                .panel_view(panel_handle(panels[ix + 4].clone()), cx)
                .active_index(0)
        };
        let layout = DockLayout::h_split()
            .child(
                DockLayout::v_split()
                    .child(group(0), None)
                    .child(group(2), None),
                None,
            )
            .child(
                DockLayout::v_split()
                    .child(group(1), None)
                    .child(group(3), None),
                None,
            );
        workspace
            .read(cx)
            .dock_area()
            .clone()
            .update(cx, |area, cx| {
                area.set_center(layout, window, cx);
            });
    });
    (handle, workspace)
}

fn terminal_group(workspace: &Entity<Workspace>, id: u64, cx: &App) -> NodeId {
    let workspace = workspace.read(cx);
    let panel = PanelId::from(
        workspace
            .remote_terminal(RemoteTerminalId(id))
            .unwrap()
            .entity_id(),
    );
    workspace
        .dock_area()
        .read(cx)
        .layout(DockPlacement::Center)
        .unwrap()
        .find_panel_node(panel)
        .unwrap()
}

#[gpui_kit::test]
fn sftp_buttons_open_in_their_own_split_without_switching_the_first_group(cx: &mut TestAppContext) {
    let (handle, workspace) = four_groups(cx);
    for (index, source) in [2_u64, 3, 4, 1].into_iter().enumerate() {
        // Focus is deliberately in another split when its SFTP button is clicked.
        in_frame(cx, handle, |window, cx| {
            window.click(("terminal", 1_u64), cx)
        });
        let target = cx.update(|cx| terminal_group(&workspace, source, cx));
        in_frame(cx, handle, |window, cx| window.click(("sftp", source), cx));
        in_frame(cx, handle, |window, cx| {
            let workspace = workspace.read(cx);
            let explorer = workspace
                .explorer(ExplorerId(index as u64 + 1))
                .expect("SFTP opened");
            let panel = PanelId::from(explorer.entity_id());
            let tree = workspace
                .dock_area()
                .read(cx)
                .layout(DockPlacement::Center)
                .unwrap();
            assert_eq!(
                tree.find_panel_node(panel),
                Some(target),
                "SFTP follows terminal {source}"
            );
            if source != 1 {
                assert!(
                    window.find(("terminal", 1_u64)).visible(),
                    "first split keeps its displayed terminal"
                );
                let first = PanelId::from(
                    workspace
                        .remote_terminal(RemoteTerminalId(1))
                        .unwrap()
                        .entity_id(),
                );
                let group = tree
                    .find_node(tree.find_panel_node(first).unwrap())
                    .unwrap();
                let PaneRef::Tabs { panels, active_ix } = group.kind() else {
                    panic!("tab group")
                };
                assert_eq!(panels[active_ix], first);
            }
            assert_eq!(
                workspace.active_tab(),
                Some(CenterTab::Explorer(ExplorerId(index as u64 + 1)))
            );
            use gpui_kit::Focusable;
            assert!(
                explorer
                    .read(cx)
                    .focus_handle(cx)
                    .contains_focused(window, cx),
                "new SFTP owns keyboard focus"
            );
            assert_eq!(tree.panels().count(), 8 + index + 1);
            let group = tree.find_node(target).unwrap();
            let PaneRef::Tabs { panels, .. } = group.kind() else {
                panic!("tab group")
            };
            let original = [source, source + 4].map(|id| {
                PanelId::from(
                    workspace
                        .remote_terminal(RemoteTerminalId(id))
                        .unwrap()
                        .entity_id(),
                )
            });
            assert_eq!(&panels[..2], &original, "existing tab order is preserved");
        });
    }
}

#[gpui_kit::test]
fn global_new_tabs_follow_the_active_group(cx: &mut TestAppContext) {
    let (handle, workspace) = four_groups(cx);
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 3_u64), cx)
    });
    let target = cx.update(|cx| terminal_group(&workspace, 3, cx));
    let assert_group = |panel, cx: &mut TestAppContext| {
        cx.update(|cx| {
            let tree = workspace
                .read(cx)
                .dock_area()
                .read(cx)
                .layout(DockPlacement::Center)
                .unwrap();
            assert_eq!(tree.find_panel_node(panel), Some(target));
        });
    };

    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(OpenExplorer::new(HostId(WEB_01))), cx)
    });
    let explorer = cx.update(|cx| {
        PanelId::from(
            workspace
                .read(cx)
                .explorer(ExplorerId(1))
                .unwrap()
                .entity_id(),
        )
    });
    assert_group(explorer, cx);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(NewLocalTerminal), cx)
    });
    let local = cx.update(|cx| {
        PanelId::from(
            workspace
                .read(cx)
                .local_terminal(LocalTerminalId(1))
                .unwrap()
                .entity_id(),
        )
    });
    assert_group(local, cx);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(HostId(WEB_01))), cx)
    });
    let remote = cx.update(|cx| {
        PanelId::from(
            workspace
                .read(cx)
                .remote_terminal(RemoteTerminalId(9))
                .unwrap()
                .entity_id(),
        )
    });
    assert_group(remote, cx);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(OpenSettings), cx)
    });
    let settings =
        cx.update(|cx| PanelId::from(workspace.read(cx).settings_tab().unwrap().entity_id()));
    assert_group(settings, cx);
}

#[gpui_kit::test]
fn duplicate_terminal_and_new_sftp_follow_the_explicit_origin(cx: &mut TestAppContext) {
    let (handle, workspace) = four_groups(cx);
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 1_u64), cx)
    });
    let target = cx.update(|cx| terminal_group(&workspace, 4, cx));
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(
            Box::new(shellrs::app::DuplicateTerminal(RemoteTerminalId(4))),
            cx,
        );
    });
    assert_eq!(cx.update(|cx| terminal_group(&workspace, 9, cx)), target);
    in_frame(cx, handle, |window, cx| window.click(("sftp", 9_u64), cx));
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 1_u64), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("open-sftp", 1_u64), cx)
    });
    in_frame(cx, handle, |window, cx| {
        let workspace = workspace.read(cx);
        let tree = workspace
            .dock_area()
            .read(cx)
            .layout(DockPlacement::Center)
            .unwrap();
        for id in [1, 2] {
            let panel = PanelId::from(workspace.explorer(ExplorerId(id)).unwrap().entity_id());
            assert_eq!(tree.find_panel_node(panel), Some(target));
        }
        assert!(window.find(("terminal", 1_u64)).visible());
        assert_eq!(
            workspace.explorers_of(HostId(WEB_01), cx).len(),
            2,
            "same host permits independent SFTP tabs"
        );
    });
}

#[gpui_kit::test]
fn an_sftp_request_resolves_a_moved_origin_and_falls_back_when_closed(cx: &mut TestAppContext) {
    use gpui_kit::component::dock::InsertTarget;
    use shellrs::app::CloseTerminal;

    let (handle, workspace) = four_groups(cx);
    let request =
        OpenExplorer::new(HostId(WEB_01)).with_origin(CenterTab::Terminal(RemoteTerminalId(3)));
    let target = cx.update(|cx| terminal_group(&workspace, 4, cx));
    in_frame(cx, handle, |window, cx| {
        let panel = PanelId::from(
            workspace
                .read(cx)
                .remote_terminal(RemoteTerminalId(3))
                .unwrap()
                .entity_id(),
        );
        workspace
            .read(cx)
            .dock_area()
            .clone()
            .update(cx, |area, cx| {
                area.move_panel(
                    panel,
                    InsertTarget::Tabs {
                        node: target,
                        ix: None,
                        activate: true,
                    },
                    window,
                    cx,
                );
            });
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 1_u64), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(request.clone()), cx)
    });
    cx.update(|cx| {
        let workspace = workspace.read(cx);
        let panel = PanelId::from(workspace.explorer(ExplorerId(1)).unwrap().entity_id());
        assert_eq!(
            workspace
                .dock_area()
                .read(cx)
                .layout(DockPlacement::Center)
                .unwrap()
                .find_panel_node(panel),
            Some(target)
        );
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(CloseTerminal(RemoteTerminalId(3))), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 2_u64), cx)
    });
    let fallback = cx.update(|cx| terminal_group(&workspace, 2, cx));
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(request), cx)
    });
    cx.update(|cx| {
        let workspace = workspace.read(cx);
        let panel = PanelId::from(workspace.explorer(ExplorerId(2)).unwrap().entity_id());
        assert_eq!(
            workspace
                .dock_area()
                .read(cx)
                .layout(DockPlacement::Center)
                .unwrap()
                .find_panel_node(panel),
            Some(fallback)
        );
    });
}

#[gpui_kit::test]
async fn an_editor_opens_in_its_sftp_group_and_preserves_other_tabs(cx: &mut TestAppContext) {
    let (handle, workspace) = four_groups(cx);
    let target = cx.update(|cx| terminal_group(&workspace, 4, cx));
    in_frame(cx, handle, |window, cx| window.click(("sftp", 4_u64), cx));
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window
            .within(("remote-pane", 1_u64))
            .try_find("name:文件 甲.txt")
            .is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 1_u64), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window
            .within(("remote-pane", 1_u64))
            .double_click("name:文件 甲.txt", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        workspace
            .read(cx)
            .editor(shellrs::editor::EditorId(1))
            .is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        let workspace = workspace.read(cx);
        let editor = workspace.editor(shellrs::editor::EditorId(1)).unwrap();
        let panel = PanelId::from(editor.entity_id());
        assert_eq!(
            workspace
                .dock_area()
                .read(cx)
                .layout(DockPlacement::Center)
                .unwrap()
                .find_panel_node(panel),
            Some(target)
        );
        assert_eq!(
            editor.read(cx).state().read(cx).value().as_ref(),
            "split file"
        );
        assert!(window.find(("terminal", 1_u64)).visible());
    });
}

/// A new command wins over the drag's deferred focus repair, even when
/// adding its tab leaves the root NodeId unchanged.
#[gpui_kit::test]
fn opening_settings_after_a_drop_wins_over_pending_focus_recovery(cx: &mut TestAppContext) {
    use gpui_kit::Focusable;

    let (handle, workspace) = four_groups(cx);
    in_frame(cx, handle, |window, cx| {
        let from = window.find(("terminal-tab", 2_u64)).bounds().center();
        let target = window.find(("terminal", 1_u64)).bounds();
        window.drag(from, point(target.right() - px(5.), target.center().y), cx);
    });
    // Deliberately do not deliver a platform next-frame callback yet.
    let after_drop = cx.update(|cx| {
        workspace
            .read(cx)
            .dock_area()
            .read(cx)
            .layout(DockPlacement::Center)
            .unwrap()
            .root()
            .id()
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(OpenSettings), cx);
    });
    in_frame(cx, handle, |window, cx| {
        let workspace = workspace.read(cx);
        let panel = workspace
            .settings_tab()
            .expect("settings opened after the drop");
        assert_eq!(
            workspace
                .dock_area()
                .read(cx)
                .layout(DockPlacement::Center)
                .unwrap()
                .root()
                .id(),
            after_drop,
            "adding settings must exercise unchanged-root validation"
        );
        assert!(panel.read(cx).focus_handle(cx).contains_focused(window, cx));
        window.simulate_next_frame(cx);
    });
    in_frame(cx, handle, |window, cx| {
        let workspace = workspace.read(cx);
        assert_eq!(workspace.active_tab(), Some(CenterTab::Settings));
        assert!(
            workspace
                .settings_tab()
                .unwrap()
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx),
            "an older drag must not take the keyboard back from settings"
        );
    });
}

fn drag_before_title_command(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    in_frame(cx, handle, |window, cx| {
        let from = window.find(("terminal-tab", 2_u64)).bounds().center();
        let target = window.find(("terminal", 1_u64)).bounds();
        window.drag(from, point(target.right() - px(5.), target.center().y), cx);
    });
}

#[gpui_kit::test]
fn title_bar_sidebar_choice_wins_over_pending_drag_focus(cx: &mut TestAppContext) {
    let (handle, _) = four_groups(cx);
    drag_before_title_command(cx, handle);
    in_frame(cx, handle, |window, cx| window.click("show-forwards", cx));
    let requested = in_frame(cx, handle, |window, cx| {
        assert!(window.find("forward-search").visible());
        window
            .focused(cx)
            .expect("the title command focuses its list")
    });
    in_frame(cx, handle, |window, cx| window.simulate_next_frame(cx));
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.focused(cx),
            Some(requested),
            "the old drag must not take the keyboard from the chosen sidebar"
        );
    });
}

#[gpui_kit::test]
fn title_bar_compose_choice_wins_over_pending_drag_focus(cx: &mut TestAppContext) {
    let (handle, _) = four_groups(cx);
    drag_before_title_command(cx, handle);
    in_frame(cx, handle, |window, cx| {
        window.click("toggle-compose-pane", cx)
    });
    let requested = in_frame(cx, handle, |window, cx| {
        assert!(window.find("compose-pane").visible());
        window.focused(cx).expect("compose input receives focus")
    });
    in_frame(cx, handle, |window, cx| window.simulate_next_frame(cx));
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.focused(cx),
            Some(requested),
            "the old drag must not take the keyboard from the compose input"
        );
    });
}

#[gpui_kit::test]
fn title_command_without_pointer_preserves_non_center_focus(cx: &mut TestAppContext) {
    let (handle, _) = four_groups(cx);
    drag_before_title_command(cx, handle);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(shellrs::app::ToggleComposePane), cx)
    });
    let requested = in_frame(cx, handle, |window, cx| {
        assert!(window.find("compose-pane").visible());
        window.focused(cx).expect("compose input receives focus")
    });
    in_frame(cx, handle, |window, cx| window.simulate_next_frame(cx));
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.focused(cx),
            Some(requested),
            "programmatic focus changes also supersede old drag recovery"
        );
    });
}
