//! 项目: the right sidebar's tool beside local terminals, which keeps them in
//! groups, opens them again in the folders they were last in, and the next
//! launch opening those left open.

use crate::support::*;
use gpui_kit::component::Placement;
use shellrs::app::{
    CloseLocalTerminal, CloseSavedTerminal, DeleteSavedTerminal, DeleteTerminalGroup,
    NewSavedTerminal, OpenSavedTerminal, RestartLocalTerminal, ToolKind,
};
use shellrs::host::{SavedTerminalId, SavedTerminalTab, TerminalGroupId};
use shellrs::terminal::LocalLaunch;

/// The groups and terminals of `projects_store`, in the order made.
const SHELLRS: TerminalGroupId = TerminalGroupId(1);
const BLOG: TerminalGroupId = TerminalGroupId(2);
/// `shellrs 1`, last in `/code/shellrs`.
const FIRST: SavedTerminalId = SavedTerminalId(1);
/// `shellrs 2`, last in `/code/shellrs/target`.
const SECOND: SavedTerminalId = SavedTerminalId(2);
/// `blog 1`, which never said where it was.
const BLOG_FIRST: SavedTerminalId = SavedTerminalId(3);

/// The seed with two groups: shellrs with two terminals, blog with one.
fn projects_store() -> HostStore {
    let mut store = HostStore::seed();
    let shellrs = store.insert_terminal_group_unnotified("shellrs".into());
    let blog = store.insert_terminal_group_unnotified("blog".into());
    store.insert_saved_terminal_unnotified(Some(shellrs), Some("/code/shellrs".into()));
    store.insert_saved_terminal_unnotified(Some(shellrs), Some("/code/shellrs/target".into()));
    store.insert_saved_terminal_unnotified(Some(blog), None);
    store
}

