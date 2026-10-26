//! Tabs and their panes: a tab dropped on a pane's edge joins that pane's
//! tab, a pane dragged by its header moves within its tab, to another tab
//! or out to a tab of its own, and tabs move along the tab bar. Every move
//! keeps the panes' connections.

use crate::support::*;
use gpui_kit::component::Placement;
use gpui_kit::{Modifiers, MouseDownEvent, MouseUpEvent, Pixels, Point};
use shellrs::app::{OpenSettings, PaneContent};
use shellrs::center::TabId;

const WEB: PaneContent = PaneContent::Terminal(RemoteTerminalId(INITIAL_WEB_TERMINAL));
const STAGING: PaneContent = PaneContent::Terminal(RemoteTerminalId(INITIAL_STAGING_TERMINAL));
const LOCAL: PaneContent = PaneContent::LocalTerminal(LocalTerminalId(1));

/// The tabs, each by its panes in reading order.
fn layout(cx: &mut TestAppContext, workspace: &Entity<Workspace>) -> Vec<Vec<PaneContent>> {
    cx.read(|cx| {
        let center = workspace.read(cx).center().read(cx);
        center
            .tab_ids()
            .into_iter()
            .map(|tab| center.panes(tab).to_vec())
            .collect()
    })
}

fn tab_of(cx: &mut TestAppContext, workspace: &Entity<Workspace>, pane: PaneContent) -> TabId {
    cx.read(|cx| {
        workspace
            .read(cx)
            .center()
            .read(cx)
            .tab_of(pane)
            .expect("an open pane")
    })
}

fn front(cx: &mut TestAppContext, workspace: &Entity<Workspace>) -> Option<TabId> {
    cx.read(|cx| workspace.read(cx).center().read(cx).active_tab())
}

/// Near one side of an element, well inside the band that splits there.
fn near(bounds: gpui_kit::Bounds<Pixels>, side: Placement) -> Point<Pixels> {
    let inset = px(12.);
    let center = bounds.center();
    match side {
        Placement::Left => point(bounds.left() + inset, center.y),
        Placement::Right => point(bounds.right() - inset, center.y),
        Placement::Top => point(center.x, bounds.top() + inset),
        Placement::Bottom => point(center.x, bounds.bottom() - inset),
    }
}

/// At one end of a tab, where a tab dropped moves along the bar.
fn near_end(bounds: gpui_kit::Bounds<Pixels>, side: Placement) -> Point<Pixels> {
    let inset = px(4.);
    let center = bounds.center();
    match side {
        Placement::Left => point(bounds.left() + inset, center.y),
        _ => point(bounds.right() - inset, center.y),
    }
}

fn press(window: &mut gpui_kit::Window, at: Point<Pixels>, cx: &mut App) {
    window.dispatch_event(
        MouseDownEvent {
            button: MouseButton::Left,
            position: at,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        }
        .to_platform_input(),
        cx,
    );
}

fn drag_to(window: &mut gpui_kit::Window, to: Point<Pixels>, cx: &mut App) {
    window.dispatch_event(
        MouseMoveEvent {
            position: to,
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        }
        .to_platform_input(),
        cx,
    );
}

fn release(window: &mut gpui_kit::Window, at: Point<Pixels>, cx: &mut App) {
    window.dispatch_event(
        MouseUpEvent {
            button: MouseButton::Left,
            position: at,
            modifiers: Modifiers::default(),
            click_count: 1,
        }
        .to_platform_input(),
        cx,
    );
}

/// Drag what is at `from`'s center to `side` of `onto`.
fn drag_onto(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    from: impl Into<ElementId>,
    onto: impl Into<ElementId>,
    side: Placement,
) {
    let (from, onto) = (from.into(), onto.into());
    in_frame(cx, handle, |window, cx| {
        let start = window.find(from).bounds().center();
        let end = near(window.find(onto).bounds(), side);
        window.drag(start, end, cx);
    });
}

