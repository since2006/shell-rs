//! The built-in editor: opening a file from an SFTP tab, saving it back in
//! place, and what is asked before changes would be lost.

use crate::support::*;
use shellrs::app::{
    CloseActiveTab, CloseEditor, CloseExplorer, ExplorerAction, ExplorerCommand, Quit,
};
use shellrs::editor::EditorId;
use shellrs::explorer::{NewEntryKind, PaneOperation};

const REMOTE_FILE: &str = "/home/tester/文件 甲.txt";
const OTHER_REMOTE_FILE: &str = "/home/tester/文件 乙.txt";
const LOCAL_FILE: &str = "/local/tester/文件 甲.txt";
const FIRST: EditorId = EditorId(1);

/// An SFTP tab on a server with these files, the remote side listed.
async fn open_server(
    cx: &mut TestAppContext,
    files: &[(&str, &[u8])],
) -> (WindowHandle<Root>, Entity<Workspace>, Arc<FakeSftpProvider>) {
    let provider = Arc::new(FakeSftpProvider::with_files(files));
    let (handle, workspace) = open_workspace_with_sftp(cx, provider.clone());
    open_test_explorer(cx, handle).await;
    (handle, workspace, provider)
}

fn open_file(cx: &mut TestAppContext, handle: WindowHandle<Root>, pane: &'static str, name: &str) {
    let name = ElementId::Name(format!("name:{name}").into());
    in_frame(cx, handle, |window, cx| {
        window.within((pane, SFTP_TAB)).double_click(name, cx)
    });
}

async fn wait_for_editor(cx: &mut TestAppContext, handle: WindowHandle<Root>, id: EditorId) {
    cx.wait_for(handle.into(), Duration::from_secs(2), move |window, cx| {
        window.render_frame(cx);
        window.try_find(("editor", id.0)).is_some()
    })
    .await;
}

async fn wait_for(cx: &mut TestAppContext, handle: WindowHandle<Root>, id: &'static str) {
    cx.wait_for(handle.into(), Duration::from_secs(2), move |window, cx| {
        window.render_frame(cx);
        window.try_find(id).is_some()
    })
    .await;
}

fn text_of(workspace: &Entity<Workspace>, id: EditorId, cx: &mut TestAppContext) -> String {
    cx.update(|cx| {
        workspace
            .read(cx)
            .editor(id)
            .expect("editor open")
            .read(cx)
            .state()
            .read(cx)
            .value()
            .to_string()
    })
}

fn is_open(workspace: &Entity<Workspace>, id: EditorId, cx: &mut TestAppContext) -> bool {
    cx.update(|cx| workspace.read(cx).editor(id).is_some())
}

/// Type at the start of the editor, which holds focus once it opens.
fn type_text(cx: &mut TestAppContext, handle: WindowHandle<Root>, text: &str) {
    in_frame(cx, handle, |window, cx| window.input(text, cx));
}

/// Until the fake server has seen `count` writes, and the tab its answer:
/// the SFTP tab polls its worker every 16 ms.
async fn wait_for_writes(cx: &mut TestAppContext, provider: &FakeSftpProvider, count: usize) {
    let writes = provider.writes.clone();
    for _ in 0..200 {
        if writes.lock().unwrap().len() >= count {
            for _ in 0..5 {
                cx.executor().advance_clock(Duration::from_millis(20));
                cx.run_until_parked();
            }
            return;
        }
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(20));
    }
    panic!("expected {count} writes");
}