/// A workspace on `store`, its local terminals started through `local`.
fn open_with_local(
    cx: &mut TestAppContext,
    store: HostStore,
    local: Arc<FixedLocalTerminalTransportProvider>,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    init_app(cx);
    cx.update(|cx| cx.set_reduce_motion(true));
    let mut workspace = None;
    let handle = cx.open_window(size(px(1280.), px(800.)), |window, cx| {
        let store = cx.new(|_| store);
        let remote = Arc::new(FixedRemoteTerminalTransportProvider::new(Arc::new(
            FakeTerminalFactory::default(),
        )));
        let view = cx.new(|cx| {
            Workspace::new_with_services(
                store,
                cx.new(|_| settings_store()),
                remote,
                fake_telnet_terminals(),
                fake_serial_terminals(),
                local.clone(),
                Arc::new(FakeSftpProvider::default()),
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
    cx.run_until_parked();
    (handle, workspace.expect("workspace created"))
}

fn launch(directory: Option<&str>) -> LocalLaunch {
    LocalLaunch {
        directory: directory.map(Into::into),
        ..LocalLaunch::default()
    }
}

fn saved(workspace: &Entity<Workspace>, cx: &mut TestAppContext) -> Vec<SavedTerminalTab> {
    cx.update(|cx| {
        workspace
            .read(cx)
            .store()
            .read(cx)
            .saved_terminal_tabs()
            .to_vec()
    })
}

fn tab(terminal: SavedTerminalId, front: bool) -> SavedTerminalTab {
    SavedTerminalTab { terminal, front }
}

fn dispatch(cx: &mut TestAppContext, handle: WindowHandle<Root>, action: impl gpui_kit::Action) {
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(action), cx)
    });
}

/// A local terminal in front, with 项目 open beside it.
fn open_projects(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    dispatch(cx, handle, NewLocalTerminal);
    in_frame(cx, handle, |window, cx| window.click("tool-projects", cx));
}

/// Put the keyboard back in the terminals, blog 1's last, as a click on a
/// tab does: a dialog closing takes it with it, and a menu's command is
/// dispatched from it.
fn focus_a_tab(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    for tab in [1_u64, 2] {
        in_frame(cx, handle, |window, cx| {
            window.click(("local-terminal-tab", tab), cx)
        });
    }
}

fn mark(cx: &mut TestAppContext, handle: WindowHandle<Root>, id: SavedTerminalId) -> String {
    let mut label = String::new();
    in_frame(cx, handle, |window, _| {
        label = window
            .within("project-tree")
            .find(("saved-terminal-mark", id.0))
            .label()
            .unwrap_or_default()
            .to_owned();
    });
    label
}

#[gpui_kit::test]
async fn a_group_made_by_name_gets_terminals_from_its_plus(cx: &mut TestAppContext) {
    let local = fake_local_terminals();
    let (handle, workspace) = open_with_local(cx, HostStore::seed(), local.clone());
    open_projects(cx, handle);
    let settings = cx.update(|cx| workspace.read(cx).settings().clone());
    assert_eq!(
        cx.update(|cx| settings.read(cx).settings().local_sidebar_tool),
        Some(ToolKind::Projects)
    );
    in_frame(cx, handle, |window, _| {
        assert!(window.find("projects-empty").visible());
    });

    // A group: its name, and nothing else.
    in_frame(cx, handle, |window, cx| {
        window.click("projects-empty-new-group", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("project-name", cx);
        window.input("shellrs", cx);
        window.click("commit", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .within("project-tree")
                .find(("terminal-group-row", SHELLRS.0))
                .visible()
        );
    });

    // Its 「+」: a terminal kept in it, open at once, named after it.
    // Twice: the second press opens one more, and leaves the group open.
    for (saved, tab) in [(FIRST, 2_u64), (SECOND, 3)] {
        in_frame(cx, handle, |window, cx| {
            window.click(("new-saved-terminal", SHELLRS.0), cx)
        });
        assert_eq!(
            cx.update(|cx| workspace.read(cx).active_pane()),
            Some(PaneContent::LocalTerminal(LocalTerminalId(tab)))
        );
        in_frame(cx, handle, |window, _| {
            assert_eq!(
                window.find(("local-terminal-tab", tab)).label(),
                Some(format!("shellrs {}", saved.0).as_str())
            );
        });
        assert_eq!(mark(cx, handle, saved), "已打开");
    }
    // Nobody said where the first was: both in the home folder.
    assert_eq!(local.launches()[1..], [launch(None), launch(None)]);

    // A second group of the same name is refused.
    in_frame(cx, handle, |window, cx| {
        window.click("new-terminal-group", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("project-name", cx);
        window.input("shellrs", cx);
        window.click("commit", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.find("form-error").visible());
        assert!(window.find("commit").visible());
    });
}

#[gpui_kit::test]
async fn a_closed_terminal_stays_and_opens_again_where_its_shell_was(cx: &mut TestAppContext) {
    let local = Arc::new(FixedLocalTerminalTransportProvider::new(Arc::new(
        FakeTerminalFactory {
            behavior: FakeBehavior::InFolder("/code/shellrs/src"),
            ..FakeTerminalFactory::default()
        },
    )));
    let (handle, workspace) = open_with_local(cx, projects_store(), local.clone());
    open_projects(cx, handle);
    in_frame(cx, handle, |window, cx| {
        window
            .within("project-tree")
            .click(("saved-terminal-row", FIRST.0), cx)
    });
    assert_eq!(
        local.launches().last(),
        Some(&launch(Some("/code/shellrs")))
    );
    let tab = LocalTerminalId(2);
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("local-terminal-tab", tab.0)).label(),
            Some("shellrs 1")
        );
    });

    // The shell moves: the terminal remembers where.
    let store = cx.update(|cx| workspace.read(cx).store().clone());
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        store
            .read(cx)
            .saved_terminal(FIRST)
            .unwrap()
            .directory
            .as_deref()
            == Some("/code/shellrs/src")
    })
    .await;

    // Closed, it stays in its group, grey.
    dispatch(cx, handle, CloseSavedTerminal(FIRST));
    assert!(cx.update(|cx| workspace.read(cx).local_terminal(tab).is_none()));
    assert_eq!(mark(cx, handle, FIRST), "未打开");
    assert!(saved(&workspace, cx).is_empty());

    // A click opens it again where its shell was last.
    in_frame(cx, handle, |window, cx| {
        window
            .within("project-tree")
            .click(("saved-terminal-row", FIRST.0), cx)
    });
    assert_eq!(
        local.launches().last(),
        Some(&launch(Some("/code/shellrs/src")))
    );
    assert_eq!(mark(cx, handle, FIRST), "已打开");
    // Open, a click brings it to the front rather than opening another.
    in_frame(cx, handle, |window, cx| {
        window.click(("local-terminal-tab", 1_u64), cx)
    });
    let started = local.launches().len();
    dispatch(cx, handle, OpenSavedTerminal(FIRST));
    assert_eq!(local.launches().len(), started);
    assert_eq!(
        cx.update(|cx| workspace.read(cx).active_pane()),
        Some(PaneContent::LocalTerminal(LocalTerminalId(3)))
    );

    // 重新启动 starts it where its shell was, with the shell set now.
    let settings = cx.update(|cx| workspace.read(cx).settings().clone());
    settings.update(cx, |settings, cx| {
        settings.update(
            |settings| settings.local_terminal.shell = Some("/bin/bash".into()),
            cx,
        )
    });
    dispatch(cx, handle, RestartLocalTerminal(LocalTerminalId(3)));
    assert_eq!(
        local.launches().last(),
        Some(&LocalLaunch {
            shell: LocalShell::new("/bin/bash"),
            ..launch(Some("/code/shellrs/src"))
        })
    );
}