#[gpui_kit::test]
fn a_tab_dropped_on_a_panes_side_joins_its_tab(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    assert_eq!(layout(cx, &workspace), [vec![WEB], vec![STAGING]]);

    drag_onto(
        cx,
        handle,
        ("terminal-tab", INITIAL_STAGING_TERMINAL),
        ("terminal", INITIAL_WEB_TERMINAL),
        Placement::Right,
    );
    assert_eq!(layout(cx, &workspace), [vec![WEB, STAGING]]);
    in_frame(cx, handle, |window, _| {
        let web = window.find(("terminal", INITIAL_WEB_TERMINAL)).bounds();
        let staging = window.find(("terminal", INITIAL_STAGING_TERMINAL)).bounds();
        assert!(web.right() <= staging.left());
        // Each pane has a header now, named after it; the tab is named
        // after both.
        assert!(
            window
                .find(("terminal-tab", INITIAL_WEB_TERMINAL))
                .visible()
        );
        assert!(
            window
                .find(("terminal-tab", INITIAL_STAGING_TERMINAL))
                .visible()
        );
    });
    // The pane dropped is the one in front.
    let active = cx.read(|cx| workspace.read(cx).active_pane());
    assert_eq!(active, Some(STAGING));
}

#[gpui_kit::test]
fn the_middle_of_a_pane_takes_nothing(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| {
        let start = window
            .find(("terminal-tab", INITIAL_STAGING_TERMINAL))
            .bounds()
            .center();
        let end = window
            .find(("terminal", INITIAL_WEB_TERMINAL))
            .bounds()
            .center();
        window.drag(start, end, cx);
    });
    assert_eq!(layout(cx, &workspace), [vec![WEB], vec![STAGING]]);
}

#[gpui_kit::test]
fn a_pane_moves_by_its_header_within_its_tab_and_out_to_one_of_its_own(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    split_beside(cx, handle, &workspace, STAGING, WEB, Placement::Right);

    // staging-api's header onto web-01's bottom: below it now.
    drag_onto(
        cx,
        handle,
        ("terminal-tab", INITIAL_STAGING_TERMINAL),
        ("terminal", INITIAL_WEB_TERMINAL),
        Placement::Bottom,
    );
    assert_eq!(layout(cx, &workspace), [vec![WEB, STAGING]]);
    in_frame(cx, handle, |window, _| {
        let web = window.find(("terminal", INITIAL_WEB_TERMINAL)).bounds();
        let staging = window.find(("terminal", INITIAL_STAGING_TERMINAL)).bounds();
        assert!(web.bottom() <= staging.top());
    });

    // Past the last tab: a tab of its own, there and in front.
    in_frame(cx, handle, |window, cx| {
        window.drag_to(
            ("terminal-tab", INITIAL_STAGING_TERMINAL),
            "tab-bar-empty-space",
            cx,
        )
    });
    assert_eq!(layout(cx, &workspace), [vec![WEB], vec![STAGING]]);
    assert_eq!(front(cx, &workspace), Some(tab_of(cx, &workspace, STAGING)));
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .find(("terminal", INITIAL_STAGING_TERMINAL))
                .visible()
        );
        assert!(
            window
                .try_find(("terminal", INITIAL_WEB_TERMINAL))
                .is_none()
        );
    });
}

#[gpui_kit::test]
async fn a_pane_dragged_over_another_tab_brings_it_forward_to_drop_into(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(NewLocalTerminal), cx)
    });
    split_beside(cx, handle, &workspace, STAGING, WEB, Placement::Right);
    assert_eq!(layout(cx, &workspace), [vec![WEB, STAGING], vec![LOCAL]]);

    // staging-api's header held over the local terminal's tab a moment.
    in_frame(cx, handle, |window, cx| {
        let header = window
            .find(("terminal-tab", INITIAL_STAGING_TERMINAL))
            .bounds()
            .center();
        let tab = window.find(("local-terminal-tab", 1_u64)).bounds().center();
        press(window, header, cx);
        drag_to(window, point(header.x + px(20.), header.y + px(20.)), cx);
        drag_to(window, tab, cx);
    });
    cx.background_executor
        .advance_clock(Duration::from_millis(600));
    cx.run_until_parked();
    assert_eq!(front(cx, &workspace), Some(tab_of(cx, &workspace, LOCAL)));

    // Then dropped on the left of the local terminal.
    in_frame(cx, handle, |window, cx| {
        let end = near(
            window.find(("local-terminal", 1_u64)).bounds(),
            Placement::Left,
        );
        drag_to(window, end, cx);
        release(window, end, cx);
    });
    assert_eq!(layout(cx, &workspace), [vec![WEB], vec![STAGING, LOCAL]]);
}

