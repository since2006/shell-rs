//! Split layouts driven through production Dock pointer handlers.

use crate::support::*;
use gpui_kit::component::dock::{DockLayout, DockPlacement, panel_handle};

/// Run platform callbacks and queued effects until consecutive frames agree.
/// The bound catches redraw loops instead of masking them with extra frames.
fn settle_layout(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    let mut previous = None;
    for _ in 0..8 {
        in_frame(cx, handle, |window, cx| window.simulate_next_frame(cx));
        let current = in_frame(cx, handle, |window, _| {
            (1..=8_u64)
                .filter_map(|id| {
                    let terminal = window.try_find(("terminal", id))?;
                    Some((id, terminal.bounds(), terminal.focused()))
                })
                .collect::<Vec<_>>()
        });
        if previous.as_ref() == Some(&current) {
            return;
        }
        previous = Some(current);
    }
    panic!("split layout did not settle within eight platform frames");
}

fn assert_panel_count(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    workspace: &Entity<Workspace>,
    count: u64,
) {
    settle_layout(cx, handle);
    in_frame(cx, handle, |_, cx| {
        let area = workspace.read(cx).dock_area().read(cx);
        assert_eq!(
            area.layout(DockPlacement::Center)
                .expect("center")
                .panels()
                .count(),
            count as usize,
            "no panel lost"
        );
    });
}