#[gpui_kit::test]
async fn a_new_terminal_starts_where_its_group_last_was(cx: &mut TestAppContext) {
    let local = fake_local_terminals();
    let (handle, workspace) = open_with_local(cx, projects_store(), local.clone());
    dispatch(cx, handle, NewSavedTerminal(Some(SHELLRS)));
    assert_eq!(
        local.launches().last(),
        Some(&launch(Some("/code/shellrs/target")))
    );
    let names: Vec<String> = cx.update(|cx| {
        workspace
            .read(cx)
            .store()
            .read(cx)
            .terminals_in(Some(SHELLRS))
            .iter()
            .map(|terminal| terminal.name.to_string())
            .collect()
    });
    assert_eq!(names, ["shellrs 1", "shellrs 2", "shellrs 3"]);
    // A group whose terminals never said starts in the home folder.
    dispatch(cx, handle, NewSavedTerminal(Some(BLOG)));
    assert_eq!(local.launches().last(), Some(&launch(None)));
}

/// Rename shellrs 1 in its dialog, `name` empty to leave it without one.
async fn rename_first(cx: &mut TestAppContext, handle: WindowHandle<Root>, name: &str) {
    focus_a_tab(cx, handle);
    dispatch(cx, handle, shellrs::app::RenameSavedTerminal(FIRST));
    in_frame(cx, handle, |window, cx| {
        window.click("project-name", cx);
        window.press("secondary-a", cx);
        window.press("backspace", cx);
        if !name.is_empty() {
            window.input(name, cx);
        }
        window.click("commit", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;
}

/// What the tab of local terminal 1 and shellrs 1's row in 项目 are called.
fn first_names(cx: &mut TestAppContext, handle: WindowHandle<Root>) -> (String, String) {
    let mut names = (String::new(), String::new());
    in_frame(cx, handle, |window, _| {
        let tab = window.find(("local-terminal-tab", 1_u64));
        let row = window
            .within("project-tree")
            .find(("saved-terminal-row", FIRST.0));
        names = (
            tab.label().unwrap_or_default().to_owned(),
            row.label().unwrap_or_default().to_owned(),
        );
    });
    names
}

#[gpui_kit::test]
async fn a_programs_title_names_the_terminal_until_it_is_named_by_hand(cx: &mut TestAppContext) {
    // Claude Code titles its terminal, as a terminal app's tab shows.
    let local = Arc::new(FixedLocalTerminalTransportProvider::new(Arc::new(
        FakeTerminalFactory::printing("\x1b]0;✳ Claude Code\x07"),
    )));
    let (handle, _workspace) = open_with_local(cx, projects_store(), local);
    dispatch(cx, handle, OpenSavedTerminal(FIRST));
    dispatch(cx, handle, OpenSavedTerminal(BLOG_FIRST));
    in_frame(cx, handle, |window, cx| window.click("tool-projects", cx));
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window
            .within("project-tree")
            .try_find(("saved-terminal-row", FIRST.0))
            .is_some_and(|row| row.label() == Some("✳ Claude Code"))
    })
    .await;
    let claude = || ("✳ Claude Code".to_owned(), "✳ Claude Code".to_owned());
    assert_eq!(first_names(cx, handle), claude());

    // Named by hand, the name shows over the title; left without one, the
    // title again.
    rename_first(cx, handle, "claude").await;
    assert_eq!(
        first_names(cx, handle),
        ("claude".to_owned(), "claude".to_owned())
    );
    rename_first(cx, handle, "").await;
    assert_eq!(first_names(cx, handle), claude());
}

/// What 项目's row of `id` and local terminal `tab`'s tab are called.
fn names(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    id: SavedTerminalId,
    tab: u64,
) -> (String, String) {
    let mut names = (String::new(), String::new());
    in_frame(cx, handle, |window, _| {
        let row = window
            .within("project-tree")
            .find(("saved-terminal-row", id.0));
        let tab = window.find(("local-terminal-tab", tab));
        names = (
            row.label().unwrap_or_default().to_owned(),
            tab.label().unwrap_or_default().to_owned(),
        );
    });
    names
}

#[gpui_kit::test]
async fn a_terminals_last_title_names_it_closed_and_at_the_next_launch(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shellrs.db");
    let database = HostDatabase::open(&path).unwrap();
    let store = projects_store();
    for group in store.terminal_groups() {
        database.insert_terminal_group(group).unwrap();
    }
    for terminal in store.saved_terminals() {
        database.insert_saved_terminal(terminal).unwrap();
    }
    let local = Arc::new(FixedLocalTerminalTransportProvider::new(Arc::new(
        FakeTerminalFactory::printing("\x1b]0;✳ Claude Code\x07"),
    )));
    let (handle, workspace) = open_with_local(cx, HostStore::load(database).unwrap(), local);
    dispatch(cx, handle, OpenSavedTerminal(FIRST));
    dispatch(cx, handle, OpenSavedTerminal(BLOG_FIRST));
    in_frame(cx, handle, |window, cx| window.click("tool-projects", cx));
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        let tree = window.within("project-tree");
        [FIRST, BLOG_FIRST].into_iter().all(|id| {
            tree.try_find(("saved-terminal-row", id.0))
                .is_some_and(|row| row.label() == Some("✳ Claude Code"))
        })
    })
    .await;

    // Closed, it is called by its last title still.
    dispatch(cx, handle, CloseSavedTerminal(FIRST));
    assert_eq!(names(cx, handle, FIRST, 2).0, "✳ Claude Code");

    // So is blog 1, open at quit, at the next launch, before its shell
    // titles it; and shellrs 1, opened again.
    drop(workspace);
    cx.update_window(handle.into(), |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    let store = HostStore::load(HostDatabase::open(&path).unwrap()).unwrap();
    let (handle, _workspace) = open_with_local(cx, store, fake_local_terminals());
    in_frame(cx, handle, |window, cx| window.click("tool-projects", cx));
    let claude = ("✳ Claude Code".to_owned(), "✳ Claude Code".to_owned());
    assert_eq!(names(cx, handle, BLOG_FIRST, 1), claude);
    dispatch(cx, handle, OpenSavedTerminal(FIRST));
    assert_eq!(names(cx, handle, FIRST, 2), claude);
}

