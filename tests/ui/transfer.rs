//! SFTP transfers: uploading, downloading, dragging between the panes and
//! the queue.

use crate::support::*;

/// Wait until the batch at the head of the transfer queue reads `status`.
async fn wait_for_head_status(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    status: &'static str,
) {
    cx.wait_for(handle.into(), Duration::from_secs(2), move |window, cx| {
        window.render_frame(cx);
        window
            .try_find("transfer-status")
            .is_some_and(|head| head.label() == Some(status))
    })
    .await;
}

#[gpui_kit::test]
async fn sftp_multi_selection_keyboard_upload_freezes_paths_and_cancel_resumes(
    cx: &mut TestAppContext,
) {
    use shellrs::app::{ExplorerAction, ExplorerCommand};
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, workspace) = open_workspace_with_sftp(cx, provider.clone());
    open_test_explorer(cx, handle).await;
    // Names sort by code point: 文件 乙 comes before 文件 甲.
    cx.update_window(handle.into(), |_, window, cx| {
        window
            .within(("local-pane", SFTP_TAB))
            .click("name:文件 乙.txt", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(pane_selection(&workspace, false, cx), ["文件 乙.txt"]));
    // Shift+↓ extends to the next row; Space takes the cursor row back out.
    for (key, expected) in [
        ("shift-down", &["文件 乙.txt", "文件 甲.txt"][..]),
        ("space", &["文件 乙.txt"][..]),
    ] {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.press(key, cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update(|cx| assert_eq!(pane_selection(&workspace, false, cx), expected));
    }
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.press("cmd-a", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            workspace
                .read(cx)
                .explorer(ExplorerId(SFTP_TAB))
                .unwrap()
                .read(cx)
                .local()
                .read(cx)
                .upload_sources(cx)
                .len(),
            4
        );
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.press("f5", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("upload-confirm").visible());
        // Counted by kind, and each item on a line of its own by name, so a
        // long path cannot pass for two items.
        assert!(
            window
                .find("transfer-summary")
                .label()
                .unwrap()
                .starts_with("上传 4 个项目（2 个文件夹、2 个文件）到 ")
        );
        let source = |path: &str| {
            window
                .find(ElementId::Name(format!("source:{path}").into()))
                .label()
                .map(str::to_string)
        };
        assert_eq!(
            source("/local/tester/文件 甲.txt").as_deref(),
            Some("文件 甲.txt")
        );
        assert_eq!(
            source("/local/tester/链接目录").as_deref(),
            Some("链接目录")
        );
        window.click("upload-target", cx);
        window.press("cmd-a", cx);
        window.input("/固定目标", cx);
        window.click("upload-confirm", cx);
    })
    .unwrap();
    wait_for_head_status(cx, handle, "正在扫描").await;
    assert_eq!(
        provider.requests.lock().unwrap()[0].destination().as_str(),
        "/固定目标"
    );
    assert_eq!(provider.requests.lock().unwrap()[0].sources().len(), 4);
    // A batch unfolds to its item results. None has ended yet, and the list
    // says so rather than opening empty; a second click folds it again.
    for open in [true, false] {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click(("queue-expand", 1u64), cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.try_find("transfer-detail-empty").is_some(), open);
        })
        .unwrap();
    }
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(
            Box::new(ExplorerAction::new(
                ExplorerId(SFTP_TAB),
                ExplorerCommand::Navigate {
                    remote: true,
                    path: "/other".into(),
                },
            )),
            cx,
        );
        window.click("cancel-transfer", cx);
    })
    .unwrap();
    wait_for_head_status(cx, handle, "已停止").await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("resume-transfer", cx);
    })
    .unwrap();
    wait_for_head_status(cx, handle, "0%").await;
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
    assert_eq!(
        provider.requests.lock().unwrap()[0].destination().as_str(),
        "/固定目标"
    );
}