async fn reproduce(
    cx: &mut TestAppContext,
    columns_first: bool,
    tabs_per_group: u64,
    repeats: usize,
    unequal: bool,
    lifecycle_checks: bool,
) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_counted_workspace(cx, factory.clone());
    let count = tabs_per_group * 4;
    for _ in 2..count {
        in_frame(cx, handle, |window, cx| {
            window.dispatch_action(Box::new(ConnectHost(HostId(WEB_01))), cx)
        });
    }
    in_frame(cx, handle, |window, cx| {
        let panels: Vec<_> = (1..=count)
            .map(|id| {
                workspace
                    .read(cx)
                    .remote_terminal(RemoteTerminalId(id))
                    .unwrap()
                    .clone()
            })
            .collect();
        let group = |ix: usize, cx: &App| {
            let mut group = DockLayout::tabs().panel_view(panel_handle(panels[ix].clone()), cx);
            if tabs_per_group == 2 {
                group = group.panel_view(panel_handle(panels[ix + 4].clone()), cx);
            }
            group.active_index(0)
        };
        let layout = if columns_first {
            DockLayout::h_split()
                .child(
                    DockLayout::v_split()
                        .child(group(0, cx), unequal.then_some(px(450.)))
                        .child(group(2, cx), unequal.then_some(px(287.5))),
                    None,
                )
                .child(
                    DockLayout::v_split()
                        .child(group(1, cx), None)
                        .child(group(3, cx), None),
                    None,
                )
        } else {
            DockLayout::v_split()
                .child(
                    DockLayout::h_split()
                        .child(group(0, cx), None)
                        .child(group(1, cx), None),
                    None,
                )
                .child(
                    DockLayout::h_split()
                        .child(group(2, cx), None)
                        .child(group(3, cx), None),
                    None,
                )
        };
        let area = workspace.read(cx).dock_area().clone();
        area.update(cx, |area, cx| area.set_center(layout, window, cx));
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        (1..=count).all(|id| {
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
    assert_panel_count(cx, handle, &workspace, count);

    let identities = in_frame(cx, handle, |_, cx| {
        (1..=count)
            .map(|id| {
                workspace
                    .read(cx)
                    .remote_terminal(RemoteTerminalId(id))
                    .unwrap()
                    .entity_id()
            })
            .collect::<Vec<_>>()
    });
    let stable = in_frame(cx, handle, |window, _| {
        window.find(("terminal", 3_u64)).bounds()
    });
    for right in (0..repeats).flat_map(|_| [true, false]) {
        in_frame(cx, handle, |window, cx| {
            let from = window.find(("terminal-tab", 2_u64)).bounds().center();
            let b = window.find(("terminal", 1_u64)).bounds();
            let to = if right {
                point(b.right() - px(5.), b.center().y)
            } else {
                point(b.center().x, b.bottom() - px(5.))
            };
            window.drag(from, to, cx);
        });

        assert_panel_count(cx, handle, &workspace, count);
        in_frame(cx, handle, |window, cx| {
            for (id, entity) in (1..=count).zip(&identities) {
                let panel = workspace
                    .read(cx)
                    .remote_terminal(RemoteTerminalId(id))
                    .expect("panel registry retained");
                assert_eq!(panel.entity_id(), *entity, "panel entity reused");
                assert_eq!(
                    panel.read(cx).terminal().read(cx).lifecycle(cx),
                    TerminalLifecycle::Running,
                    "terminal still running"
                );
            }
            assert_eq!(factory.starts(), count as usize, "no connection restart");
            let unchanged = window.find(("terminal", 3_u64)).bounds();
            assert!(
                (unchanged.top() - stable.top()).as_f32().abs() <= 1.,
                "unrelated lower pane stays in place: {:?} versus {:?}",
                stable,
                unchanged
            );
            assert!(
                (unchanged.size.height - stable.size.height).as_f32().abs() <= 1.,
                "unrelated lower pane keeps height"
            );
        });
    }
    in_frame(cx, handle, |window, _| {
        let first = window.find(("terminal", 1_u64)).bounds();
        let second = window.find(("terminal", 2_u64)).bounds();
        let difference = (first.size.height - second.size.height).as_f32().abs();
        assert!(
            difference <= 1.,
            "a bottom-edge drop should divide its target evenly; terminal content heights are {} and {}",
            first.size.height.as_f32(),
            second.size.height.as_f32()
        );
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find(("terminal", 2_u64)).focused(),
            Some(true),
            "focus stays on moved terminal"
        );
        assert_eq!(
            workspace.read(cx).active_tab(),
            Some(CenterTab::Terminal(RemoteTerminalId(2))),
            "active tab stays on moved terminal"
        );
    });
    if lifecycle_checks {
        in_frame(cx, handle, |window, cx| {
            window.input("input_after_drag", cx)
        });
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
            factory.written_text().contains("input_after_drag")
        })
        .await;
        let before_zoom = in_frame(cx, handle, |window, cx| {
            let bounds = window.find(("terminal", 2_u64)).bounds();
            let area = workspace.read(cx).dock_area().clone();
            let panel = workspace
                .read(cx)
                .remote_terminal(RemoteTerminalId(2))
                .unwrap()
                .entity_id()
                .into();
            let node = area
                .read(cx)
                .layout(DockPlacement::Center)
                .unwrap()
                .find_panel_node(panel)
                .unwrap();
            area.update(cx, |area, cx| area.set_zoomed_in(node, window, cx));
            bounds
        });
        in_frame(cx, handle, |window, cx| {
            assert!(workspace.read(cx).dock_area().read(cx).is_zoomed());
            assert!(
                window.find(("terminal", 2_u64)).bounds().size.height > before_zoom.size.height
            );
            workspace
                .read(cx)
                .dock_area()
                .clone()
                .update(cx, |area, cx| area.set_zoomed_out(window, cx));
        });
        assert_panel_count(cx, handle, &workspace, count);
        in_frame(cx, handle, |window, _| {
            let after = window.find(("terminal", 2_u64)).bounds();
            assert!(
                (after.size.height - before_zoom.size.height).as_f32().abs() <= 1.,
                "zoom restores layout"
            );
        });
        in_frame(cx, handle, |window, cx| {
            window.click(("close-terminal", 2_u64), cx)
        });
        in_frame(cx, handle, |_, cx| {
            assert!(
                workspace
                    .read(cx)
                    .remote_terminal(RemoteTerminalId(2))
                    .is_none(),
                "close command still routes"
            );
            assert_eq!(
                workspace
                    .read(cx)
                    .dock_area()
                    .read(cx)
                    .layout(DockPlacement::Center)
                    .unwrap()
                    .panels()
                    .count(),
                count as usize - 1
            );
        });
    }
}

#[gpui_kit::test]
async fn columns_first_one_tab(cx: &mut TestAppContext) {
    reproduce(cx, true, 1, 1, false, false).await;
}
#[gpui_kit::test]
async fn rows_first_one_tab(cx: &mut TestAppContext) {
    reproduce(cx, false, 1, 1, false, false).await;
}
#[gpui_kit::test]
async fn columns_first_two_tabs(cx: &mut TestAppContext) {
    reproduce(cx, true, 2, 1, false, false).await;
}
#[gpui_kit::test]
async fn rows_first_two_tabs(cx: &mut TestAppContext) {
    reproduce(cx, false, 2, 1, false, false).await;
}