#[gpui_kit::test]
async fn renaming_names_the_tab_and_deleting_closes_it(cx: &mut TestAppContext) {
    let (handle, workspace) = open_with_local(cx, projects_store(), fake_local_terminals());
    dispatch(cx, handle, OpenSavedTerminal(FIRST));
    dispatch(cx, handle, OpenSavedTerminal(BLOG_FIRST));
    let first = LocalTerminalId(1);

    // Renamed in its dialog: the tab is called so too, until its shell
    // titles it.
    dispatch(cx, handle, shellrs::app::RenameSavedTerminal(FIRST));
    in_frame(cx, handle, |window, cx| {
        window.click("project-name", cx);
        window.press("secondary-a", cx);
        window.input("claude", cx);
        window.click("commit", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("local-terminal-tab", first.0)).label(),
            Some("claude")
        );
    });

    // Deleted while open, only after asking, for what runs in it ends;
    // its tab closes with it.
    focus_a_tab(cx, handle);
    dispatch(cx, handle, DeleteSavedTerminal(FIRST));
    let store = cx.update(|cx| workspace.read(cx).store().clone());
    assert!(cx.update(|cx| store.read(cx).saved_terminal(FIRST).is_some()));
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    cx.run_until_parked();
    assert!(cx.update(|cx| store.read(cx).saved_terminal(FIRST).is_none()));
    assert!(cx.update(|cx| workspace.read(cx).local_terminal(first).is_none()));
    assert_eq!(saved(&workspace, cx), [tab(BLOG_FIRST, true)]);

    // A group goes with its terminals, their tabs too.
    in_frame(cx, handle, |window, cx| {
        window.click(("local-terminal", 2_u64), cx)
    });
    dispatch(cx, handle, DeleteTerminalGroup(BLOG));
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    cx.run_until_parked();
    assert!(cx.update(|cx| store.read(cx).terminal_group(BLOG).is_none()));
    assert!(cx.update(|cx| store.read(cx).saved_terminal(BLOG_FIRST).is_none()));
    assert!(cx.update(|cx| {
        workspace
            .read(cx)
            .local_terminal(LocalTerminalId(2))
            .is_none()
    }));
    assert!(saved(&workspace, cx).is_empty());
}