#[gpui_kit::test]
async fn double_clicking_a_remote_file_opens_it_once(cx: &mut TestAppContext) {
    let (handle, workspace, provider) = open_server(cx, &[(REMOTE_FILE, b"hello world\n")]).await;
    open_file(cx, handle, "remote-pane", "文件 甲.txt");
    wait_for_editor(cx, handle, FIRST).await;

    assert_eq!(text_of(&workspace, FIRST, cx), "hello world\n");
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("editor-tab", FIRST.0)).label(),
            Some("文件 甲.txt")
        );
        assert_eq!(
            window.find(("editor-path", FIRST.0)).label(),
            Some("db-01:/home/tester/文件 甲.txt")
        );
        assert_eq!(
            window.find("status-editor-cursor").label(),
            Some("行 1，列 1")
        );
        assert_eq!(
            window.find("status-editor-format").label(),
            Some("UTF-8 · LF")
        );
        assert_eq!(
            window.find("status-connection").label(),
            Some("已连接 db-01")
        );
        // Nothing changed yet: no mark, nothing to save.
        assert!(
            window
                .within(("editor-tab", FIRST.0))
                .try_find("modified")
                .is_none()
        );
        assert!(window.try_find(("editor-state", FIRST.0)).is_none());
    });

    // Opening it again shows the same tab: no second read, no second tab.
    in_frame(cx, handle, |window, cx| {
        window.click(("explorer-tab", SFTP_TAB), cx)
    });
    open_file(cx, handle, "remote-pane", "文件 甲.txt");
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find(("editor", FIRST.0)).is_some());
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("explorer-tab", SFTP_TAB), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window
            .within(("remote-pane", SFTP_TAB))
            .click("name:文件 甲.txt", cx);
        window.press("f4", cx);
    });
    cx.run_until_parked();
    assert_eq!(provider.reads.lock().unwrap().len(), 1);
    assert!(!is_open(&workspace, EditorId(2), cx));

    // A directory still opens as a directory.
    in_frame(cx, handle, |window, cx| {
        window.click(("explorer-tab", SFTP_TAB), cx)
    });
    open_file(cx, handle, "remote-pane", "目录");
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("remote-path").value() == Some("/home/tester/目录")
    })
    .await;
}

#[gpui_kit::test]
async fn f4_and_enter_open_a_file_too(cx: &mut TestAppContext) {
    let (handle, workspace, _) = open_server(
        cx,
        &[(REMOTE_FILE, b"one\n"), (OTHER_REMOTE_FILE, b"two\n")],
    )
    .await;
    press_on_row(cx, handle, "remote-pane", "文件 甲.txt", "f4");
    wait_for_editor(cx, handle, FIRST).await;
    assert_eq!(text_of(&workspace, FIRST, cx), "one\n");

    // Back to the list, and Enter on the other file.
    in_frame(cx, handle, |window, cx| {
        window.click(("explorer-tab", SFTP_TAB), cx)
    });
    press_on_row(cx, handle, "remote-pane", "文件 乙.txt", "enter");
    wait_for_editor(cx, handle, EditorId(2)).await;
    assert_eq!(text_of(&workspace, EditorId(2), cx), "two\n");
}

#[gpui_kit::test]
async fn saving_writes_the_file_back_and_clears_the_mark(cx: &mut TestAppContext) {
    let (handle, workspace, provider) = open_server(cx, &[(REMOTE_FILE, b"hello world\n")]).await;
    open_file(cx, handle, "remote-pane", "文件 甲.txt");
    wait_for_editor(cx, handle, FIRST).await;

    type_text(cx, handle, "# ");
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .within(("editor-tab", FIRST.0))
                .try_find("modified")
                .is_some()
        );
        assert_eq!(
            window.find(("editor-state", FIRST.0)).label(),
            Some("已修改")
        );
        assert_eq!(
            window.find("status-editor-cursor").label(),
            Some("行 1，列 3")
        );
    });

    in_frame(cx, handle, |window, cx| window.press("cmd-s", cx));
    wait_for_writes(cx, &provider, 1).await;
    cx.run_until_parked();

    let (path, bytes, expected) = provider.writes.lock().unwrap()[0].clone();
    assert_eq!(path, REMOTE_FILE);
    assert_eq!(bytes, b"# hello world\n");
    // Only over the file as it was read.
    assert_eq!(expected, Some(fake_stamp(b"hello world\n", 100)));
    assert_eq!(provider.file(REMOTE_FILE), b"# hello world\n");
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .within(("editor-tab", FIRST.0))
                .try_find("modified")
                .is_none()
        );
        assert!(window.try_find(("editor-state", FIRST.0)).is_none());
    });

    // Undoing back to the saved text is no change either; the next save
    // expects the file as saved.
    type_text(cx, handle, "x");
    in_frame(cx, handle, |window, cx| window.press("cmd-s", cx));
    wait_for_writes(cx, &provider, 2).await;
    let expected = provider.writes.lock().unwrap()[1].2;
    assert_eq!(expected, Some(fake_stamp(b"# hello world\n", 101)));
    assert_eq!(text_of(&workspace, FIRST, cx), "# xhello world\n");
}