fn open_counted_workspace(
    cx: &mut TestAppContext,
    factory: Arc<FakeTerminalFactory>,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    open_counted_workspace_with_sftp(cx, factory, Arc::new(FakeSftpProvider::default()))
}

fn open_counted_workspace_with_sftp(
    cx: &mut TestAppContext,
    factory: Arc<FakeTerminalFactory>,
    provider: Arc<FakeSftpProvider>,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    init_app(cx);
    cx.update(|cx| cx.set_reduce_motion(true));
    let mut workspace = None;
    let handle = cx.open_window(size(px(1280.), px(800.)), |window, cx| {
        let store = cx.new(|_| HostStore::seed());
        let remote = Arc::new(FixedRemoteTerminalTransportProvider::new(factory));
        let view = cx.new(|cx| {
            Workspace::new_with_services(
                store,
                cx.new(|_| settings_store()),
                remote,
                fake_telnet_terminals(),
                fake_serial_terminals(),
                fake_local_terminals(),
                provider,
                Arc::new(FakeLocalDirectory::default()),
                Arc::new(FakeConnectionTester::default()),
                Arc::new(FakeForwardProvider::default()),
                window,
                cx,
            )
        });
        workspace = Some(view.clone());
        Root::new(view, window, cx)
    });
    (handle, workspace.unwrap())
}
#[gpui_kit::test]
async fn columns_first_repeated_drag(cx: &mut TestAppContext) {
    reproduce(cx, true, 2, 5, false, false).await;
}
#[gpui_kit::test]
async fn rows_first_repeated_drag(cx: &mut TestAppContext) {
    reproduce(cx, false, 2, 5, false, false).await;
}
#[gpui_kit::test]
async fn unequal_heights_survive_drag(cx: &mut TestAppContext) {
    reproduce(cx, true, 2, 3, true, false).await;
}

#[gpui_kit::test]
async fn repair_preserves_input_zoom_and_close(cx: &mut TestAppContext) {
    reproduce(cx, true, 2, 1, false, true).await;
}

#[gpui_kit::test]
async fn pointer_created_grid_after_window_resize(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_counted_workspace(cx, factory.clone());
    for _ in 0..2 {
        in_frame(cx, handle, |window, cx| {
            window.dispatch_action(Box::new(ConnectHost(HostId(WEB_01))), cx)
        });
    }
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
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
        window.click(("terminal-tab", 1_u64), cx)
    });
    for (source, target, right) in [(2_u64, 1_u64, true), (3, 1, false), (4, 2, false)] {
        in_frame(cx, handle, |window, cx| {
            let from = window.find(("terminal-tab", source)).bounds().center();
            let b = window.find(("terminal", target)).bounds();
            let to = if right {
                point(b.right() - px(5.), b.center().y)
            } else {
                point(b.center().x, b.bottom() - px(5.))
            };
            window.drag(from, to, cx);
        });
        assert_panel_count(cx, handle, &workspace, 4);
    }
    in_frame(cx, handle, |window, _| {
        window.resize(size(px(1440.), px(900.)))
    });
    assert_panel_count(cx, handle, &workspace, 4);
    let stable = in_frame(cx, handle, |window, _| {
        window.find(("terminal", 3_u64)).bounds()
    });
    for right in [true, false, true, false] {
        in_frame(cx, handle, |window, cx| {
            let from = window.find(("terminal-tab", 2_u64)).bounds().center();
            let b = window.find(("terminal", 1_u64)).bounds();
            let to = if right {
                point(b.right() - px(5.), b.center().y)
            } else {
                point(b.center().x, b.bottom() - px(5.))
            };
            window.drag(from, to, cx);
        });
        assert_panel_count(cx, handle, &workspace, 4);
        in_frame(cx, handle, |window, _| {
            assert_eq!(window.find(("terminal", 2_u64)).focused(), Some(true));
            let first = window.find(("terminal", 1_u64)).bounds();
            let second = window.find(("terminal", 2_u64)).bounds();
            let untouched = window.find(("terminal", 3_u64)).bounds();
            if !right {
                assert!((first.size.height - second.size.height).as_f32().abs() <= 1.);
            }
            assert!((untouched.size.height - stable.size.height).as_f32().abs() <= 1.);
            assert!((untouched.top() - stable.top()).as_f32().abs() <= 1.);
            assert_eq!(factory.starts(), 4);
        });
    }
}