/// A transfer confirmed while another runs waits its turn in the queue,
/// WinSCP-style, and starts when the engine is free again.
#[gpui_kit::test]
async fn sftp_transfers_wait_their_turn_in_the_queue(cx: &mut TestAppContext) {
    use shellrs::app::{ExplorerAction, ExplorerCommand};
    use shellrs::sftp::{TransferPhase, TransferProgress};
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, _) = open_workspace_with_sftp(cx, provider.clone());
    open_test_explorer(cx, handle).await;
    let upload = |cx: &mut TestAppContext, name: &str| {
        let paths = vec![std::path::PathBuf::from(format!("/local/tester/{name}"))];
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.dispatch_action(
                Box::new(ExplorerAction::new(
                    ExplorerId(SFTP_TAB),
                    ExplorerCommand::UploadPaths {
                        paths,
                        target: "/home/tester".into(),
                    },
                )),
                cx,
            );
        })
        .unwrap();
        cx.run_until_parked();
    };
    let confirm = |cx: &mut TestAppContext, queued: bool| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.try_find("transfer-queued-note").is_some(), queued);
            window.click("upload-confirm", cx);
        })
        .unwrap();
    };
    let label = |cx: &mut TestAppContext, id: ElementId| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window
                .try_find(id)
                .and_then(|row| row.label().map(str::to_string))
        })
        .unwrap()
    };
    let send = |event: SftpEvent| {
        provider.events.lock().unwrap()[0]
            .send_blocking(event)
            .unwrap()
    };

    upload(cx, "文件 甲.txt");
    confirm(cx, false);
    wait_for_head_status(cx, handle, "正在扫描").await;
    upload(cx, "文件 乙.txt");
    confirm(cx, true);
    cx.run_until_parked();
    assert_eq!(
        label(cx, ("queue-entry", 2u64).into()).as_deref(),
        Some("等待中")
    );
    assert_eq!(provider.requests.lock().unwrap().len(), 1);

    // The first one ends; the engine goes idle and takes the second.
    send(SftpEvent::Progress(TransferProgress::new(
        TransferPhase::Completed,
    )));
    send(SftpEvent::Idle);
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window
            .try_find(("queue-entry", 1u64))
            .is_some_and(|row| row.label() == Some("已完成"))
    })
    .await;
    assert_eq!(provider.requests.lock().unwrap().len(), 2);
    assert_eq!(
        provider.requests.lock().unwrap()[1].sources(),
        [std::path::PathBuf::from("/local/tester/文件 乙.txt")]
    );

    // Its file in flight gets a row and a bar of its own.
    send(SftpEvent::Progress(
        TransferProgress::new(TransferPhase::Transferring)
            .with_bytes(512, 1024)
            .with_current(
                "/local/tester/文件 乙.txt",
                "/home/tester/文件 乙.txt",
                256,
                1024,
            ),
    ));
    wait_for_head_status(cx, handle, "50%").await;
    assert_eq!(
        label(cx, ("queue-file", 2u64).into()).as_deref(),
        Some("/local/tester/文件 乙.txt")
    );
    assert_eq!(
        label(cx, ("queue-file-status", 2u64).into()).as_deref(),
        Some("25%")
    );

    // Cleared once done, and the queue goes away with its last row.
    let clear = |cx: &mut TestAppContext| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("clear-finished-transfers", cx);
        })
        .unwrap();
        cx.run_until_parked();
    };
    clear(cx);
    assert_eq!(label(cx, ("queue-entry", 1u64).into()), None);
    send(SftpEvent::Progress(TransferProgress::new(
        TransferPhase::Completed,
    )));
    send(SftpEvent::Idle);
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window
            .try_find(("queue-entry", 2u64))
            .is_some_and(|row| row.label() == Some("已完成"))
    })
    .await;
    clear(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window
                .try_find(("transfer-queue", SFTP_TAB))
                .is_none_or(|queue| !queue.visible())
        );
    })
    .unwrap();
}