#[gpui_kit::test]
async fn rows_are_dragged_into_order_and_deleted_from_their_hover_button(cx: &mut TestAppContext) {
    let (handle, workspace) = open_with_local(cx, projects_store(), fake_local_terminals());
    open_projects(cx, handle);
    let store = cx.update(|cx| workspace.read(cx).store().clone());
    let in_group = |group, cx: &mut TestAppContext| -> Vec<SavedTerminalId> {
        cx.update(|cx| {
            store
                .read(cx)
                .terminals_in(Some(group))
                .iter()
                .map(|terminal| terminal.id)
                .collect()
        })
    };

    // A terminal dropped on another group goes last in it.
    in_frame(cx, handle, |window, cx| {
        window.within("project-tree").drag_to(
            ("saved-terminal-row", FIRST.0),
            ("terminal-group-row", BLOG.0),
            cx,
        )
    });
    assert_eq!(in_group(BLOG, cx), [BLOG_FIRST, FIRST]);
    assert_eq!(in_group(SHELLRS, cx), [SECOND]);

    // 删除 shows on the row the pointer is on.
    let shown = |cx: &mut TestAppContext| {
        let mut shown = false;
        in_frame(cx, handle, |window, _| {
            shown = window
                .within("project-tree")
                .try_find(("delete-saved-terminal", SECOND.0))
                .is_some_and(|button| button.visible());
        });
        shown
    };
    assert!(!shown(cx));
    in_frame(cx, handle, |window, cx| {
        window
            .within("project-tree")
            .hover(("saved-terminal-row", SECOND.0), cx)
    });
    assert!(shown(cx));
    // Not open, it goes at once: nothing to ask about.
    in_frame(cx, handle, |window, cx| {
        window
            .within("project-tree")
            .click(("delete-saved-terminal", SECOND.0), cx)
    });
    assert!(cx.update(|cx| store.read(cx).saved_terminal(SECOND).is_none()));

    // A group dropped on another goes after it.
    in_frame(cx, handle, |window, cx| {
        window.within("project-tree").drag_to(
            ("terminal-group-row", SHELLRS.0),
            ("terminal-group-row", BLOG.0),
            cx,
        )
    });
    let groups = cx.update(|cx| {
        let mut groups: Vec<_> = store
            .read(cx)
            .terminal_groups()
            .iter()
            .map(|group| (group.sort_order, group.id))
            .collect();
        groups.sort();
        groups.into_iter().map(|(_, id)| id).collect::<Vec<_>>()
    });
    assert_eq!(groups, [BLOG, SHELLRS]);
}

#[gpui_kit::test]
async fn terminals_go_at_the_top_too_and_groups_fold(cx: &mut TestAppContext) {
    let local = fake_local_terminals();
    let (handle, workspace) = open_with_local(cx, projects_store(), local.clone());
    open_projects(cx, handle);
    let store = cx.update(|cx| workspace.read(cx).store().clone());

    // The title bar's 「+」: a terminal of no group, in the home folder,
    // listed below the groups.
    in_frame(cx, handle, |window, cx| {
        window.click("new-project-terminal", cx)
    });
    assert_eq!(local.launches().last(), Some(&launch(None)));
    let top: Vec<_> = cx.update(|cx| {
        store
            .read(cx)
            .terminals_in(None)
            .iter()
            .map(|terminal| (terminal.id, terminal.name.to_string()))
            .collect()
    });
    assert_eq!(top, [(SavedTerminalId(4), "终端 1".to_owned())]);
    in_frame(cx, handle, |window, _| {
        let tree = window.within("project-tree");
        let group = tree.find(("terminal-group-row", BLOG.0)).bounds();
        let row = tree.find(("saved-terminal-row", 4_u64)).bounds();
        assert!(row.top() > group.bottom());
        // A group's heading is one line, a terminal's row two.
        assert!(group.size.height < row.size.height);
    });

    // A terminal dropped off every row goes last at the top.
    in_frame(cx, handle, |window, cx| {
        window
            .within("projects")
            .drag_to(("saved-terminal-row", FIRST.0), "project-tree", cx)
    });
    let top: Vec<_> = cx.update(|cx| {
        store
            .read(cx)
            .terminals_in(None)
            .iter()
            .map(|terminal| terminal.id)
            .collect()
    });
    assert_eq!(top, [SavedTerminalId(4), FIRST]);

    // A click on a group's heading hides its terminals, another shows them.
    for shown in [false, true] {
        in_frame(cx, handle, |window, cx| {
            window
                .within("project-tree")
                .click(("terminal-group-row", SHELLRS.0), cx)
        });
        in_frame(cx, handle, |window, _| {
            let row = window
                .within("project-tree")
                .try_find(("saved-terminal-row", SECOND.0));
            assert_eq!(row.is_some(), shown);
        });
        // Kept for the next launch, as a host group's is.
        let expanded = cx.update(|cx| {
            let store = workspace.read(cx).store().read(cx);
            store.terminal_group(SHELLRS).unwrap().expanded
        });
        assert_eq!(expanded, shown);
    }
}