#[gpui_kit::test]
fn a_pane_dropped_on_a_tab_goes_into_it_on_the_right(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(NewLocalTerminal), cx)
    });
    split_beside(cx, handle, &workspace, STAGING, WEB, Placement::Right);
    in_frame(cx, handle, |window, cx| {
        window.drag_to(
            ("terminal-tab", INITIAL_STAGING_TERMINAL),
            ("local-terminal-tab", 1_u64),
            cx,
        )
    });
    assert_eq!(layout(cx, &workspace), [vec![WEB], vec![LOCAL, STAGING]]);
}

#[gpui_kit::test]
fn tabs_move_along_the_bar_and_one_of_several_panes_joins_no_other(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(NewLocalTerminal), cx)
    });
    // The local terminal's tab onto the left end of web-01's: before it.
    in_frame(cx, handle, |window, cx| {
        let start = window.find(("local-terminal-tab", 1_u64)).bounds().center();
        let end = near_end(
            window.find(("terminal-tab", INITIAL_WEB_TERMINAL)).bounds(),
            Placement::Left,
        );
        window.drag(start, end, cx);
    });
    assert_eq!(
        layout(cx, &workspace),
        [vec![LOCAL], vec![WEB], vec![STAGING]]
    );

    // A tab of two panes, dragged onto a pane, stays as it is; onto either
    // end of a tab, it moves there.
    split_beside(cx, handle, &workspace, STAGING, WEB, Placement::Right);
    let split = tab_of(cx, &workspace, WEB);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(shellrs::app::SwitchToTab(1)), cx)
    });
    in_frame(cx, handle, |window, cx| {
        let start = window.find(("pane-tab", split.0)).bounds().center();
        let end = near(
            window.find(("local-terminal", 1_u64)).bounds(),
            Placement::Right,
        );
        window.drag(start, end, cx);
    });
    assert_eq!(layout(cx, &workspace), [vec![LOCAL], vec![WEB, STAGING]]);
    in_frame(cx, handle, |window, cx| {
        let start = window.find(("pane-tab", split.0)).bounds().center();
        let end = near_end(
            window.find(("local-terminal-tab", 1_u64)).bounds(),
            Placement::Left,
        );
        window.drag(start, end, cx);
    });
    assert_eq!(layout(cx, &workspace), [vec![WEB, STAGING], vec![LOCAL]]);
}

#[gpui_kit::test]
fn settings_shares_its_tab_with_nothing(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(OpenSettings), cx)
    });
    // web-01's tab dropped on 设置: nothing lands.
    drag_onto(
        cx,
        handle,
        ("terminal-tab", INITIAL_WEB_TERMINAL),
        "settings",
        Placement::Right,
    );
    assert_eq!(
        layout(cx, &workspace),
        [vec![WEB], vec![STAGING], vec![PaneContent::Settings]]
    );
    // 设置's tab dropped on web-01: nothing either.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(shellrs::app::SwitchToTab(1)), cx)
    });
    drag_onto(
        cx,
        handle,
        "settings-tab",
        ("terminal", INITIAL_WEB_TERMINAL),
        Placement::Right,
    );
    assert_eq!(
        layout(cx, &workspace),
        [vec![WEB], vec![STAGING], vec![PaneContent::Settings]]
    );
}

