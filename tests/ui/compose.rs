//! The remote tab's draft is local editing until submitted with a fresh Enter.
use crate::support::*;
use shellrs::app::{SubmitTerminalDraft, ToggleTerminalCompose};

async fn open_compose(
    cx: &mut TestAppContext,
    output: &'static str,
) -> (
    WindowHandle<Root>,
    Entity<Workspace>,
    Arc<FakeTerminalFactory>,
) {
    let factory = Arc::new(FakeTerminalFactory::printing(output));
    let (handle, workspace) =
        open_workspace_with_remote_factory(cx, HostStore::seed(), factory.clone());
    in_frame(cx, handle, |window, cx| {
        window.activate_window();
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
        remote_lifecycle(&workspace, RemoteTerminalId(INITIAL_WEB_TERMINAL), cx)
            == TerminalLifecycle::Running
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click(("toggle-terminal-compose", INITIAL_WEB_TERMINAL), cx);
    });
    (handle, workspace, factory)
}

#[gpui_kit::test]
async fn compose_shift_enter_edits_and_plain_enter_submits_once(cx: &mut TestAppContext) {
    let (handle, _, factory) = open_compose(cx, "\x1b[?2004h").await;
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-compose-input", INITIAL_WEB_TERMINAL), cx);
        window.input("echo 中文", cx);
        window.press("shift-enter", cx);
        window.input("echo second", cx);
    });
    assert_eq!(factory.written_text(), "");
    in_frame(cx, handle, |window, cx| {
        window.click(("toggle-terminal-compose", INITIAL_WEB_TERMINAL), cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(
            window
                .try_find(("terminal-compose", INITIAL_WEB_TERMINAL))
                .is_none()
        );
        window.click(("toggle-terminal-compose", INITIAL_WEB_TERMINAL), cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-compose-input", INITIAL_WEB_TERMINAL), cx);
        window.press("enter", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        factory.written_text() == "\x1b[200~echo 中文\necho second\x1b[201~\r"
    })
    .await;
}

#[gpui_kit::test]
async fn compose_rejects_multiline_without_bracketed_paste_and_disconnected_input(
    cx: &mut TestAppContext,
) {
    let (handle, workspace, factory) = open_compose(cx, "").await;
    let id = RemoteTerminalId(INITIAL_WEB_TERMINAL);
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-compose-input", id.0), cx);
        window.input("echo first\necho second", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.press("enter", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(factory.written_text(), "");
        window.dispatch_action(Box::new(DisconnectTerminal(id)), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        !remote_lifecycle(&workspace, id, cx).accepts_input()
    })
    .await;
    let starts = factory.starts();
    in_frame(cx, handle, |window, cx| {
        // The handler rechecks connection state even for direct actions.
        window.dispatch_action(Box::new(SubmitTerminalDraft(id)), cx);
        window.click(("terminal-compose-input", id.0), cx);
        window.press("enter", cx);
        window.input("echo still editing", cx);
    });
    cx.run_until_parked();
    assert_eq!(factory.written_text(), "");
    assert_eq!(factory.starts(), starts, "draft Enter must not reconnect");
    cx.update(|cx| {
        assert!(!remote_lifecycle(&workspace, id, cx).accepts_input());
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", id.0), cx);
        window.dispatch_action(Box::new(shellrs::app::ReconnectTerminal(id)), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
        remote_lifecycle(&workspace, id, cx) == TerminalLifecycle::Running
            && factory.starts() == starts + 1
    })
    .await;
    assert_eq!(factory.written_text(), "");
}

#[gpui_kit::test]
async fn compose_drafts_are_per_tab_and_removed_with_the_tab(cx: &mut TestAppContext) {
    let (handle, workspace, factory) = open_compose(cx, "").await;
    let first = INITIAL_WEB_TERMINAL;
    let second = INITIAL_STAGING_TERMINAL;
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-compose-input", first), cx);
        window.input("echo first", cx);
        window.click(("terminal-tab", second), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
        remote_lifecycle(&workspace, RemoteTerminalId(second), cx) == TerminalLifecycle::Running
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(
            Box::new(ToggleTerminalCompose(RemoteTerminalId(second))),
            cx,
        );
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-compose-input", second), cx);
        window.input("echo second", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.press("enter", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        factory.written_text() == "echo second\r"
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", first), cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-compose-input", first), cx);
        window.press("enter", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        factory.written_text() == "echo second\recho first\r"
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click(("close-terminal", first), cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find(("terminal-compose", first)).is_none());
        assert!(
            workspace
                .read(cx)
                .remote_terminal(RemoteTerminalId(first))
                .is_none()
        );
    });
}

#[gpui_kit::test]
async fn compose_top_divider_resizes_without_covering_the_terminal(cx: &mut TestAppContext) {
    use gpui_kit::{MouseDownEvent, MouseUpEvent};
    let (handle, _, factory) = open_compose(cx, "").await;
    let mut original_height = px(0.);
    in_frame(cx, handle, |window, cx| {
        let pane = window
            .find(("terminal-compose", INITIAL_WEB_TERMINAL))
            .bounds();
        original_height = pane.size.height;
        let start = point(pane.center().x, pane.top() - px(2.));
        window.dispatch_event(
            MouseDownEvent {
                button: MouseButton::Left,
                position: start,
                modifiers: gpui_kit::Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            }
            .to_platform_input(),
            cx,
        );
        for dy in [10., 100.] {
            window.dispatch_event(
                MouseMoveEvent {
                    position: point(start.x, start.y - px(dy)),
                    pressed_button: Some(MouseButton::Left),
                    modifiers: gpui_kit::Modifiers::default(),
                }
                .to_platform_input(),
                cx,
            );
            window.render_frame(cx);
        }
        window.dispatch_event(
            MouseUpEvent {
                button: MouseButton::Left,
                position: point(start.x, start.y - px(100.)),
                modifiers: gpui_kit::Modifiers::default(),
                click_count: 1,
            }
            .to_platform_input(),
            cx,
        );
    });
    in_frame(cx, handle, |window, _| {
        let pane = window
            .find(("terminal-compose", INITIAL_WEB_TERMINAL))
            .bounds();
        let terminal = window.find(("terminal", INITIAL_WEB_TERMINAL)).bounds();
        assert!(pane.size.height > original_height);
        assert!(pane.size.height >= px(80.));
        assert!(terminal.size.height >= px(80.));
        assert!(terminal.bottom() <= pane.top() + px(1.));
    });
    assert_eq!(factory.written_text(), "");
}

#[gpui_kit::test]
async fn compose_success_clears_draft_keeps_focus_and_ignores_repeat(cx: &mut TestAppContext) {
    use gpui_kit::{KeyDownEvent, Keystroke};
    let (handle, _, factory) = open_compose(cx, "").await;
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-compose-input", INITIAL_WEB_TERMINAL), cx);
        window.input("echo one", cx);
        window.press("enter", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        factory.written_text() == "echo one\r"
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        // Empty Enter and OS key-repeat cannot resend the previous command.
        window.press("enter", cx);
        window.input("echo two", cx);
        window.dispatch_event(
            KeyDownEvent {
                keystroke: Keystroke::parse("enter").unwrap(),
                is_held: true,
                prefer_character_input: false,
            }
            .to_platform_input(),
            cx,
        );
    });
    cx.run_until_parked();
    assert_eq!(factory.written_text(), "echo one\r");
    in_frame(cx, handle, |window, cx| {
        window.press("enter", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        factory.written_text() == "echo one\recho two\r"
    })
    .await;
}

#[gpui_kit::test]
async fn compose_does_not_submit_while_find_or_another_tab_has_focus(cx: &mut TestAppContext) {
    let (handle, _, factory) = open_compose(cx, "").await;
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-compose-input", INITIAL_WEB_TERMINAL), cx);
        window.input("echo retained", cx);
        window.dispatch_action(Box::new(FindInTerminal), cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.press("enter", cx);
    });
    assert_eq!(factory.written_text(), "");
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_STAGING_TERMINAL), cx);
        window.dispatch_action(
            Box::new(SubmitTerminalDraft(RemoteTerminalId(INITIAL_WEB_TERMINAL))),
            cx,
        );
    });
    cx.run_until_parked();
    assert_eq!(factory.written_text(), "");
}

#[gpui_kit::test]
async fn compose_cancels_queued_enter_when_focus_moves_before_dispatch(cx: &mut TestAppContext) {
    let (handle, _, factory) = open_compose(cx, "").await;
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-compose-input", INITIAL_WEB_TERMINAL), cx);
        window.input("echo never", cx);
        window.press("enter", cx);
        window.click(("terminal-tab", INITIAL_STAGING_TERMINAL), cx);
    });
    cx.run_until_parked();
    assert_eq!(factory.written_text(), "");
}