#[gpui_kit::test]
async fn the_saved_tabs_follow_the_saved_terminals(cx: &mut TestAppContext) {
    let (handle, workspace) = open_with_local(cx, projects_store(), fake_local_terminals());
    // shellrs 1, a plain terminal, then blog 1 in front.
    dispatch(cx, handle, OpenSavedTerminal(FIRST));
    dispatch(cx, handle, NewLocalTerminal);
    dispatch(cx, handle, OpenSavedTerminal(BLOG_FIRST));
    assert_eq!(
        saved(&workspace, cx),
        [tab(FIRST, false), tab(BLOG_FIRST, true)]
    );

    // shellrs 1 in front; a plain terminal in front leaves it the one.
    dispatch(cx, handle, OpenSavedTerminal(FIRST));
    assert_eq!(
        saved(&workspace, cx),
        [tab(FIRST, true), tab(BLOG_FIRST, false)]
    );
    in_frame(cx, handle, |window, cx| {
        window.click(("local-terminal-tab", 2_u64), cx)
    });
    assert_eq!(
        saved(&workspace, cx),
        [tab(FIRST, true), tab(BLOG_FIRST, false)]
    );

    // shellrs 1's tab dragged to the end: after blog 1.
    cx.update(|cx| {
        let center = workspace.read(cx).center().clone();
        center.update(cx, |center, cx| {
            let tab = center
                .tab_of(PaneContent::LocalTerminal(LocalTerminalId(1)))
                .expect("shellrs 1's tab");
            center.move_tab(tab, usize::MAX, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(
        saved(&workspace, cx),
        [tab(BLOG_FIRST, false), tab(FIRST, true)]
    );

    // Closed, it is no longer there to open again.
    dispatch(cx, handle, CloseLocalTerminal(LocalTerminalId(1)));
    assert_eq!(saved(&workspace, cx), [tab(BLOG_FIRST, false)]);
}

#[gpui_kit::test]
async fn closing_the_window_keeps_the_saved_tabs(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shellrs.db");
    let database = HostDatabase::open(&path).unwrap();
    let store = projects_store();
    for group in store.terminal_groups() {
        database.insert_terminal_group(group).unwrap();
    }
    for terminal in store.saved_terminals() {
        database.insert_saved_terminal(terminal).unwrap();
    }
    let (handle, _) = open_with_local(
        cx,
        HostStore::load(database).unwrap(),
        fake_local_terminals(),
    );
    dispatch(cx, handle, OpenSavedTerminal(FIRST));
    dispatch(cx, handle, OpenSavedTerminal(BLOG_FIRST));

    // As quitting does: the window goes, its tabs with it, unclosed.
    cx.update_window(handle.into(), |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    let data = HostDatabase::open(&path).unwrap().load().unwrap();
    assert_eq!(
        data.saved_terminal_tabs,
        [tab(FIRST, false), tab(BLOG_FIRST, true)]
    );
}

#[gpui_kit::test]
async fn the_next_launch_opens_the_saved_terminals_left_open(cx: &mut TestAppContext) {
    let mut store = projects_store();
    store.set_saved_terminal_tabs_unnotified(vec![tab(SECOND, false), tab(FIRST, true)]);
    let local = fake_local_terminals();
    let (handle, workspace) = open_with_local(cx, store, local.clone());

    // In their order after the seed's terminals, each where it was, the
    // one in front then in front again.
    assert_eq!(
        local.launches(),
        [
            launch(Some("/code/shellrs/target")),
            launch(Some("/code/shellrs")),
        ]
    );
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("local-terminal-tab", 1_u64)).label(),
            Some("shellrs 2")
        );
        assert_eq!(
            window.find(("local-terminal-tab", 2_u64)).label(),
            Some("shellrs 1")
        );
    });
    assert_eq!(
        cx.update(|cx| workspace.read(cx).active_pane()),
        Some(PaneContent::LocalTerminal(LocalTerminalId(2)))
    );
    assert_eq!(
        saved(&workspace, cx),
        [tab(SECOND, false), tab(FIRST, true)]
    );
    // 项目 marks them open, and the one left closed closed.
    in_frame(cx, handle, |window, cx| window.click("tool-projects", cx));
    assert_eq!(mark(cx, handle, FIRST), "已打开");
    assert_eq!(mark(cx, handle, SECOND), "已打开");
    assert_eq!(mark(cx, handle, BLOG_FIRST), "未打开");
    // A new terminal comes after them.
    dispatch(cx, handle, NewLocalTerminal);
    assert!(cx.update(|cx| {
        workspace
            .read(cx)
            .local_terminal(LocalTerminalId(3))
            .is_some()
    }));
}

#[gpui_kit::test]
async fn with_the_center_split_the_sidebar_follows_the_terminal_clicked_last(
    cx: &mut TestAppContext,
) {
    let (handle, workspace) = open_with_local(cx, projects_store(), fake_local_terminals());
    in_frame(cx, handle, |window, _| window.activate_window());
    // web-01's terminal with 命令片段 open; a local one with 项目.
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| window.click("tool-snippets", cx));
    open_projects(cx, handle);

    // The local terminal on the right of web-01's.
    split_beside(
        cx,
        handle,
        &workspace,
        PaneContent::LocalTerminal(LocalTerminalId(1)),
        PaneContent::Terminal(RemoteTerminalId(INITIAL_WEB_TERMINAL)),
        Placement::Right,
    );
    // web-01's terminal on the left, beside the local one.
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });

    for (terminal, tool) in [
        (("terminal", INITIAL_WEB_TERMINAL), "tool-snippets"),
        (("local-terminal", 1_u64), "tool-projects"),
        (("terminal", INITIAL_WEB_TERMINAL), "tool-snippets"),
    ] {
        in_frame(cx, handle, |window, cx| window.click(terminal, cx));
        in_frame(cx, handle, |window, _| {
            assert_eq!(window.find(tool).checked(), Some(true), "{tool}");
        });
    }
}