/// 放大窗格 fills a split tab with the pane that has the keyboard, until it
/// is zoomed out again; a tab of one pane has nothing to zoom.
#[gpui_kit::test]
fn a_zoomed_pane_fills_its_tab(cx: &mut TestAppContext) {
    use gpui_kit::component::dock::ToggleZoom;

    let (handle, workspace) = open_workspace(cx);
    let visible = |cx: &mut TestAppContext| {
        cx.read(|cx| workspace.read(cx).center().read(cx).visible_panes(cx))
    };
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", INITIAL_WEB_TERMINAL), cx);
        window.dispatch_action(Box::new(ToggleZoom), cx);
    });
    assert_eq!(visible(cx), [WEB]);
    let zoomed = cx.read(|cx| {
        let center = workspace.read(cx).center().read(cx);
        center.tab_ids().len()
    });
    assert_eq!(zoomed, 2);

    split_beside(cx, handle, &workspace, STAGING, WEB, Placement::Right);
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", INITIAL_WEB_TERMINAL), cx);
        window.dispatch_action(Box::new(ToggleZoom), cx);
    });
    assert_eq!(visible(cx), [WEB]);
    in_frame(cx, handle, |window, cx| {
        assert!(
            window
                .try_find(("terminal", INITIAL_STAGING_TERMINAL))
                .is_none()
        );
        window.dispatch_action(Box::new(ToggleZoom), cx);
    });
    assert_eq!(visible(cx), [WEB, STAGING]);
}

#[gpui_kit::test]
fn a_tab_of_several_panes_is_named_after_them_and_counts_them(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    split_beside(cx, handle, &workspace, STAGING, WEB, Placement::Right);
    let tab = tab_of(cx, &workspace, WEB);
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("pane-tab", tab.0)).label(),
            Some("web-01、staging-api")
        );
        assert_eq!(
            window
                .within(("pane-tab", tab.0))
                .find("pane-count")
                .label(),
            Some("2 个窗格")
        );
    });

    // Named by hand, it keeps the name as panes come and go.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(shellrs::app::RenameTab(tab)), cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("tab-name").value(), Some("web-01、staging-api"));
        window.click("tab-name", cx);
        window.press("secondary-a", cx);
        window.input("部署", cx);
        window.click("commit", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find(("pane-tab", tab.0)).label(), Some("部署"));
        // The dialog gone, nothing has the keyboard until a click.
        window.click(("terminal", INITIAL_WEB_TERMINAL), cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(NewLocalTerminal), cx)
    });
    split_beside(cx, handle, &workspace, LOCAL, WEB, Placement::Bottom);
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find(("pane-tab", tab.0)).label(), Some("部署"));
        assert_eq!(
            window
                .within(("pane-tab", tab.0))
                .find("pane-count")
                .label(),
            Some("3 个窗格")
        );
    });
}

#[gpui_kit::test]
fn closing_a_tab_of_several_panes_asks_first(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    split_beside(cx, handle, &workspace, STAGING, WEB, Placement::Right);
    let tab = tab_of(cx, &workspace, WEB);
    in_frame(cx, handle, |window, cx| {
        window.click(("close-tab", tab.0), cx)
    });
    assert_eq!(layout(cx, &workspace), [vec![WEB, STAGING]]);
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    assert!(layout(cx, &workspace).is_empty());
    in_frame(cx, handle, |window, _| {
        assert!(window.find("recent-hosts").visible())
    });
}

#[gpui_kit::test]
fn closing_the_others_asks_first_when_they_hold_several_panes(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(NewLocalTerminal), cx)
    });
    split_beside(cx, handle, &workspace, STAGING, WEB, Placement::Right);
    let close_others = shellrs::app::CloseTabs {
        tab: LOCAL,
        scope: CloseScope::Others,
    };
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(close_others.clone()), cx)
    });
    assert_eq!(layout(cx, &workspace), [vec![WEB, STAGING], vec![LOCAL]]);
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    assert_eq!(layout(cx, &workspace), [vec![LOCAL]]);
}

