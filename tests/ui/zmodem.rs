//! ZMODEM in a terminal: `rz` on the other end asks for the files to
//! upload, and files dropped on a remote terminal go to the `rz` typed
//! for them.

use crate::support::*;
use std::path::PathBuf;

use shellrs::analytics::{Analytics, Counter, Signal};

const TERMINAL: RemoteTerminalId = RemoteTerminalId(INITIAL_WEB_TERMINAL);

/// The seeded workspace, its web-01 terminal running `rz` when it is typed.
fn open(
    cx: &mut TestAppContext,
) -> (
    WindowHandle<Root>,
    Entity<Workspace>,
    Arc<FakeTerminalFactory>,
    mpsc::Receiver<Signal>,
) {
    let factory = Arc::new(FakeTerminalFactory::rz());
    let (handle, workspace) =
        open_workspace_with_remote_factory(cx, HostStore::seed(), factory.clone());
    let (sender, signals) = mpsc::channel();
    cx.update_window(handle.into(), |_, window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.set_analytics(Analytics::new(sender, true), window, cx)
        });
    })
    .unwrap();
    (handle, workspace, factory, signals)
}

fn screen(workspace: &Entity<Workspace>, cx: &App) -> String {
    workspace
        .read(cx)
        .remote_terminal(TERMINAL)
        .expect("web-01's terminal")
        .read(cx)
        .terminal()
        .read(cx)
        .screen_text(cx)
}

/// ZMODEM counted, the seeded terminals' connections aside.
fn counted(signals: &mpsc::Receiver<Signal>) -> Vec<Counter> {
    signals
        .try_iter()
        .filter_map(|signal| match signal {
            Signal::Count(counter @ (Counter::ZmodemUpload | Counter::ZmodemDownload)) => {
                Some(counter)
            }
            _ => None,
        })
        .collect()
}

async fn running(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    workspace: &Entity<Workspace>,
) {
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        remote_lifecycle(workspace, TERMINAL, cx) == TerminalLifecycle::Running
    })
    .await;
}

/// Type `rz` and Enter in web-01's terminal, and wait for `rz` to start.
async fn start_rz(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    workspace: &Entity<Workspace>,
) {
    running(cx, handle, workspace).await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("terminal", TERMINAL.0), cx);
        for key in ["r", "z", "enter"] {
            window.press(key, cx);
        }
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        screen(workspace, cx).contains("rz waiting to receive")
    })
    .await;
    // The screen is the emulator's own; the question comes through the
    // terminal's events, a moment later.
    for _ in 0..200 {
        if cx.did_prompt_for_paths() {
            return;
        }
        cx.executor().timer(Duration::from_millis(10)).await;
        cx.run_until_parked();
    }
    panic!("rz asked for no files");
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[gpui_kit::test]
async fn rz_asks_for_the_files_to_upload_and_takes_only_ctrl_c(cx: &mut TestAppContext) {
    let folder = tempfile::tempdir().unwrap();
    let file = folder.path().join("notes.txt");
    std::fs::write(&file, b"hello").unwrap();
    let (handle, workspace, factory, signals) = open(cx);
    start_rz(cx, handle, &workspace).await;
    cx.simulate_path_prompt_response(|options| {
        assert!(options.files && options.multiple && !options.directories);
        Some(vec![file.clone()])
    });
    // The file offered (ZFILE), and the line saying so.
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        contains(&factory.data(), b"notes.txt")
            && screen(&workspace, cx).contains("正在用 ZMODEM 上传，按 Ctrl+C 取消")
    })
    .await;
    assert_eq!(counted(&signals), [Counter::ZmodemUpload]);

    // Keys would land in the transfer: only Ctrl+C means anything.
    let typed = factory.written_text();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.press("l", cx);
        window.press("ctrl-c", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        screen(&workspace, cx).contains("已取消")
    })
    .await;
    assert_eq!(factory.written_text(), typed);
    assert!(contains(&factory.data(), &[0x18; 10]), "the cancel");
}

#[gpui_kit::test]
async fn choosing_nothing_cancels_rz(cx: &mut TestAppContext) {
    let (handle, workspace, factory, signals) = open(cx);
    start_rz(cx, handle, &workspace).await;
    cx.simulate_path_prompt_response(|_| None);
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        screen(&workspace, cx).contains("已取消")
    })
    .await;
    assert!(factory.data().starts_with(&[0x18; 10]));
    assert!(counted(&signals).is_empty());
}

/// Drop `paths` on web-01's terminal.
fn drop_on_terminal(cx: &mut TestAppContext, handle: WindowHandle<Root>, paths: Vec<PathBuf>) {
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let position = window.find(("terminal", TERMINAL.0)).bounds().center();
        let paths = gpui_kit::ExternalPaths(paths.into());
        window.dispatch_event(
            gpui_kit::FileDropEvent::Entered { position, paths }.to_platform_input(),
            cx,
        );
        window.dispatch_event(
            gpui_kit::FileDropEvent::Submit { position }.to_platform_input(),
            cx,
        );
    })
    .unwrap();
}

#[gpui_kit::test]
async fn files_dropped_on_a_remote_terminal_go_up_without_asking(cx: &mut TestAppContext) {
    let folder = tempfile::tempdir().unwrap();
    let file = folder.path().join("dropped.txt");
    std::fs::write(&file, b"dropped").unwrap();
    let (handle, workspace, factory, signals) = open(cx);
    running(cx, handle, &workspace).await;

    // A folder alone is refused: ZMODEM sends files.
    drop_on_terminal(cx, handle, vec![folder.path().to_path_buf()]);
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.notifications(cx).len() == 1
    })
    .await;
    assert_eq!(factory.written_text(), "");

    drop_on_terminal(cx, handle, vec![file, folder.path().to_path_buf()]);
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        contains(&factory.data(), b"dropped.txt")
    })
    .await;
    assert_eq!(factory.written_text(), "rz\r");
    assert!(!cx.did_prompt_for_paths());
    assert_eq!(counted(&signals), [Counter::ZmodemUpload]);
}

#[gpui_kit::test]
async fn compose_refuses_zmodem_without_clearing_the_draft_and_ctrl_c_cancels(
    cx: &mut TestAppContext,
) {
    use shellrs::app::{SendCompose, SendComposeInterrupt, ToggleComposePane};
    let (handle, workspace, factory, _) = open(cx);
    start_rz(cx, handle, &workspace).await;
    in_frame(cx, handle, |w, cx| {
        w.dispatch_action(Box::new(ToggleComposePane), cx)
    });
    in_frame(cx, handle, |w, cx| {
        w.click("compose-pane", cx);
        w.input("after-transfer", cx);
        w.dispatch_action(Box::new(SendCompose), cx);
    });
    in_frame(cx, handle, |w, _| {
        assert_eq!(w.find("compose-result").label(), Some("已提交 0，未发送 1"))
    });
    assert_eq!(factory.written_text(), "rz\r");
    in_frame(cx, handle, |w, cx| {
        w.dispatch_action(Box::new(SendComposeInterrupt), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        !factory.data().is_empty()
    })
    .await;
    // The queued file dialog is dismissed with cancellation; the control character
    // already ended the transfer, and the original draft remains available.
    cx.simulate_path_prompt_response(|_| None);
    in_frame(cx, handle, |w, cx| {
        w.click("compose-pane", cx);
        w.dispatch_action(Box::new(SendCompose), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        factory.written_text().ends_with("after-transfer\r")
    })
    .await;
}