#[gpui_kit::test]
async fn crlf_and_a_byte_order_mark_are_written_back_as_they_were(cx: &mut TestAppContext) {
    let (handle, workspace, provider) =
        open_server(cx, &[(REMOTE_FILE, b"\xEF\xBB\xBFa=1\r\nb=2\r\n")]).await;
    open_file(cx, handle, "remote-pane", "文件 甲.txt");
    wait_for_editor(cx, handle, FIRST).await;
    assert_eq!(text_of(&workspace, FIRST, cx), "a=1\nb=2\n");
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("status-editor-format").label(),
            Some("UTF-8 BOM · CRLF")
        );
    });

    type_text(cx, handle, "c=3\n");
    in_frame(cx, handle, |window, cx| window.press("cmd-s", cx));
    wait_for_writes(cx, &provider, 1).await;
    assert_eq!(
        provider.writes.lock().unwrap()[0].1,
        b"\xEF\xBB\xBFc=3\r\na=1\r\nb=2\r\n"
    );
}

#[gpui_kit::test]
async fn a_file_changed_on_the_server_is_overwritten_only_when_asked(cx: &mut TestAppContext) {
    let (handle, workspace, provider) = open_server(cx, &[(REMOTE_FILE, b"mine\n")]).await;
    open_file(cx, handle, "remote-pane", "文件 甲.txt");
    wait_for_editor(cx, handle, FIRST).await;
    type_text(cx, handle, "1");
    provider.change_file(REMOTE_FILE, b"theirs\n");

    in_frame(cx, handle, |window, cx| window.press("cmd-s", cx));
    wait_for(cx, handle, "ok").await;
    // Not written: the file is theirs, and the editor keeps its changes.
    assert_eq!(provider.file(REMOTE_FILE), b"theirs\n");
    in_frame(cx, handle, |window, cx| window.click("cancel", cx));
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .within(("editor-tab", FIRST.0))
                .try_find("modified")
                .is_some()
        );
    });

    in_frame(cx, handle, |window, cx| window.press("cmd-s", cx));
    wait_for(cx, handle, "ok").await;
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    wait_for_writes(cx, &provider, 3).await;
    cx.run_until_parked();
    let writes = provider.writes.lock().unwrap().clone();
    assert_eq!(writes[2].2, None, "覆盖 writes without the check");
    assert_eq!(provider.file(REMOTE_FILE), b"1mine\n");
    assert_eq!(text_of(&workspace, FIRST, cx), "1mine\n");
}

#[gpui_kit::test]
async fn closing_an_edited_file_asks_whether_to_save_it(cx: &mut TestAppContext) {
    let (handle, workspace, provider) = open_server(cx, &[(REMOTE_FILE, b"text\n")]).await;
    open_file(cx, handle, "remote-pane", "文件 甲.txt");
    wait_for_editor(cx, handle, FIRST).await;
    type_text(cx, handle, "1");

    // 取消 keeps the tab and the changes.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(CloseEditor(FIRST)), cx)
    });
    wait_for(cx, handle, "editor-close-save").await;
    in_frame(cx, handle, |window, cx| {
        window.click("editor-close-cancel", cx)
    });
    assert!(is_open(&workspace, FIRST, cx));

    // ⌘W asks the same; 放弃修改 closes without writing.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(CloseActiveTab), cx)
    });
    wait_for(cx, handle, "editor-close-discard").await;
    in_frame(cx, handle, |window, cx| {
        window.click("editor-close-discard", cx)
    });
    assert!(!is_open(&workspace, FIRST, cx));
    assert!(provider.writes.lock().unwrap().is_empty());

    // 保存 writes, then closes. (Closing the editor showed the SFTP tab.)
    open_file(cx, handle, "remote-pane", "文件 甲.txt");
    wait_for_editor(cx, handle, EditorId(2)).await;
    type_text(cx, handle, "2");
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(CloseEditor(EditorId(2))), cx)
    });
    wait_for(cx, handle, "editor-close-save").await;
    in_frame(cx, handle, |window, cx| {
        window.click("editor-close-save", cx)
    });
    wait_for_writes(cx, &provider, 1).await;
    cx.run_until_parked();
    assert_eq!(provider.file(REMOTE_FILE), b"2text\n");
    assert!(!is_open(&workspace, EditorId(2), cx));

    // Without changes nothing is asked.
    open_file(cx, handle, "remote-pane", "文件 甲.txt");
    wait_for_editor(cx, handle, EditorId(3)).await;
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(CloseEditor(EditorId(3))), cx)
    });
    assert!(!is_open(&workspace, EditorId(3), cx));
}