#[gpui_kit::test]
fn a_panes_menu_moves_it_to_a_new_tab_and_zooms_it(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(NewLocalTerminal), cx)
    });
    split_beside(cx, handle, &workspace, STAGING, WEB, Placement::Right);
    let visible = |cx: &mut TestAppContext| {
        cx.read(|cx| workspace.read(cx).center().read(cx).visible_panes(cx))
    };

    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(shellrs::app::ZoomPane(STAGING)), cx)
    });
    assert_eq!(visible(cx), [STAGING]);
    let place = cx.read(|cx| shellrs::center::tab_place(STAGING, cx));
    assert!(place.is_some_and(|place| place.zoomed && place.panes == 2));
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(shellrs::app::ZoomPane(STAGING)), cx)
    });
    assert_eq!(visible(cx), [WEB, STAGING]);

    // Out to a tab of its own, right after the one it leaves.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(
            Box::new(shellrs::app::MovePaneToNewTab {
                pane: WEB,
                ix: None,
            }),
            cx,
        )
    });
    assert_eq!(
        layout(cx, &workspace),
        [vec![STAGING], vec![WEB], vec![LOCAL]]
    );
    assert_eq!(front(cx, &workspace), Some(tab_of(cx, &workspace, WEB)));
}

/// 所有标签 lists the tabs, and under a tab of several panes its panes;
/// a row brings its tab or pane to the front. (Its rows dispatch these
/// actions; menus are not driven here.)
#[gpui_kit::test]
fn all_tabs_reaches_every_tab_and_pane(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    split_beside(cx, handle, &workspace, STAGING, WEB, Placement::Right);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(NewLocalTerminal), cx)
    });
    in_frame(cx, handle, |window, cx| window.click("all-tabs", cx));
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("popup-menu").visible());
        window.press("escape", cx);
    });

    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(shellrs::app::ActivatePane(WEB)), cx)
    });
    assert_eq!(front(cx, &workspace), Some(tab_of(cx, &workspace, WEB)));
    assert_eq!(cx.read(|cx| workspace.read(cx).active_pane()), Some(WEB));

    let local = tab_of(cx, &workspace, LOCAL);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(shellrs::app::ActivateTab(local)), cx)
    });
    assert_eq!(front(cx, &workspace), Some(local));
    assert_eq!(cx.read(|cx| workspace.read(cx).active_pane()), Some(LOCAL));
}

/// With nothing focused, as after some dialogs close, a tab's × still
/// closes it.
#[gpui_kit::test]
fn a_tab_closes_from_its_button_with_nothing_focused(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| {
        window.blur(cx);
        assert!(window.focused(cx).is_none());
        window.click(("close-terminal", INITIAL_STAGING_TERMINAL), cx)
    });
    assert_eq!(layout(cx, &workspace), [vec![WEB]]);
}

/// Each pane of a split has its own controls on its header, its latency
/// and buttons; a tab of one pane has them on the tab bar.
#[gpui_kit::test]
fn each_pane_of_a_split_has_its_own_controls(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    in_frame(cx, handle, |window, _| {
        assert!(window.find(("sftp", INITIAL_WEB_TERMINAL)).visible());
        assert!(
            window
                .try_find(("sftp", INITIAL_STAGING_TERMINAL))
                .is_none()
        );
    });
    split_beside(cx, handle, &workspace, STAGING, WEB, Placement::Right);
    in_frame(cx, handle, |window, _| {
        for id in [INITIAL_WEB_TERMINAL, INITIAL_STAGING_TERMINAL] {
            let terminal = window.find(("terminal", id)).bounds();
            for button in ["sftp", "reconnect"] {
                let control = window.find((button, id)).bounds();
                // On its own pane's header, above it.
                assert!(control.left() >= terminal.left() && control.right() <= terminal.right());
                assert!(control.bottom() <= terminal.top());
            }
        }
    });
}

/// The tab bar's switch shows and hides 撰写窗格.
#[gpui_kit::test]
fn the_tab_bar_switches_the_compose_pane(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("compose-pane").is_none());
        assert_eq!(window.find("toggle-compose-pane").checked(), Some(false));
        window.click("toggle-compose-pane", cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("compose-pane").visible());
        assert_eq!(window.find("toggle-compose-pane").checked(), Some(true));
        window.click("toggle-compose-pane", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("compose-pane").is_none());
    });
}