#[gpui_kit::test]
async fn canceled_drag_does_not_repair_a_later_open(cx: &mut TestAppContext) {
    use gpui_kit::{Modifiers, MouseDownEvent, MouseUpEvent};
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_counted_workspace(cx, factory);
    assert_panel_count(cx, handle, &workspace, 2);
    let root_before = in_frame(cx, handle, |window, cx| {
        let root = workspace
            .read(cx)
            .dock_area()
            .read(cx)
            .layout(DockPlacement::Center)
            .unwrap()
            .root()
            .id();
        let from = window.find(("terminal-tab", 2_u64)).bounds().center();
        let target = window.find(("terminal", 1_u64)).bounds().center();
        window.dispatch_event(
            MouseMoveEvent {
                position: from,
                pressed_button: None,
                modifiers: Modifiers::default(),
            }
            .to_platform_input(),
            cx,
        );
        window.dispatch_event(
            MouseDownEvent {
                position: from,
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            }
            .to_platform_input(),
            cx,
        );
        for step in 1..=8 {
            let fraction = step as f32 / 8.;
            let position = point(
                from.x + (target.x - from.x) * fraction,
                from.y + (target.y - from.y) * fraction,
            );
            window.dispatch_event(
                MouseMoveEvent {
                    position,
                    pressed_button: Some(MouseButton::Left),
                    modifiers: Modifiers::default(),
                }
                .to_platform_input(),
                cx,
            );
            window.render_frame(cx);
        }
        let outside = point(px(2.), px(2.));
        window.dispatch_event(
            MouseMoveEvent {
                position: outside,
                pressed_button: Some(MouseButton::Left),
                modifiers: Modifiers::default(),
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
        window.dispatch_event(
            MouseUpEvent {
                position: outside,
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
                click_count: 1,
            }
            .to_platform_input(),
            cx,
        );
        root
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(OpenSettings), cx)
    });
    settle_layout(cx, handle);
    in_frame(cx, handle, |_, cx| {
        assert_eq!(
            workspace
                .read(cx)
                .dock_area()
                .read(cx)
                .layout(DockPlacement::Center)
                .unwrap()
                .root()
                .id(),
            root_before,
            "canceled drag must not rebuild the later settings-open layout"
        );
    });
}

#[derive(Clone, Copy)]
enum Edge {
    Left,
    Right,
    Top,
    Bottom,
    Center,
}

fn drag_terminal(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    source: u64,
    target: u64,
    edge: Edge,
) {
    in_frame(cx, handle, |window, cx| {
        let from = window.find(("terminal-tab", source)).bounds().center();
        let bounds = window.find(("terminal", target)).bounds();
        let to = match edge {
            Edge::Left => point(bounds.left() + px(5.), bounds.center().y),
            Edge::Right => point(bounds.right() - px(5.), bounds.center().y),
            Edge::Top => point(bounds.center().x, bounds.top() + px(5.)),
            Edge::Bottom => point(bounds.center().x, bounds.bottom() - px(5.)),
            Edge::Center => bounds.center(),
        };
        window.drag(from, to, cx);
    });
    settle_layout(cx, handle);
}

fn assert_visible_terminals_do_not_overlap(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    ids: &[u64],
) {
    in_frame(cx, handle, |window, _| {
        let bounds: Vec<_> = ids
            .iter()
            .map(|id| {
                let terminal = window.find(("terminal", *id));
                assert!(terminal.visible(), "terminal {id} stays visible");
                let bounds = terminal.bounds();
                assert!(bounds.size.width > px(0.) && bounds.size.height > px(0.));
                bounds
            })
            .collect();
        for (ix, first) in bounds.iter().enumerate() {
            for second in &bounds[ix + 1..] {
                assert!(
                    first.right() <= second.left()
                        || second.right() <= first.left()
                        || first.bottom() <= second.top()
                        || second.bottom() <= first.top(),
                    "terminal content cannot overlap: {first:?} and {second:?}"
                );
            }
        }
    });
}

#[gpui_kit::test]
async fn same_group_can_split_in_each_direction_and_merge_back(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_counted_workspace(cx, factory.clone());
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        [1, 2].into_iter().all(|id| {
            remote_lifecycle(&workspace, RemoteTerminalId(id), cx) == TerminalLifecycle::Running
        })
    })
    .await;
    for edge in [Edge::Left, Edge::Top, Edge::Right, Edge::Bottom] {
        in_frame(cx, handle, |window, cx| {
            window.click(("terminal-tab", 1_u64), cx)
        });
        drag_terminal(cx, handle, 2, 1, edge);
        assert_panel_count(cx, handle, &workspace, 2);
        assert_visible_terminals_do_not_overlap(cx, handle, &[1, 2]);
        in_frame(cx, handle, |window, cx| {
            let first = window.find(("terminal", 1_u64)).bounds();
            let second = window.find(("terminal", 2_u64)).bounds();
            let difference = match edge {
                Edge::Left => {
                    assert!(
                        second.right() <= first.left(),
                        "left drop puts the moved pane on the left"
                    );
                    first.size.width - second.size.width
                }
                Edge::Right => {
                    assert!(
                        second.left() >= first.right(),
                        "right drop puts the moved pane on the right"
                    );
                    first.size.width - second.size.width
                }
                Edge::Top => {
                    assert!(
                        second.bottom() <= first.top(),
                        "top drop puts the moved pane above its target"
                    );
                    first.size.height - second.size.height
                }
                Edge::Bottom => {
                    assert!(
                        second.top() >= first.bottom(),
                        "bottom drop puts the moved pane below its target"
                    );
                    first.size.height - second.size.height
                }
                Edge::Center => unreachable!(),
            };
            assert!(
                difference.as_f32().abs() <= 1.,
                "edge split divides target equally"
            );
            assert_eq!(window.find(("terminal", 2_u64)).focused(), Some(true));
            assert_eq!(
                workspace.read(cx).active_tab(),
                Some(CenterTab::Terminal(RemoteTerminalId(2)))
            );
        });
        drag_terminal(cx, handle, 2, 1, Edge::Center);
        in_frame(cx, handle, |_, cx| {
            let area = workspace.read(cx).dock_area().read(cx);
            let tree = area.layout(DockPlacement::Center).unwrap();
            let first = workspace
                .read(cx)
                .remote_terminal(RemoteTerminalId(1))
                .unwrap()
                .entity_id()
                .into();
            let second = workspace
                .read(cx)
                .remote_terminal(RemoteTerminalId(2))
                .unwrap()
                .entity_id()
                .into();
            assert_eq!(
                tree.find_panel_node(first),
                tree.find_panel_node(second),
                "center drop merges tabs"
            );
        });
        assert_eq!(
            factory.starts(),
            2,
            "split and merge reuse both connections"
        );
    }
}

