//! 命令片段: the right sidebar's tool that keeps commands for every host,
//! in categories, and puts them on the SSH terminal's line.

use crate::support::*;
use shellrs::app::{
    DeleteSnippet, DeleteSnippetCategory, EditSnippet, NewSnippetCategory, NewSnippetIn,
    RenameSnippetCategory,
};
use shellrs::host::{SnippetCategoryId, SnippetDraft, SnippetId};

/// Two categories and a snippet of none, made in an order the list does
/// not keep: Docker's 「容器」 and 「镜像」, 日志's 「系统日志」, and 「磁盘」.
fn store_with_snippets() -> HostStore {
    let mut store = HostStore::seed();
    let logs = store.insert_snippet_category_unnotified("日志");
    let docker = store.insert_snippet_category_unnotified("Docker");
    store.insert_snippet_unnotified(SnippetDraft::new("镜像", "docker images", Some(docker)));
    store.insert_snippet_unnotified(
        SnippetDraft::new("系统日志", "journalctl -f", Some(logs)).with_run_on_click(true),
    );
    store.insert_snippet_unnotified(SnippetDraft::new("磁盘", "df -h", None));
    store.insert_snippet_unnotified(SnippetDraft::new("容器", "docker ps -a", Some(docker)));
    store
}

const IMAGES: u64 = 1;
const JOURNAL: u64 = 2;
const DISK: u64 = 3;
const CONTAINERS: u64 = 4;
const LOGS: u64 = 1;
const DOCKER: u64 = 2;

/// 命令片段 shown beside web-01's terminal, once its shell runs.
async fn open_snippets(
    cx: &mut TestAppContext,
    store: HostStore,
) -> (
    WindowHandle<Root>,
    Entity<Workspace>,
    Arc<FakeTerminalFactory>,
) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_workspace_with_remote_factory(cx, store, factory.clone());
    in_frame(cx, handle, |window, cx| {
        window.activate_window();
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| window.click("tool-snippets", cx));
    let terminal = RemoteTerminalId(INITIAL_WEB_TERMINAL);
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
        remote_lifecycle(&workspace, terminal, cx).accepts_input()
    })
    .await;
    (handle, workspace, factory)
}

/// A closed dialog leaves nothing focused, and an action dispatched from
/// nothing goes nowhere: a click in the panel first, as a user would.
fn dispatch_from_panel(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    action: impl gpui_kit::Action,
) {
    in_frame(cx, handle, |window, cx| {
        window.click("snippets-summary", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(action), cx)
    });
}

/// The pointer onto a snippet's card, which shows what a click on it does.
fn point_at(cx: &mut TestAppContext, handle: WindowHandle<Root>, snippet: u64) {
    in_frame(cx, handle, |window, cx| {
        let position = window.find(("snippet", snippet)).bounds().center();
        window.dispatch_event(
            MouseMoveEvent {
                position,
                pressed_button: None,
                modifiers: gpui_kit::Modifiers::default(),
            }
            .to_platform_input(),
            cx,
        );
    });
}

fn top(window: &mut gpui_kit::Window, id: impl Into<ElementId>) -> gpui_kit::Pixels {
    window.find(id).bounds().top()
}

fn names(workspace: &Entity<Workspace>, cx: &mut TestAppContext) -> Vec<String> {
    workspace.read_with(cx, |workspace, cx| {
        workspace
            .store()
            .read(cx)
            .snippets()
            .iter()
            .map(|snippet| snippet.name.to_string())
            .collect()
    })
}

#[gpui_kit::test]
async fn snippets_are_listed_by_category_and_a_click_puts_one_on_the_line(cx: &mut TestAppContext) {
    let (handle, _, factory) = open_snippets(cx, store_with_snippets()).await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("snippets-summary").label(),
            Some("共 4 个片段 · 2 个分类")
        );
        // Docker, then 日志, each by name; the rest last, under 未分类.
        let order = [
            top(window, "snippet-category:2"),
            top(window, ("snippet", CONTAINERS)),
            top(window, ("snippet", IMAGES)),
            top(window, "snippet-category:1"),
            top(window, ("snippet", JOURNAL)),
            top(window, "snippet-category:none"),
            top(window, ("snippet", DISK)),
        ];
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{order:?}");
        assert_eq!(window.find("snippet-category:2").label(), Some("Docker 2"));
        assert_eq!(
            window.find(("snippet", CONTAINERS)).label(),
            Some("容器 · docker ps -a")
        );
    });

    // A click puts it on the line in place of what is typed there, and the
    // terminal takes the keyboard.
    let before = factory.written_text().len();
    in_frame(cx, handle, |window, cx| {
        window.click(("snippet", CONTAINERS), cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("terminal", INITIAL_WEB_TERMINAL)).focused(),
            Some(true)
        );
    });
    assert_eq!(&factory.written_text()[before..], "\x05\x15docker ps -a");

    // 执行 shows with the pointer on the card, and runs it, and only that.
    in_frame(cx, handle, |window, _| {
        assert!(!window.find(("snippet-run", DISK)).visible());
    });
    point_at(cx, handle, DISK);
    let before = factory.written_text().len();
    in_frame(cx, handle, |window, cx| {
        assert!(window.find(("snippet-run", DISK)).visible());
        assert!(!window.find(("snippet-run", JOURNAL)).visible());
        window.click(("snippet-run", DISK), cx)
    });
    assert_eq!(&factory.written_text()[before..], "\x05\x15df -h\r");

    // One set to run on a click says so, and runs.
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("snippet", JOURNAL)).label(),
            Some("系统日志 · journalctl -f · 点击时自动执行")
        );
    });
    let before = factory.written_text().len();
    in_frame(cx, handle, |window, cx| {
        window.click(("snippet", JOURNAL), cx)
    });
    assert_eq!(&factory.written_text()[before..], "\x05\x15journalctl -f\r");
}