#[gpui_kit::test]
async fn projects_hidden_in_the_settings_leave_local_terminals_without_a_sidebar(
    cx: &mut TestAppContext,
) {
    let (handle, workspace) = open_with_local(cx, projects_store(), fake_local_terminals());
    open_projects(cx, handle);
    let settings = cx.update(|cx| workspace.read(cx).settings().clone());
    settings.update(cx, |settings, cx| {
        settings.update(
            |settings| settings.sidebar_tools.set(ToolKind::Projects, false),
            cx,
        )
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("tool-switch").is_none());
        assert!(window.try_find(("tool-sidebar-local", 1_u64)).is_none());
    });
}

#[gpui_kit::test]
async fn a_link_after_the_launch_opens_its_terminal_in_front(cx: &mut TestAppContext) {
    let mut store = projects_store();
    store.set_saved_terminal_tabs_unnotified(vec![tab(FIRST, true), tab(BLOG_FIRST, false)]);
    let (handle, workspace) = open_with_local(cx, store, fake_local_terminals());
    assert_eq!(
        cx.update(|cx| workspace.read(cx).active_pane()),
        Some(PaneContent::LocalTerminal(LocalTerminalId(1)))
    );
    // As `main` does once the window is open.
    cx.update_window(handle.into(), |_, window, cx| {
        let link = shellrs::cli::OpenLink {
            url: "ssh://root@10.0.0.9".into(),
            tab: None,
        };
        shellrs::workspace::open_link_once_open(&workspace, link, window, cx);
    })
    .unwrap();
    cx.run_until_parked();
    in_frame(cx, handle, |_, _| {});
    assert_eq!(
        cx.update(|cx| workspace.read(cx).active_pane()),
        Some(PaneContent::Terminal(RemoteTerminalId(FIRST_NEW_TERMINAL)))
    );
}

/// Whether what `launch` draws first holds `text`.
fn replays(launch: &LocalLaunch, text: &str) -> bool {
    launch
        .replay
        .as_deref()
        .is_some_and(|replay| String::from_utf8_lossy(replay).contains(text))
}

/// Until tab `tab` shows the fake shell's first line.
async fn wait_for_output(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    workspace: &Entity<Workspace>,
    tab: u64,
) {
    let workspace = workspace.clone();
    cx.wait_for(handle.into(), Duration::from_secs(2), move |_, cx| {
        workspace
            .read(cx)
            .local_terminal(LocalTerminalId(tab))
            .and_then(|panel| panel.read(cx).terminal().read(cx).kept_output(cx))
            .is_some_and(|output| String::from_utf8_lossy(&output).contains("alpha.txt"))
    })
    .await;
}