#[gpui_kit::test]
async fn sftp_native_picker_and_external_drop_share_confirmation(cx: &mut TestAppContext) {
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, _) = open_workspace_with_sftp(cx, provider);
    open_test_explorer(cx, handle).await;
    // 「选择文件上传…」 sits in the upload button's menu, which the tests do
    // not open; dispatch what the item dispatches.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(
            Box::new(shellrs::app::ExplorerAction::new(
                ExplorerId(SFTP_TAB),
                shellrs::app::ExplorerCommand::ChooseFiles,
            )),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|options| {
        assert!(options.files && options.directories && options.multiple);
        Some(vec!["/tmp/甲".into(), "/tmp/目录".into()])
    });
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("upload-confirm").visible());
        assert_eq!(window.find("upload-target").value(), Some("/home/tester"));
        window.press("escape", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within(("remote-pane", SFTP_TAB))
            .hover("file:目录", cx);
        window.render_frame(cx);
        let position = window
            .within(("remote-pane", SFTP_TAB))
            .find("file:目录")
            .bounds()
            .center();
        let paths =
            gpui_kit::ExternalPaths(vec![std::path::PathBuf::from("/tmp/Finder 文件")].into());
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
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("upload-confirm").visible());
        assert_eq!(
            window.find("upload-target").value(),
            Some("/home/tester/目录")
        );
    })
    .unwrap();
}

#[gpui_kit::test]
async fn sftp_internal_drag_conflict_and_close_confirmation(cx: &mut TestAppContext) {
    use shellrs::sftp::{TransferQuestion, TransferQuestionKind};
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, workspace) = open_workspace_with_sftp(cx, provider.clone());
    open_test_explorer(cx, handle).await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let from = window
            .within(("local-pane", SFTP_TAB))
            .find("name:文件 甲.txt")
            .bounds()
            .center();
        let to = window
            .within(("remote-pane", SFTP_TAB))
            .find("file:目录")
            .bounds()
            .center();
        window.drag(from, to, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("upload-target").value(),
            Some("/home/tester/目录")
        );
        window.click("upload-confirm", cx);
    })
    .unwrap();
    wait_for_head_status(cx, handle, "正在扫描").await;
    provider.events.lock().unwrap()[0]
        .send_blocking(SftpEvent::Question(TransferQuestion::new(
            900,
            TransferQuestionKind::Conflict,
            "/home/tester/目录/文件 甲.txt",
            "目标已存在",
        )))
        .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.try_find("transfer-question-confirm").is_some()
    })
    .await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("transfer-apply-all", cx);
        window.click("transfer-question-cancel", cx);
    })
    .unwrap();
    wait_for_head_status(cx, handle, "已停止").await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("resume-transfer", cx);
    })
    .unwrap();
    wait_for_head_status(cx, handle, "0%").await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("close-explorer", SFTP_TAB), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("ok").visible());
        assert!(workspace.read(cx).explorer(ExplorerId(SFTP_TAB)).is_some());
        window.click("ok", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert!(workspace.read(cx).explorer(ExplorerId(SFTP_TAB)).is_none()));
}

#[gpui_kit::test]
async fn sftp_downloads_the_selection_with_f5_into_a_chosen_folder(cx: &mut TestAppContext) {
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, _) = open_workspace_with_sftp(cx, provider.clone());
    open_test_explorer(cx, handle).await;
    // F5 in the remote list downloads the selection into the local directory.
    press_on_row(cx, handle, "remote-pane", "文件 甲.txt", "f5");
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("download-confirm").visible());
        assert_eq!(
            window.find("download-target").value(),
            Some("/local/tester")
        );
        window.click("download-browse", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|options| {
        assert!(options.directories && !options.files && !options.multiple);
        Some(vec!["/picked".into()])
    });
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("download-target").value(), Some("/picked"));
        window.click("download-confirm", cx);
    })
    .unwrap();
    wait_for_head_status(cx, handle, "0%").await;
    {
        let downloads = provider.downloads.lock().unwrap();
        assert_eq!(downloads.len(), 1);
        assert_eq!(
            downloads[0].sources(),
            [RemotePath::new("/home/tester/文件 甲.txt").unwrap()]
        );
        assert_eq!(downloads[0].destination(), std::path::Path::new("/picked"));
    }
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("cancel-transfer").visible());
        // The running download's row, with the arrow pointing down.
        assert_eq!(window.find(("queue-entry", 1u64)).label(), Some("0%"));
    })
    .unwrap();
}

