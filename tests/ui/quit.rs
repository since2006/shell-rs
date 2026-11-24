//! Quitting asks first while any tab is open, the start page being all
//! there is otherwise (退出前确认, issue #152), unless that is turned off.

use crate::support::*;
use shellrs::app::Quit;

/// Quit as ⌘Q does: from the app, wherever focus is.
fn quit(cx: &mut TestAppContext) {
    cx.update(|cx| cx.dispatch_action(&Quit));
    cx.run_until_parked();
}

/// The confirmation's button, while it is up.
fn confirmation(cx: &mut TestAppContext, handle: WindowHandle<Root>) -> Option<String> {
    in_frame(cx, handle, |window, _| {
        window
            .try_find("ok")
            .and_then(|ok| ok.label().map(str::to_string))
    })
}

fn set_confirm_quit(workspace: &Entity<Workspace>, on: bool, cx: &mut TestAppContext) {
    cx.update(|cx| {
        workspace
            .read(cx)
            .settings()
            .clone()
            .update(cx, |store, cx| {
                store.update(|settings| settings.window.confirm_quit = on, cx);
            });
    });
}

/// With no host connected at start: the start page is all there is.
fn open_start_page(cx: &mut TestAppContext) -> (WindowHandle<Root>, Entity<Workspace>) {
    open_workspace_with_store(cx, HostStore::empty())
}

fn open_local_terminal(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(NewLocalTerminal), cx)
    });
}

#[gpui_kit::test]
async fn the_start_page_alone_quits_without_asking(cx: &mut TestAppContext) {
    let (handle, _) = open_start_page(cx);
    quit(cx);
    assert_eq!(confirmation(cx, handle), None);
}

#[gpui_kit::test]
async fn a_local_terminal_asks_before_quitting(cx: &mut TestAppContext) {
    let (handle, workspace) = open_start_page(cx);
    open_local_terminal(cx, handle);
    quit(cx);
    assert_eq!(confirmation(cx, handle).as_deref(), Some("退出"));
    in_frame(cx, handle, |window, cx| window.click("cancel", cx));
    wait_for_dialog_to_close(cx, handle).await;
    cx.update(|cx| {
        assert!(
            workspace
                .read(cx)
                .local_terminal(LocalTerminalId(1))
                .is_some(),
            "still open"
        );
    });

    // Confirmed, it goes: the test platform's quit does nothing more.
    quit(cx);
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    wait_for_dialog_to_close(cx, handle).await;
    assert_eq!(confirmation(cx, handle), None);
}

#[gpui_kit::test]
async fn a_connected_remote_terminal_asks_before_quitting(cx: &mut TestAppContext) {
    let (store, host) = one_host_store(AuthKind::NoPassword);
    let (handle, workspace) = open_workspace_with_store(cx, store);
    quit(cx);
    assert_eq!(confirmation(cx, handle), None, "nothing open yet");
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(host)), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        workspace.read(cx).has_connection(host, cx)
    })
    .await;
    quit(cx);
    assert_eq!(confirmation(cx, handle).as_deref(), Some("退出"));
}

/// Any tab counts, not only one with something running: the rule is the
/// tabs, so that it reads plainly.
#[gpui_kit::test]
async fn any_tab_asks_before_quitting(cx: &mut TestAppContext) {
    let (handle, _) = open_start_page(cx);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(OpenSettings), cx)
    });
    quit(cx);
    assert_eq!(confirmation(cx, handle).as_deref(), Some("退出"));
}

#[gpui_kit::test]
async fn quitting_asks_nothing_once_turned_off(cx: &mut TestAppContext) {
    let (handle, workspace) = open_start_page(cx);
    set_confirm_quit(&workspace, false, cx);
    open_local_terminal(cx, handle);
    quit(cx);
    assert_eq!(confirmation(cx, handle), None);
}