#[gpui_kit::test]
async fn a_closed_terminal_draws_again_what_it_showed(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let outputs = dir.path().join("terminal-output");
    let local = fake_local_terminals();
    let (handle, workspace) = open_with_local(
        cx,
        projects_store().with_terminal_output_dir(&outputs),
        local.clone(),
    );
    dispatch(cx, handle, OpenSavedTerminal(FIRST));
    assert_eq!(local.launches().last().unwrap().replay, None);
    wait_for_output(cx, handle, &workspace, 1).await;

    // Closed, what it showed is kept…
    dispatch(cx, handle, CloseSavedTerminal(FIRST));
    let kept = std::fs::read(outputs.join(format!("{}.ansi", FIRST.0))).unwrap();
    assert!(String::from_utf8_lossy(&kept).contains("alpha.txt"));

    // …and drawn first when it opens again, with a line saying so.
    dispatch(cx, handle, OpenSavedTerminal(FIRST));
    let opened = local.launches().last().unwrap().clone();
    assert!(replays(&opened, "alpha.txt"));
    assert!(replays(&opened, "以上是上次关闭时的内容"));
    // Not when it restarts.
    dispatch(cx, handle, RestartLocalTerminal(LocalTerminalId(2)));
    assert_eq!(local.launches().last().unwrap().replay, None);

    // A plain terminal keeps nothing.
    dispatch(cx, handle, NewLocalTerminal);
    wait_for_output(cx, handle, &workspace, 3).await;
    dispatch(cx, handle, CloseLocalTerminal(LocalTerminalId(3)));
    assert_eq!(std::fs::read_dir(&outputs).unwrap().count(), 1);

    // Deleted, open, it takes what it kept with it, keeping nothing as its
    // tab closes.
    dispatch(cx, handle, DeleteSavedTerminal(FIRST));
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    cx.run_until_parked();
    assert_eq!(std::fs::read_dir(&outputs).unwrap().count(), 0);
}

#[gpui_kit::test]
async fn the_next_launch_draws_what_the_terminals_showed_at_quit(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shellrs.db");
    let outputs = dir.path().join("terminal-output");
    let database = HostDatabase::open(&path).unwrap();
    let store = projects_store();
    for group in store.terminal_groups() {
        database.insert_terminal_group(group).unwrap();
    }
    for terminal in store.saved_terminals() {
        database.insert_saved_terminal(terminal).unwrap();
    }
    let store = HostStore::load(database)
        .unwrap()
        .with_terminal_output_dir(&outputs);
    let (handle, workspace) = open_with_local(cx, store, fake_local_terminals());
    dispatch(cx, handle, OpenSavedTerminal(FIRST));
    wait_for_output(cx, handle, &workspace, 1).await;

    // As quitting does: the window goes, its tabs with it, unclosed, and
    // the workspace with the window, which alone holds it.
    drop(workspace);
    cx.update_window(handle.into(), |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    assert!(outputs.join(format!("{}.ansi", FIRST.0)).exists());

    let store = HostStore::load(HostDatabase::open(&path).unwrap())
        .unwrap()
        .with_terminal_output_dir(&outputs);
    let local = fake_local_terminals();
    let (_, _) = open_with_local(cx, store, local.clone());
    assert!(replays(&local.launches()[0], "alpha.txt"));
}

#[gpui_kit::test]
async fn turned_off_terminal_output_is_neither_kept_nor_drawn(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let outputs = dir.path().join("terminal-output");
    let store = projects_store().with_terminal_output_dir(&outputs);
    store.keep_output(FIRST, Some(b"$ ls\r\nalpha.txt\r\n".to_vec()));
    let local = fake_local_terminals();
    let (handle, workspace) = open_with_local(cx, store, local.clone());

    // Turning it off deletes what was kept.
    let settings = cx.update(|cx| workspace.read(cx).settings().clone());
    settings.update(cx, |settings, cx| {
        settings.update(|settings| settings.local_terminal.keep_output = false, cx)
    });
    cx.run_until_parked();
    assert!(!outputs.exists());

    dispatch(cx, handle, OpenSavedTerminal(FIRST));
    assert_eq!(local.launches().last().unwrap().replay, None);
    wait_for_output(cx, handle, &workspace, 1).await;
    dispatch(cx, handle, CloseSavedTerminal(FIRST));
    assert!(!outputs.exists());
}
