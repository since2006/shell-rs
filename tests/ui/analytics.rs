//! 匿名使用统计: what the workspace tells the analytics thread. What the
//! thread makes of it, and sends, is tested in `shellrs::analytics`.

use shellrs::analytics::{Analytics, Counter, Signal};

use crate::support::*;

/// Have the workspace report to a channel the test reads.
fn report(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    workspace: &Entity<Workspace>,
) -> mpsc::Receiver<Signal> {
    let (sender, receiver) = mpsc::channel();
    cx.update_window(handle.into(), |_, window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.set_analytics(Analytics::new(sender, true), window, cx)
        });
    })
    .unwrap();
    cx.run_until_parked();
    receiver
}

fn counted(signals: &[Signal]) -> Vec<Counter> {
    signals
        .iter()
        .filter_map(|signal| match signal {
            Signal::Count(counter) => Some(*counter),
            _ => None,
        })
        .collect()
}

fn came_forward(signals: &[Signal]) -> bool {
    signals
        .iter()
        .any(|signal| matches!(signal, Signal::Active(_)))
}

fn bring_forward(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    gpui_kit::VisualTestContext::from_window(handle.into(), cx).deactivate_window();
    in_frame(cx, handle, |window, _| window.activate_window());
}

#[gpui_kit::test]
fn the_window_coming_forward_is_reported_with_the_setup(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let signals = report(cx, handle, &workspace);
    bring_forward(cx, handle);
    let received: Vec<Signal> = signals.try_iter().collect();
    let snapshot = received.iter().find_map(|signal| match signal {
        Signal::Active(snapshot) => Some(snapshot.clone()),
        _ => None,
    });
    let snapshot = snapshot.expect("the window coming forward is reported");
    // How much is saved, never what.
    assert_eq!(snapshot.hosts, 6);
    assert_eq!(snapshot.appearance, "system");
    assert!(!snapshot.highlight);
}

#[gpui_kit::test]
async fn the_features_used_are_counted(cx: &mut TestAppContext) {
    let (store, host) = one_host_store(AuthKind::NoPassword);
    let (handle, workspace) = open_workspace_with_store(cx, store);
    let signals = report(cx, handle, &workspace);

    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(NewLocalTerminal), cx)
    });
    assert_eq!(
        counted(&signals.try_iter().collect::<Vec<_>>()),
        [Counter::LocalTerminal]
    );

    // An SSH terminal is counted once connected, and by how.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(host)), cx)
    });
    let mut received = Vec::new();
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        received.extend(signals.try_iter());
        counted(&received).contains(&Counter::Ssh)
    })
    .await;
    assert_eq!(counted(&received), [Counter::Ssh, Counter::SshNoPassword]);

    // With it in front: a tool of the right sidebar, and the find bar.
    in_frame(cx, handle, |window, cx| window.click("tool-history", cx));
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(FindInTerminal), cx)
    });
    assert_eq!(
        counted(&signals.try_iter().collect::<Vec<_>>()),
        [Counter::ToolHistory, Counter::Find]
    );

    // An SFTP tab, once connected.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(OpenExplorer(host)), cx)
    });
    let mut received = Vec::new();
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        received.extend(signals.try_iter());
        counted(&received).contains(&Counter::Sftp)
    })
    .await;
    assert_eq!(counted(&received), [Counter::Sftp]);
}

#[gpui_kit::test]
fn switches_turned_in_the_settings_are_counted(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let signals = report(cx, handle, &workspace);
    let set_highlight = |cx: &mut TestAppContext, on: bool| {
        workspace.update(cx, |workspace, cx| {
            workspace.settings().update(cx, |settings, cx| {
                settings.update(|settings| settings.terminal_highlight.enabled = on, cx)
            });
        });
        cx.run_until_parked();
    };
    set_highlight(cx, true);
    set_highlight(cx, false);
    assert_eq!(
        counted(&signals.try_iter().collect::<Vec<_>>()),
        [Counter::HighlightOn, Counter::HighlightOff]
    );
}

#[gpui_kit::test]
fn turned_off_in_the_settings_nothing_is_reported(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let signals = report(cx, handle, &workspace);
    let enabled = |cx: &mut TestAppContext| {
        workspace.read_with(cx, |workspace, cx| {
            workspace.settings().read(cx).settings().analytics.enabled
        })
    };
    assert!(enabled(cx));

    // 设置 › 关于 › 隐私 › 发送匿名使用统计.
    in_frame(cx, handle, |window, cx| window.click("open-settings", cx));
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-6", cx)
    });
    in_frame(cx, handle, |window, cx| {
        let switch = window
            .within("settings")
            .within("group-1")
            .within("item-0")
            .find("check");
        assert_eq!(switch.checked(), Some(true));
        window
            .within("settings")
            .within("group-1")
            .within("item-0")
            .click("check", cx);
    });
    assert!(!enabled(cx));
    let received: Vec<Signal> = signals.try_iter().collect();
    assert!(received.contains(&Signal::Enabled(false)), "{received:?}");

    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(NewLocalTerminal), cx)
    });
    bring_forward(cx, handle);
    let received: Vec<Signal> = signals.try_iter().collect();
    assert!(counted(&received).is_empty(), "{received:?}");
    assert!(!came_forward(&received), "{received:?}");
}