#[gpui_kit::test]
async fn a_category_folds_away_and_the_search_finds_through_it(cx: &mut TestAppContext) {
    let (handle, _, _) = open_snippets(cx, store_with_snippets()).await;
    in_frame(cx, handle, |window, cx| {
        window.click("snippet-category:2", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find(("snippet", CONTAINERS)).is_none());
        assert_eq!(window.find("snippet-category:2").label(), Some("Docker 2"));
        assert!(window.try_find(("snippet", JOURNAL)).is_some());
    });

    // The search looks in names and commands, folded or not, and leaves
    // out the categories with nothing that matches.
    in_frame(cx, handle, |window, cx| {
        window.click("snippets-search", cx);
        window.input("PS", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find(("snippet", CONTAINERS)).is_some());
        assert_eq!(window.find("snippet-category:2").label(), Some("Docker 1"));
        assert!(window.try_find("snippet-category:1").is_none());
        assert!(window.try_find(("snippet", DISK)).is_none());
    });
    in_frame(cx, handle, |window, cx| window.input(" kubectl", cx));
    in_frame(cx, handle, |window, _| {
        assert!(window.find("snippets-no-match").visible());
    });
}

#[gpui_kit::test]
async fn a_snippet_is_made_edited_and_deleted(cx: &mut TestAppContext) {
    let (handle, workspace, _) = open_snippets(cx, HostStore::seed()).await;
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("snippets-empty").label(),
            Some("还没有命令片段")
        );
        window.click("snippets-empty-new", cx);
    });

    // The name takes the keyboard at once; without a command it is refused.
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("snippet-name").focused(), Some(true));
        window.input("查看端口", cx);
        window.click("commit", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window
            .try_find("form-error")
            .is_some_and(|error| error.label() == Some("请输入命令"))
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click("snippet-command", cx);
        window.input("ss -tlnp", cx);
        window.click("commit", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find(("snippet", 1u64)).is_some()
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("snippet", 1u64)).label(),
            Some("查看端口 · ss -tlnp · 点击时自动执行")
        );
        assert_eq!(window.find("snippets-summary").label(), Some("共 1 个片段"));
    });

    // Edited in the same dialog, where a click is told only to type it.
    dispatch_from_panel(cx, handle, EditSnippet(SnippetId(1)));
    in_frame(cx, handle, |window, cx| {
        window.press("cmd-a", cx);
        window.input("监听端口", cx);
        window.click("snippet-run-on-click", cx);
        window.click("commit", cx);
    });
    cx.run_until_parked();
    assert_eq!(names(&workspace, cx), ["监听端口"]);
    let runs = workspace.read_with(cx, |workspace, cx| {
        workspace.store().read(cx).snippets()[0].run_on_click
    });
    assert!(!runs);

    // Deleted once confirmed.
    dispatch_from_panel(cx, handle, DeleteSnippet(SnippetId(1)));
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    assert!(names(&workspace, cx).is_empty());
    in_frame(cx, handle, |window, _| {
        assert!(window.find("snippets-empty").visible());
    });
}

#[gpui_kit::test]
async fn a_category_is_made_renamed_and_deleted_with_its_snippets(cx: &mut TestAppContext) {
    let (handle, workspace, _) = open_snippets(cx, store_with_snippets()).await;

    // A name the list already has is refused.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(NewSnippetCategory), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.input("Docker", cx);
        window.click("commit", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window
            .try_find("form-error")
            .is_some_and(|error| error.label() == Some("已有同名分类"))
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click("snippet-category-name", cx);
        window.press("cmd-a", cx);
        window.input("部署", cx);
        window.click("commit", cx);
    });
    // Empty, it shows until something goes in it.
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window
            .try_find("snippet-category:3")
            .is_some_and(|heading| heading.label() == Some("部署 0"))
    })
    .await;

    // A snippet made from its menu goes in it.
    dispatch_from_panel(cx, handle, NewSnippetIn(SnippetCategoryId(3)));
    in_frame(cx, handle, |window, cx| {
        window.input("发布", cx);
        window.click("snippet-command", cx);
        window.input("./deploy.sh", cx);
        window.click("commit", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window
            .try_find("snippet-category:3")
            .is_some_and(|heading| heading.label() == Some("部署 1"))
    })
    .await;

    dispatch_from_panel(cx, handle, RenameSnippetCategory(SnippetCategoryId(LOGS)));
    in_frame(cx, handle, |window, cx| {
        window.press("cmd-a", cx);
        window.input("日志查看", cx);
        window.click("commit", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window
            .try_find("snippet-category:1")
            .is_some_and(|heading| heading.label() == Some("日志查看 1"))
    })
    .await;

    // Deleting Docker takes its two snippets.
    dispatch_from_panel(cx, handle, DeleteSnippetCategory(SnippetCategoryId(DOCKER)));
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    assert_eq!(names(&workspace, cx), ["系统日志", "磁盘", "发布"]);
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("snippet-category:2").is_none());
    });
}

#[gpui_kit::test]
async fn on_windows_a_snippet_is_typed_after_what_is_there(cx: &mut TestAppContext) {
    let mut store = store_with_snippets();
    store.set_host_os_unnotified(HostId(WEB_01), Some(HostOs::Windows));
    let (handle, _, factory) = open_snippets(cx, store).await;
    point_at(cx, handle, DISK);
    let before = factory.written_text().len();
    in_frame(cx, handle, |window, cx| {
        window.click(("snippet-run", DISK), cx)
    });
    // No Ctrl-E, Ctrl-U: its shells do not edit the line with them.
    assert_eq!(&factory.written_text()[before..], "df -h\r");
}