#[gpui_kit::test]
async fn tab_reordering_preserves_container_and_the_other_split(cx: &mut TestAppContext) {
    use gpui_kit::component::dock::PaneRef;
    let (handle, workspace) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(HostId(WEB_01))), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", 1_u64), cx)
    });
    drag_terminal(cx, handle, 3, 1, Edge::Right);
    let (root, sibling) = in_frame(cx, handle, |window, cx| {
        (
            workspace
                .read(cx)
                .dock_area()
                .read(cx)
                .layout(DockPlacement::Center)
                .unwrap()
                .root()
                .id(),
            window.find(("terminal", 3_u64)).bounds(),
        )
    });
    in_frame(cx, handle, |window, cx| {
        let source = window.find(("terminal-tab", 2_u64)).bounds().center();
        let target = window.find(("terminal-tab", 1_u64)).bounds().center();
        window.drag(source, target, cx);
    });
    settle_layout(cx, handle);
    in_frame(cx, handle, |window, cx| {
        let area = workspace.read(cx).dock_area().read(cx);
        let tree = area.layout(DockPlacement::Center).unwrap();
        assert_eq!(
            tree.root().id(),
            root,
            "reordering does not rebuild the center"
        );
        assert_eq!(window.find(("terminal", 3_u64)).bounds(), sibling);
        let first = workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(1))
            .unwrap()
            .entity_id()
            .into();
        let second = workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(2))
            .unwrap()
            .entity_id()
            .into();
        let node = tree.find_panel_node(first).unwrap();
        let PaneRef::Tabs { panels, .. } = tree.find_node(node).unwrap().kind() else {
            panic!("tab group");
        };
        assert_eq!(panels, &[second, first]);
    });
}