fn bounds_of(window: &mut gpui_kit::Window, pane: PaneContent) -> gpui_kit::Bounds<Pixels> {
    match pane {
        PaneContent::Terminal(id) => window.find(("terminal", id.0)).bounds(),
        PaneContent::LocalTerminal(id) => window.find(("local-terminal", id.0)).bounds(),
        other => panic!("no element for {other:?}"),
    }
}

/// About the same size, within the width of a divider or two.
fn about(a: Pixels, b: Pixels) -> bool {
    (a - b).abs() <= px(8.)
}

fn open_local_terminals(cx: &mut TestAppContext, handle: WindowHandle<Root>, count: usize) {
    for _ in 0..count {
        in_frame(cx, handle, |window, cx| {
            window.dispatch_action(Box::new(NewLocalTerminal), cx)
        });
    }
}

const LOCAL_2: PaneContent = PaneContent::LocalTerminal(LocalTerminalId(2));
const LOCAL_3: PaneContent = PaneContent::LocalTerminal(LocalTerminalId(3));

/// In a grid of four, a pane moved beside another joins that pane's row,
/// which then shares its space equally: no pane is left a sliver, as the
/// dock left them when it sized rows itself.
#[gpui_kit::test]
fn moving_panes_in_a_grid_shares_the_space_equally(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    open_local_terminals(cx, handle, 2);
    // web-01 and staging-api above, the two local terminals below.
    split_beside(cx, handle, &workspace, STAGING, WEB, Placement::Right);
    split_beside(cx, handle, &workspace, LOCAL, WEB, Placement::Bottom);
    split_beside(cx, handle, &workspace, LOCAL_2, STAGING, Placement::Bottom);

    // The first local terminal to the right of web-01: three columns.
    split_beside(cx, handle, &workspace, LOCAL, WEB, Placement::Right);
    in_frame(cx, handle, |window, _| {
        let [web, local, staging, local_2] =
            [WEB, LOCAL, STAGING, LOCAL_2].map(|pane| bounds_of(window, pane));
        assert!(web.right() <= local.left() && local.right() <= staging.left());
        assert!(about(web.size.width, local.size.width));
        assert!(about(web.size.width, staging.size.width));
        assert!(about(staging.size.width, local_2.size.width));
        assert!(staging.bottom() <= local_2.top());
        assert!(about(web.size.height, local.size.height));
    });

    // Then staging-api below web-01: the second local terminal has its
    // column to itself, and web-01 and staging-api share theirs.
    split_beside(cx, handle, &workspace, STAGING, WEB, Placement::Bottom);
    assert_eq!(layout(cx, &workspace), [vec![WEB, STAGING, LOCAL, LOCAL_2]]);
    in_frame(cx, handle, |window, _| {
        let [web, staging, local, local_2] =
            [WEB, STAGING, LOCAL, LOCAL_2].map(|pane| bounds_of(window, pane));
        assert!(web.bottom() <= staging.top());
        assert!(about(web.size.height, staging.size.height));
        for column in [local, local_2] {
            assert!(about(web.size.width, column.size.width));
            assert!(column.size.height > web.size.height * 1.8);
        }
    });

    // 均分窗格 leaves an even layout as it is, and every pane connected.
    let tab = tab_of(cx, &workspace, WEB);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(shellrs::app::EqualizePanes(tab)), cx)
    });
    assert_eq!(layout(cx, &workspace), [vec![WEB, STAGING, LOCAL, LOCAL_2]]);
}

#[gpui_kit::test]
fn a_pane_dropped_on_the_middle_of_another_trades_places_with_it(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    split_beside(cx, handle, &workspace, STAGING, WEB, Placement::Right);
    in_frame(cx, handle, |window, cx| {
        let start = window
            .find(("terminal-tab", INITIAL_STAGING_TERMINAL))
            .bounds()
            .center();
        let end = window
            .find(("terminal", INITIAL_WEB_TERMINAL))
            .bounds()
            .center();
        window.drag(start, end, cx);
    });
    assert_eq!(layout(cx, &workspace), [vec![STAGING, WEB]]);
    in_frame(cx, handle, |window, _| {
        let [staging, web] = [STAGING, WEB].map(|pane| bounds_of(window, pane));
        assert!(staging.right() <= web.left());
        assert!(about(staging.size.width, web.size.width));
    });
    let active = cx.read(|cx| workspace.read(cx).active_pane());
    assert_eq!(active, Some(STAGING));
}