#[gpui_kit::test]
async fn a_binary_or_too_large_file_is_explained_not_opened(cx: &mut TestAppContext) {
    let large = vec![b'a'; EDIT_LIMIT as usize + 1];
    let (handle, workspace, _) = open_server(
        cx,
        &[
            (REMOTE_FILE, b"PK\x03\x04\x00\x00"),
            (OTHER_REMOTE_FILE, &large),
        ],
    )
    .await;
    open_file(cx, handle, "remote-pane", "文件 甲.txt");
    wait_for(cx, handle, "ok").await;
    assert!(!is_open(&workspace, FIRST, cx));

    // 下载… is the way out for a remote file.
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    wait_for(cx, handle, "download-confirm").await;
    in_frame(cx, handle, |window, cx| window.press("escape", cx));

    open_file(cx, handle, "remote-pane", "文件 乙.txt");
    wait_for(cx, handle, "ok").await;
    assert!(!is_open(&workspace, FIRST, cx));
}

#[gpui_kit::test]
async fn a_local_file_is_edited_on_this_machine(cx: &mut TestAppContext) {
    let local = FakeLocalDirectory::default();
    local
        .files
        .lock()
        .unwrap()
        .insert(LOCAL_FILE.into(), b"local\n".to_vec());
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, workspace) = open_workspace_with_services(cx, provider.clone(), local.clone());
    open_test_explorer(cx, handle).await;
    open_file(cx, handle, "local-pane", "文件 甲.txt");
    wait_for_editor(cx, handle, FIRST).await;
    assert_eq!(text_of(&workspace, FIRST, cx), "local\n");
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("editor-path", FIRST.0)).label(),
            Some(LOCAL_FILE)
        );
        assert_eq!(window.find("status-connection").label(), Some("本机文件"));
    });

    type_text(cx, handle, "edited ");
    in_frame(cx, handle, |window, cx| window.press("cmd-s", cx));
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.try_find(("editor-state", FIRST.0)).is_none()
    })
    .await;
    assert_eq!(
        local.files.lock().unwrap()[std::path::Path::new(LOCAL_FILE)],
        b"edited local\n"
    );
    assert!(
        local
            .calls
            .lock()
            .unwrap()
            .contains(&format!("write {LOCAL_FILE}"))
    );
    // Nothing went to the server.
    assert!(provider.writes.lock().unwrap().is_empty());
}

#[gpui_kit::test]
async fn closing_the_sftp_tab_asks_once_and_takes_its_files_along(cx: &mut TestAppContext) {
    let (handle, workspace, provider) = open_server(cx, &[(REMOTE_FILE, b"text\n")]).await;
    open_file(cx, handle, "remote-pane", "文件 甲.txt");
    wait_for_editor(cx, handle, FIRST).await;
    type_text(cx, handle, "1");

    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(CloseExplorer(ExplorerId(SFTP_TAB))), cx)
    });
    wait_for(cx, handle, "ok").await;
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    cx.run_until_parked();
    cx.update(|cx| assert!(workspace.read(cx).explorer(ExplorerId(SFTP_TAB)).is_none()));
    assert!(!is_open(&workspace, FIRST, cx));
    assert!(provider.writes.lock().unwrap().is_empty());
}