#[gpui_kit::test]
async fn nested_drag_after_interface_zoom_keeps_positive_nonoverlapping_panes(
    cx: &mut TestAppContext,
) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_counted_workspace(cx, factory.clone());
    for _ in 0..2 {
        in_frame(cx, handle, |window, cx| {
            window.dispatch_action(Box::new(ConnectHost(HostId(WEB_01))), cx)
        });
    }
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", 1_u64), cx)
    });
    for (source, target, edge) in [
        (2, 1, Edge::Right),
        (3, 1, Edge::Bottom),
        (4, 3, Edge::Right),
    ] {
        drag_terminal(cx, handle, source, target, edge);
    }
    in_frame(cx, handle, |window, cx| {
        workspace
            .read(cx)
            .settings()
            .clone()
            .update(cx, |settings, cx| {
                settings.update(|settings| settings.interface.font_size = 18., cx)
            });
        window.resize(size(px(900.), px(700.)));
    });
    settle_layout(cx, handle);
    for (source, target, edge) in [(2, 4, Edge::Bottom), (2, 4, Edge::Left), (2, 4, Edge::Top)] {
        drag_terminal(cx, handle, source, target, edge);
        assert_panel_count(cx, handle, &workspace, 4);
        assert_visible_terminals_do_not_overlap(cx, handle, &[1, 2, 3, 4]);
    }
    assert_eq!(factory.starts(), 4);
}

