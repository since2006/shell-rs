//! The credential tests' workspace and steps.

use super::*;

/// A workspace over `store`, its remote terminals recorded by `remote` and
/// its connection test answered by `tester`. Dialogs do not slide, so their
/// fields stay where they were found.
pub fn open_workspace_with_credentials(
    cx: &mut TestAppContext,
    store: HostStore,
    remote: Arc<RecordingRemoteProvider>,
    tester: Arc<FakeConnectionTester>,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    cx.update(shellrs::init);
    cx.update(|cx| cx.set_reduce_motion(true));
    let mut workspace = None;
    let handle = cx.open_window(size(px(1280.), px(800.)), |window, cx| {
        let store = cx.new(|_| store);
        let view = cx.new(|cx| {
            Workspace::new_with_services(
                store,
                cx.new(|_| SettingsStore::in_memory()),
                remote,
                Arc::new(FakeTerminalFactory::default()),
                Arc::new(FakeSftpProvider::default()),
                Arc::new(FakeLocalDirectory::default()),
                tester,
                Arc::new(FakeForwardProvider::default()),
                window,
                cx,
            )
        });
        workspace = Some(view.clone());
        Root::new(view, window, cx)
    });
    cx.run_until_parked();
    (handle, workspace.expect("workspace created"))
}

/// Show the credential list and wait for it to be up.
pub async fn show_credentials(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    in_frame(cx, handle, |window, cx| {
        window.click("show-credentials", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("credential-search").is_some()
    })
    .await;
}

/// Wait for the open dialog to close.
pub async fn wait_for_dialog_to_close(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;
    cx.run_until_parked();
}