#[gpui_kit::test]
async fn sftp_dragging_remote_rows_to_a_local_directory_asks_to_download(cx: &mut TestAppContext) {
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, workspace) = open_workspace_with_sftp(cx, provider);
    open_test_explorer(cx, handle).await;
    // By its name: the rest of the row draws a selection rectangle.
    let drag_name = |cx: &mut TestAppContext, name: &str| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let from = window
                .within(("remote-pane", SFTP_TAB))
                .find(ElementId::Name(format!("name:{name}").into()))
                .bounds()
                .center();
            let to = window
                .within(("local-pane", SFTP_TAB))
                .find("file:目录")
                .bounds()
                .center();
            window.drag(from, to, cx);
        })
        .unwrap();
        cx.run_until_parked();
    };
    let summary = |cx: &mut TestAppContext| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("download-target").value(),
                Some("/local/tester/目录")
            );
            window.find("transfer-summary").label().unwrap().to_string()
        })
        .unwrap()
    };

    // A name that is not selected is selected alone and goes alone.
    cx.update_window(handle.into(), |_, window, cx| {
        window
            .within(("remote-pane", SFTP_TAB))
            .click("name:文件 甲.txt", cx)
    })
    .unwrap();
    cx.run_until_parked();
    drag_name(cx, "文件 乙.txt");
    cx.update(|cx| assert_eq!(pane_selection(&workspace, true, cx), ["文件 乙.txt"]));
    assert!(summary(cx).ends_with(" 下载 1 个文件"));
    cx.update_window(handle.into(), |_, window, cx| window.press("escape", cx))
        .unwrap();
    cx.run_until_parked();

    // A selected name takes the whole selection with it.
    cx.update_window(handle.into(), |_, window, cx| {
        modified_click(
            window,
            ("remote-pane", SFTP_TAB),
            "name:文件 甲.txt",
            gpui_kit::Modifiers::secondary_key(),
            cx,
        )
    })
    .unwrap();
    cx.run_until_parked();
    drag_name(cx, "文件 乙.txt");
    cx.update(|cx| {
        assert_eq!(
            pane_selection(&workspace, true, cx),
            ["文件 乙.txt", "文件 甲.txt"]
        )
    });
    assert!(summary(cx).ends_with(" 下载 2 个文件"));
}

/// A file drag shows what it will do only over the pane that takes it; over
/// its own list the pointer says no instead, as in WinSCP.
#[gpui_kit::test]
async fn sftp_a_file_drag_shows_its_preview_only_over_the_other_pane(cx: &mut TestAppContext) {
    use gpui_kit::{MouseDownEvent, MouseUpEvent};
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, _) = open_workspace_with_sftp(cx, provider);
    open_test_explorer(cx, handle).await;
    let move_to = |window: &mut gpui_kit::Window, position, cx: &mut App| {
        window.dispatch_event(
            MouseMoveEvent {
                position,
                pressed_button: Some(MouseButton::Left),
                modifiers: gpui_kit::Modifiers::default(),
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
        window
            .try_find("file-drag-preview")
            .and_then(|preview| preview.label().map(str::to_string))
    };
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let local = window.within(("local-pane", SFTP_TAB));
        let from = local.find("name:文件 甲.txt").bounds().center();
        let still_local = local.find("size:目录").bounds().center();
        let remote = window
            .within(("remote-pane", SFTP_TAB))
            .find("size:文件 乙.txt")
            .bounds()
            .center();
        window.dispatch_event(
            MouseDownEvent {
                button: MouseButton::Left,
                position: from,
                modifiers: gpui_kit::Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            }
            .to_platform_input(),
            cx,
        );
        // Dragging, but still over its own list: no preview.
        assert_eq!(move_to(window, still_local, cx), None);
        // Over the other pane it says what a drop does.
        assert_eq!(
            move_to(window, remote, cx).as_deref(),
            Some("上传 1 个项目")
        );
        // And back.
        assert_eq!(move_to(window, still_local, cx), None);
        // Let go where nothing takes it.
        window.dispatch_event(
            MouseUpEvent {
                button: MouseButton::Left,
                position: still_local,
                modifiers: gpui_kit::Modifiers::default(),
                click_count: 1,
            }
            .to_platform_input(),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("upload-confirm").is_none());
    })
    .unwrap();
}