#[gpui_kit::test]
async fn dragging_with_sftp_editor_settings_and_compose_preserves_live_work(
    cx: &mut TestAppContext,
) {
    use shellrs::app::{ExplorerAction, ExplorerCommand, ToggleComposePane};
    use shellrs::editor::EditorId;
    let factory = Arc::new(FakeTerminalFactory::default());
    let provider = Arc::new(FakeSftpProvider::with_files(&[(
        "/home/tester/文件 甲.txt",
        b"original text",
    )]));
    let (handle, workspace) =
        open_counted_workspace_with_sftp(cx, factory.clone(), provider.clone());
    open_test_explorer(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window
            .within(("remote-pane", SFTP_TAB))
            .double_click("name:文件 甲.txt", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.try_find(("editor", 1_u64)).is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| window.input("unsaved ", cx));
    let (editor_identity, draft) = in_frame(cx, handle, |_, cx| {
        let editor = workspace.read(cx).editor(EditorId(1)).unwrap();
        assert!(editor.read(cx).is_dirty());
        (
            editor.entity_id(),
            editor.read(cx).state().read(cx).value().to_string(),
        )
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(
            Box::new(ExplorerAction::new(
                ExplorerId(SFTP_TAB),
                ExplorerCommand::UploadPaths {
                    paths: vec![std::path::PathBuf::from("/local/tester/文件 甲.txt")],
                    target: "/home/tester".into(),
                },
            )),
            cx,
        );
    });
    in_frame(cx, handle, |window, cx| window.click("upload-confirm", cx));
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        !provider.requests.lock().unwrap().is_empty()
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click(("editor", 1_u64), cx);
        window.dispatch_action(Box::new(OpenSettings), cx);
    });
    let (explorer_identity, settings_identity) = in_frame(cx, handle, |window, cx| {
        let workspace_ref = workspace.read(cx);
        let terminal = |id| {
            panel_handle(
                workspace_ref
                    .remote_terminal(RemoteTerminalId(id))
                    .unwrap()
                    .clone(),
            )
        };
        let explorer = workspace_ref
            .explorer(ExplorerId(SFTP_TAB))
            .unwrap()
            .clone();
        let settings = workspace_ref.settings_tab().unwrap().clone();
        let editor = workspace_ref.editor(EditorId(1)).unwrap().clone();
        let layout = DockLayout::h_split()
            .child(
                DockLayout::v_split()
                    .child(DockLayout::tabs().panel_view(terminal(1), cx), None)
                    .child(
                        DockLayout::tabs().panel_view(panel_handle(explorer.clone()), cx),
                        None,
                    ),
                None,
            )
            .child(
                DockLayout::v_split()
                    .child(DockLayout::tabs().panel_view(terminal(2), cx), None)
                    .child(
                        DockLayout::tabs()
                            .panel_view(panel_handle(editor), cx)
                            .panel_view(panel_handle(settings.clone()), cx)
                            .active_index(0),
                        None,
                    ),
                None,
            );
        let area = workspace_ref.dock_area().clone();
        area.update(cx, |area, cx| area.set_center(layout, window, cx));
        (explorer.entity_id(), settings.entity_id())
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 1_u64), cx);
        window.dispatch_action(Box::new(ToggleComposePane), cx);
    });
    in_frame(cx, handle, |window, cx| window.input("draft command", cx));
    settle_layout(cx, handle);
    let (compose_before, compose_panels) = in_frame(cx, handle, |window, cx| {
        let panels = workspace
            .read(cx)
            .dock_area()
            .read(cx)
            .layout(DockPlacement::Bottom)
            .expect("compose dock")
            .panels()
            .collect::<Vec<_>>();
        assert_eq!(panels.len(), 1);
        (window.find("compose-pane").bounds(), panels)
    });
    let connection_count = provider.events.lock().unwrap().len();
    for edge in [Edge::Right, Edge::Bottom] {
        drag_terminal(cx, handle, 2, 1, edge);
        in_frame(cx, handle, |window, cx| {
            let workspace = workspace.read(cx);
            assert_eq!(
                workspace
                    .explorer(ExplorerId(SFTP_TAB))
                    .unwrap()
                    .entity_id(),
                explorer_identity
            );
            assert_eq!(
                workspace.settings_tab().unwrap().entity_id(),
                settings_identity
            );
            let editor = workspace.editor(EditorId(1)).unwrap();
            assert_eq!(editor.entity_id(), editor_identity);
            assert!(editor.read(cx).is_dirty());
            assert_eq!(editor.read(cx).state().read(cx).value().to_string(), draft);
            assert!(
                (window.find("compose-pane").bounds().size.height - compose_before.size.height)
                    .as_f32()
                    .abs()
                    <= 1.
            );
            assert_eq!(
                workspace
                    .dock_area()
                    .read(cx)
                    .layout(DockPlacement::Center)
                    .unwrap()
                    .panels()
                    .count(),
                5
            );
            assert_eq!(
                workspace
                    .dock_area()
                    .read(cx)
                    .layout(DockPlacement::Bottom)
                    .expect("compose dock retained")
                    .panels()
                    .collect::<Vec<_>>(),
                compose_panels
            );
        });
        assert_eq!(
            provider.requests.lock().unwrap().len(),
            1,
            "upload not restarted"
        );
        assert_eq!(*provider.reconnects.lock().unwrap(), 0);
        assert_eq!(
            provider.events.lock().unwrap().len(),
            connection_count,
            "SFTP connection reused"
        );
        assert_eq!(factory.starts(), 2, "terminal connections reused");
    }
    // Sending the retained draft proves both its text and its new target still work.
    in_frame(cx, handle, |window, cx| window.click("compose-send", cx));
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        factory.written_text().contains("draft command")
    })
    .await;
}

#[gpui_kit::test]
async fn a_new_pointer_focus_wins_over_pending_drag_focus_restoration(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(HostId(WEB_01))), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", 1_u64), cx)
    });
    drag_terminal(cx, handle, 3, 1, Edge::Right);
    in_frame(cx, handle, |window, cx| {
        let from = window.find(("terminal-tab", 2_u64)).bounds().center();
        let bounds = window.find(("terminal", 1_u64)).bounds();
        window.drag(from, point(bounds.center().x, bounds.bottom() - px(5.)), cx);
    });
    // The drag's entity effects ran, but its next-frame callback has not.
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 3_u64), cx)
    });
    settle_layout(cx, handle);
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find(("terminal", 3_u64)).focused(), Some(true));
        assert_eq!(
            workspace.read(cx).active_tab(),
            Some(CenterTab::Terminal(RemoteTerminalId(3)))
        );
    });
}