#[gpui_kit::test]
async fn saving_while_disconnected_offers_to_reconnect(cx: &mut TestAppContext) {
    let (handle, workspace, provider) = open_server(cx, &[(REMOTE_FILE, b"text\n")]).await;
    open_file(cx, handle, "remote-pane", "文件 甲.txt");
    wait_for_editor(cx, handle, FIRST).await;
    type_text(cx, handle, "1");
    let events = provider.events.lock().unwrap()[0].clone();
    events
        .send_blocking(SftpEvent::Disconnected("连接中断".into()))
        .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window
            .find("status-connection")
            .label()
            .is_some_and(|text| text.starts_with("未连接"))
    })
    .await;

    in_frame(cx, handle, |window, cx| window.press("cmd-s", cx));
    wait_for(cx, handle, "ok").await;
    assert!(provider.writes.lock().unwrap().is_empty());
    assert!(cx.update(|cx| {
        workspace
            .read(cx)
            .editor(FIRST)
            .unwrap()
            .read(cx)
            .is_dirty()
    }));
}

#[gpui_kit::test]
async fn a_new_file_opens_in_the_editor(cx: &mut TestAppContext) {
    let (handle, workspace, _) = open_server(cx, &[]).await;
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(
            Box::new(ExplorerAction::new(
                ExplorerId(SFTP_TAB),
                ExplorerCommand::Operate {
                    remote: true,
                    operation: PaneOperation::Create {
                        kind: NewEntryKind::File,
                        name: "新.conf".into(),
                    },
                },
            )),
            cx,
        )
    });
    wait_for_editor(cx, handle, FIRST).await;
    assert_eq!(text_of(&workspace, FIRST, cx), "");
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("editor-tab", FIRST.0)).label(),
            Some("新.conf")
        );
    });
}

#[gpui_kit::test]
async fn quitting_with_unsaved_changes_asks_first(cx: &mut TestAppContext) {
    let (handle, workspace, _) = open_server(cx, &[(REMOTE_FILE, b"text\n")]).await;
    open_file(cx, handle, "remote-pane", "文件 甲.txt");
    wait_for_editor(cx, handle, FIRST).await;

    // Nothing unsaved: nothing asked.
    cx.update(|cx| cx.dispatch_action(&Quit));
    cx.run_until_parked();
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("ok").is_none())
    });

    type_text(cx, handle, "1");
    // From the app, with no window in front, as ⌘Q may arrive after a
    // dialog left nothing focused.
    cx.update(|cx| cx.dispatch_action(&Quit));
    wait_for(cx, handle, "ok").await;
    in_frame(cx, handle, |window, cx| window.click("cancel", cx));
    assert!(is_open(&workspace, FIRST, cx));
}

#[gpui_kit::test]
async fn closing_all_tabs_asks_once_for_every_unsaved_file(cx: &mut TestAppContext) {
    let (handle, workspace, provider) = open_server(
        cx,
        &[(REMOTE_FILE, b"one\n"), (OTHER_REMOTE_FILE, b"two\n")],
    )
    .await;
    open_file(cx, handle, "remote-pane", "文件 甲.txt");
    wait_for_editor(cx, handle, FIRST).await;
    type_text(cx, handle, "1");
    in_frame(cx, handle, |window, cx| {
        window.click(("explorer-tab", SFTP_TAB), cx)
    });
    open_file(cx, handle, "remote-pane", "文件 乙.txt");
    wait_for_editor(cx, handle, EditorId(2)).await;
    type_text(cx, handle, "2");

    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(
            Box::new(CloseTabs {
                tab: CenterTab::Editor(EditorId(2)),
                scope: CloseScope::All,
            }),
            cx,
        )
    });
    wait_for(cx, handle, "ok").await;
    // One question, not one per file.
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("editor-close-save").is_none())
    });
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    cx.run_until_parked();
    assert!(!is_open(&workspace, FIRST, cx));
    assert!(!is_open(&workspace, EditorId(2), cx));
    assert!(provider.writes.lock().unwrap().is_empty());
}