/// A tab dropped on the middle of another goes into it, along its right
/// side; on either end of it, the tab moves along the bar.
#[gpui_kit::test]
fn a_tab_dropped_on_the_middle_of_another_joins_it(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    open_local_terminals(cx, handle, 1);
    in_frame(cx, handle, |window, cx| {
        let start = window.find(("local-terminal-tab", 1_u64)).bounds().center();
        let end = near_end(
            window
                .find(("terminal-tab", INITIAL_STAGING_TERMINAL))
                .bounds(),
            Placement::Right,
        );
        window.drag(start, end, cx);
    });
    assert_eq!(
        layout(cx, &workspace),
        [vec![WEB], vec![STAGING], vec![LOCAL]]
    );
    in_frame(cx, handle, |window, cx| {
        let start = window.find(("local-terminal-tab", 1_u64)).bounds().center();
        let end = near_end(
            window
                .find(("terminal-tab", INITIAL_STAGING_TERMINAL))
                .bounds(),
            Placement::Left,
        );
        window.drag(start, end, cx);
    });
    assert_eq!(
        layout(cx, &workspace),
        [vec![WEB], vec![LOCAL], vec![STAGING]]
    );

    in_frame(cx, handle, |window, cx| {
        window.drag_to(
            ("local-terminal-tab", 1_u64),
            ("terminal-tab", INITIAL_STAGING_TERMINAL),
            cx,
        )
    });
    assert_eq!(layout(cx, &workspace), [vec![WEB], vec![STAGING, LOCAL]]);
    assert_eq!(front(cx, &workspace), Some(tab_of(cx, &workspace, LOCAL)));
    in_frame(cx, handle, |window, _| {
        let [staging, local] = [STAGING, LOCAL].map(|pane| bounds_of(window, pane));
        assert!(staging.right() <= local.left());
    });
}

/// Held over the middle of a tab of several panes, a tab brings it forward,
/// and can then be dropped beside any of its panes.
#[gpui_kit::test]
async fn a_tab_held_over_a_split_tab_brings_it_forward_to_drop_into(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    open_local_terminals(cx, handle, 1);
    split_beside(cx, handle, &workspace, STAGING, WEB, Placement::Right);
    let split = tab_of(cx, &workspace, WEB);
    assert_eq!(layout(cx, &workspace), [vec![WEB, STAGING], vec![LOCAL]]);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(shellrs::app::SwitchToTab(2)), cx)
    });
    assert_eq!(front(cx, &workspace), Some(tab_of(cx, &workspace, LOCAL)));

    in_frame(cx, handle, |window, cx| {
        let start = window.find(("local-terminal-tab", 1_u64)).bounds().center();
        let tab = window.find(("pane-tab", split.0)).bounds().center();
        press(window, start, cx);
        drag_to(window, point(start.x + px(20.), start.y + px(20.)), cx);
        drag_to(window, tab, cx);
    });
    cx.background_executor
        .advance_clock(Duration::from_millis(600));
    cx.run_until_parked();
    assert_eq!(front(cx, &workspace), Some(split));

    in_frame(cx, handle, |window, cx| {
        let end = near(
            window.find(("terminal", INITIAL_WEB_TERMINAL)).bounds(),
            Placement::Bottom,
        );
        drag_to(window, end, cx);
        release(window, end, cx);
    });
    assert_eq!(layout(cx, &workspace), [vec![WEB, LOCAL, STAGING]]);
    in_frame(cx, handle, |window, _| {
        let [web, local] = [WEB, LOCAL].map(|pane| bounds_of(window, pane));
        assert!(web.bottom() <= local.top());
    });
}