#[gpui_kit::test]
async fn a_dialog_opened_after_drop_keeps_its_focus(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", 1_u64), cx)
    });
    in_frame(cx, handle, |window, cx| {
        let from = window.find(("terminal-tab", 2_u64)).bounds().center();
        let bounds = window.find(("terminal", 1_u64)).bounds();
        window.drag(from, point(bounds.right() - px(5.), bounds.center().y), cx);
    });
    in_frame(cx, handle, |window, cx| window.click("new-host-panel", cx));
    in_frame(cx, handle, |window, cx| window.click("host-name", cx));
    settle_layout(cx, handle);
    in_frame(cx, handle, |window, cx| {
        window.input("dialog keeps input", cx);
        assert_eq!(window.find("host-name").value(), Some("dialog keeps input"));
        assert_eq!(window.find("host-name").focused(), Some(true));
    });
}

#[gpui_kit::test]
async fn sftp_then_drag_then_close_other_split_needs_no_focus_repair(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(HostId(WEB_01))), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", 1_u64), cx)
    });
    drag_terminal(cx, handle, 2, 1, Edge::Right);
    drag_terminal(cx, handle, 3, 1, Edge::Bottom);
    in_frame(cx, handle, |window, cx| window.click(("sftp", 2_u64), cx));
    in_frame(cx, handle, |_, cx| {
        let workspace = workspace.read(cx);
        let tree = workspace
            .dock_area()
            .read(cx)
            .layout(DockPlacement::Center)
            .unwrap();
        let terminal = workspace
            .remote_terminal(RemoteTerminalId(2))
            .unwrap()
            .entity_id()
            .into();
        let explorer = workspace
            .explorer(ExplorerId(1))
            .unwrap()
            .entity_id()
            .into();
        assert_eq!(
            tree.find_panel_node(terminal),
            tree.find_panel_node(explorer)
        );
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", 2_u64), cx)
    });
    for edge in [Edge::Right, Edge::Bottom] {
        drag_terminal(cx, handle, 2, 1, edge);
    }
    in_frame(cx, handle, |window, cx| {
        window.click(("close-terminal", 2_u64), cx)
    });
    // No click on terminal content after destroying the focused split.
    in_frame(cx, handle, |window, cx| {
        window.click(("close-terminal", 3_u64), cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert!(
            workspace
                .read(cx)
                .remote_terminal(RemoteTerminalId(2))
                .is_none()
        );
        assert!(
            workspace
                .read(cx)
                .remote_terminal(RemoteTerminalId(3))
                .is_none()
        );
        assert!(
            workspace
                .read(cx)
                .remote_terminal(RemoteTerminalId(1))
                .is_some()
        );
        assert!(workspace.read(cx).explorer(ExplorerId(1)).is_some());
        assert!(window.find(("terminal", 1_u64)).visible());
    });
}

#[gpui_kit::test]
async fn consecutive_drags_need_only_one_platform_frame_between_moves(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, _) = open_counted_workspace(cx, factory.clone());
    for _ in 0..2 {
        in_frame(cx, handle, |window, cx| {
            window.dispatch_action(Box::new(ConnectHost(HostId(WEB_01))), cx)
        });
    }
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", 1_u64), cx)
    });
    for (source, target, edge) in [
        (2, 1, Edge::Right),
        (3, 1, Edge::Bottom),
        (4, 2, Edge::Bottom),
    ] {
        drag_terminal(cx, handle, source, target, edge);
    }
    let stable = in_frame(cx, handle, |window, _| {
        window.find(("terminal", 3_u64)).bounds()
    });
    for right in [true, false, true, false] {
        // Do not use settle_layout: render exactly one platform callback
        // after native effects, like the next user event in the real window.
        in_frame(cx, handle, |window, cx| {
            let source = window.find(("terminal-tab", 2_u64)).bounds().center();
            let target = window.find(("terminal", 1_u64)).bounds();
            let destination = if right {
                point(target.right() - px(5.), target.center().y)
            } else {
                point(target.center().x, target.bottom() - px(5.))
            };
            window.drag(source, destination, cx);
        });
        in_frame(cx, handle, |window, cx| window.simulate_next_frame(cx));
        in_frame(cx, handle, |window, _| {
            let lower = window.find(("terminal", 3_u64)).bounds();
            assert!((lower.top() - stable.top()).as_f32().abs() <= 1.);
            assert!((lower.size.height - stable.size.height).as_f32().abs() <= 1.);
            if !right {
                let first = window.find(("terminal", 1_u64)).bounds();
                let second = window.find(("terminal", 2_u64)).bounds();
                assert!((first.size.height - second.size.height).as_f32().abs() <= 1.);
            }
        });
    }
    assert_eq!(factory.starts(), 4);
}