/// The tab in front dropped on a side of its own pane: 选择标签 asks which
/// tab goes there, and the tab chosen does.
#[gpui_kit::test]
fn a_tab_dropped_on_its_own_pane_asks_which_tab_goes_there(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    assert_eq!(front(cx, &workspace), Some(tab_of(cx, &workspace, WEB)));
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("popup-menu").is_none());
    });
    drag_onto(
        cx,
        handle,
        ("terminal-tab", INITIAL_WEB_TERMINAL),
        ("terminal", INITIAL_WEB_TERMINAL),
        Placement::Right,
    );
    in_frame(cx, handle, |window, _| {
        assert!(window.find("popup-menu").visible());
    });
    assert_eq!(layout(cx, &workspace), [vec![WEB], vec![STAGING]]);

    // What choosing staging-api's tab does.
    let tab = tab_of(cx, &workspace, WEB);
    let join = shellrs::app::JoinTab {
        pane: STAGING,
        tab,
        placement: Placement::Right,
    };
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(join.clone()), cx)
    });
    assert_eq!(layout(cx, &workspace), [vec![WEB, STAGING]]);

    // 向下分屏… asks the same.
    open_local_terminals(cx, handle, 1);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(
            Box::new(shellrs::app::ChooseTabToJoin {
                tab,
                placement: Placement::Bottom,
            }),
            cx,
        )
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.find("popup-menu").visible());
    });
}

/// A drop that would leave a pane too small to use does nothing: past four
/// columns in this window, a fifth would.
#[gpui_kit::test]
fn nothing_lands_where_a_pane_would_be_too_small(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    open_local_terminals(cx, handle, 3);
    split_beside(cx, handle, &workspace, STAGING, WEB, Placement::Right);
    split_beside(cx, handle, &workspace, LOCAL, STAGING, Placement::Right);
    split_beside(cx, handle, &workspace, LOCAL_2, LOCAL, Placement::Right);
    let row = vec![WEB, STAGING, LOCAL, LOCAL_2];
    assert_eq!(layout(cx, &workspace), [row.clone(), vec![LOCAL_3]]);

    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(shellrs::app::SwitchToTab(1)), cx)
    });
    let local_3_tab = ("local-terminal-tab", 3_u64);
    drag_onto(
        cx,
        handle,
        local_3_tab,
        ("local-terminal", 2_u64),
        Placement::Right,
    );
    assert_eq!(layout(cx, &workspace), [row.clone(), vec![LOCAL_3]]);

    // Below one of them there is room.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(shellrs::app::SwitchToTab(1)), cx)
    });
    drag_onto(
        cx,
        handle,
        local_3_tab,
        ("local-terminal", 2_u64),
        Placement::Bottom,
    );
    assert_eq!(
        layout(cx, &workspace),
        [vec![WEB, STAGING, LOCAL, LOCAL_2, LOCAL_3]]
    );
}

/// A pane closing gives its space to the rest of its row.
#[gpui_kit::test]
fn a_pane_closing_gives_its_space_to_its_row(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    open_local_terminals(cx, handle, 1);
    split_beside(cx, handle, &workspace, STAGING, WEB, Placement::Right);
    split_beside(cx, handle, &workspace, LOCAL, STAGING, Placement::Right);
    in_frame(cx, handle, |window, _| {
        let [web, staging, local] = [WEB, STAGING, LOCAL].map(|pane| bounds_of(window, pane));
        assert!(about(web.size.width, staging.size.width));
        assert!(about(web.size.width, local.size.width));
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(
            Box::new(shellrs::app::CloseTerminal(RemoteTerminalId(
                INITIAL_STAGING_TERMINAL,
            ))),
            cx,
        )
    });
    assert_eq!(layout(cx, &workspace), [vec![WEB, LOCAL]]);
    in_frame(cx, handle, |window, _| {
        let [web, local] = [WEB, LOCAL].map(|pane| bounds_of(window, pane));
        assert!(about(web.size.width, local.size.width));
    });
}
