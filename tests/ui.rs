//! UI integration tests: the production `Workspace` rendered in a headless
//! window, driven through real pointer and keyboard events.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use gpui_kit::component::{ActiveTheme as _, Root};
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use gpui_kit::{
    AppContext as _, ClipboardItem, ElementId, Entity, InputEvent as _, MouseButton,
    MouseMoveEvent, TestAppContext, WindowHandle, point, px, size,
};

use shellr::app::{DeleteGroup, NewSessionInGroup, RenameGroup};
use shellr::session::{GroupId, SessionDatabase, SessionId, SessionStore};
use shellr::terminal::{
    LocalTerminalId, TerminalLifecycle, TerminalSize, TerminalTransport, TerminalTransportCommand,
    TerminalTransportEvent, TerminalTransportFactory,
};
use shellr::workspace::Workspace;

/// Seeded session ids, in insertion order (see `SessionStore::seed`).
const WEB_01: u64 = 1;
const DB_01: u64 = 3;
const STAGING_API: u64 = 4;
/// Seeded group ids, in insertion order: 生产, 测试, 开发.
const PRODUCTION: u64 = 1;
const DEVELOPMENT: u64 = 3;

fn open_workspace(cx: &mut TestAppContext) -> (WindowHandle<Root>, Entity<Workspace>) {
    open_workspace_with_store(cx, SessionStore::seed())
}

/// Production loads the store from the database; the tests hand one in
/// directly so they get the fixed shape `SessionStore::seed` describes.
fn open_workspace_with_store(
    cx: &mut TestAppContext,
    store: SessionStore,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    cx.update(shellr::init);
    let mut workspace = None;
    let handle = cx.open_window(size(px(1280.), px(800.)), |window, cx| {
        let store = cx.new(|_| store);
        let view = cx.new(|cx| Workspace::new(store, window, cx));
        workspace = Some(view.clone());
        Root::new(view, window, cx)
    });
    (handle, workspace.expect("workspace created"))
}

#[derive(Clone, Copy, Default)]
enum FakeBehavior {
    #[default]
    Running,
    ExitFirst,
    FailFirst,
}

#[derive(Default)]
struct FakeTerminalFactory {
    starts: AtomicUsize,
    behavior: FakeBehavior,
    writes: Arc<Mutex<Vec<Vec<u8>>>>,
    resizes: Arc<Mutex<Vec<TerminalSize>>>,
}

impl FakeTerminalFactory {
    fn exit_first() -> Self {
        Self {
            behavior: FakeBehavior::ExitFirst,
            ..Self::default()
        }
    }

    fn fail_first() -> Self {
        Self {
            behavior: FakeBehavior::FailFirst,
            ..Self::default()
        }
    }

    fn written_text(&self) -> String {
        let bytes: Vec<_> = self
            .writes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .flatten()
            .copied()
            .collect();
        String::from_utf8(bytes).expect("fake transport receives UTF-8 test input")
    }
}

impl TerminalTransportFactory for FakeTerminalFactory {
    fn create(&self) -> Box<dyn TerminalTransport> {
        let run = self.starts.fetch_add(1, Ordering::SeqCst) + 1;
        Box::new(FakeTerminalTransport {
            run,
            behavior: self.behavior,
            writes: self.writes.clone(),
            resizes: self.resizes.clone(),
        })
    }
}

struct FakeTerminalTransport {
    run: usize,
    behavior: FakeBehavior,
    writes: Arc<Mutex<Vec<Vec<u8>>>>,
    resizes: Arc<Mutex<Vec<TerminalSize>>>,
}

impl TerminalTransport for FakeTerminalTransport {
    fn run(
        self: Box<Self>,
        _: TerminalSize,
        commands: mpsc::Receiver<TerminalTransportCommand>,
        events: async_channel::Sender<TerminalTransportEvent>,
    ) -> anyhow::Result<()> {
        if matches!(self.behavior, FakeBehavior::FailFirst) && self.run == 1 {
            anyhow::bail!("测试启动失败");
        }
        events.send_blocking(TerminalTransportEvent::Started)?;
        events.send_blocking(TerminalTransportEvent::Output(
            format!(
                "\x1b]0;测试终端 {}\x07run:{}$ alpha.txt 会议纪要.md\r\n",
                self.run, self.run
            )
            .into_bytes(),
        ))?;
        if matches!(self.behavior, FakeBehavior::ExitFirst) && self.run == 1 {
            events.send_blocking(TerminalTransportEvent::Exited {
                code: 9,
                signal: None,
            })?;
            return Ok(());
        }
        while let Ok(command) = commands.recv() {
            match command {
                TerminalTransportCommand::Write(bytes) => {
                    self.writes
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .push(bytes.clone());
                    events.send_blocking(TerminalTransportEvent::Output(bytes))?;
                }
                TerminalTransportCommand::Resize(size) => {
                    self.resizes
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .push(size);
                }
                TerminalTransportCommand::Shutdown => break,
            }
        }
        Ok(())
    }
}

fn open_workspace_with_factory(
    cx: &mut TestAppContext,
    factory: Arc<FakeTerminalFactory>,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    cx.update(shellr::init);
    let mut workspace = None;
    let handle = cx.open_window(size(px(1280.), px(800.)), |window, cx| {
        let store = cx.new(|_| SessionStore::seed());
        let view = cx.new(|cx| {
            Workspace::new_with_local_terminal_factory(store, factory.clone(), window, cx)
        });
        workspace = Some(view.clone());
        Root::new(view, window, cx)
    });
    (handle, workspace.expect("workspace created"))
}

#[gpui_kit::test]
fn double_click_on_session_opens_terminal_and_updates_status(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // Sessions seeded as connected already have terminal tabs.
        assert!(window.find(("terminal", WEB_01)).visible());
        assert!(window.try_find(("terminal", DB_01)).is_none());

        window
            .within("session-tree")
            .double_click(("session-row", DB_01), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("terminal", DB_01)).visible());
        assert_eq!(
            window.find("status-connection").label(),
            Some("已连接 db-01")
        );
    })
    .unwrap();

    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert!(workspace.terminal(SessionId(DB_01)).is_some());
        let store = workspace.store().read(cx);
        assert!(
            store
                .session(SessionId(DB_01))
                .unwrap()
                .state
                .is_connected()
        );
    });
}

#[gpui_kit::test]
async fn new_session_dialog_validates_then_inserts(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-session", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("commit").visible());
        assert!(window.try_find("form-error").is_none());

        // An empty form is rejected and the dialog stays open. The commit
        // action is dispatched deferred, so the error shows after effects run.
        // The dialog's focus trap owns focus until a field is clicked.
        window.click("session-name", cx);
        assert_eq!(window.find("session-name").focused(), Some(true));
        window.click("commit", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("form-error").visible());
        assert!(window.find("commit").visible());

        window.click("session-name", cx);
        window.input("db-02", cx);
        window.click("session-host", cx);
        window.input("10.0.3.7", cx);
        window.click("commit", cx);
    })
    .unwrap();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;

    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let created = store
            .sessions()
            .iter()
            .find(|session| session.name == "db-02")
            .expect("db-02 inserted");
        assert_eq!(created.host.as_ref(), "10.0.3.7");
        assert_eq!(created.port, 22);
    });
}

#[gpui_kit::test]
fn search_filters_the_tree(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window
                .within("session-tree")
                .try_find(("session-row", WEB_01))
                .is_some()
        );
        window.click("session-search", cx);
        window.input("staging", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let tree = window.within("session-tree");
        assert!(tree.try_find(("session-row", STAGING_API)).is_some());
        assert!(tree.try_find(("session-row", WEB_01)).is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn sftp_button_opens_explorer_and_navigates(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("session-tree")
            .double_click(("session-row", DB_01), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // The active terminal tab's toolbar shows the SFTP button.
        window.click("sftp", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("explorer", DB_01)).visible());
        assert_eq!(window.find("remote-path").value(), Some("~"));

        window
            .within(("remote-pane", DB_01))
            .double_click(ElementId::Name("file:..".into()), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("remote-path").value(), Some("/home"));
    })
    .unwrap();

    cx.update(|cx| {
        let workspace = workspace.read(cx);
        let explorer = workspace.explorer(SessionId(DB_01)).expect("explorer open");
        assert_eq!(explorer.read(cx).remote().read(cx).path(), "/home");
    });
}

#[gpui_kit::test]
fn theme_toggle_flips_mode(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);
    let before = cx.update(|cx| cx.theme().is_dark());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("theme-toggle", cx);
    })
    .unwrap();
    cx.run_until_parked();

    let after = cx.update(|cx| cx.theme().is_dark());
    assert_ne!(before, after);
}

#[gpui_kit::test]
fn tab_close_button_closes_the_terminal_and_disconnects(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("session-tree")
            .double_click(("session-row", DB_01), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("terminal", DB_01)).visible());
        window.click(("close-terminal", DB_01), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("terminal", DB_01)).is_none());
        assert!(window.try_find(("close-terminal", DB_01)).is_none());
        // The other seeded tabs are untouched.
        assert!(window.find(("close-terminal", WEB_01)).visible());
    })
    .unwrap();

    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert!(workspace.terminal(SessionId(DB_01)).is_none());
        let store = workspace.store().read(cx);
        assert!(
            !store
                .session(SessionId(DB_01))
                .unwrap()
                .state
                .is_connected()
        );
    });
}

#[gpui_kit::test]
fn closing_every_tab_shows_the_recent_sessions(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // While tabs are open the start page stays out of the way.
        assert!(window.try_find("recent-sessions").is_none());
        window.click(("close-terminal", WEB_01), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("terminal", WEB_01)).is_none());
        assert!(window.try_find("recent-sessions").is_none());
        // The last tab closes too (the tab group alone would refuse).
        window.click(("close-terminal", STAGING_API), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("terminal", STAGING_API)).is_none());
        assert!(window.find("recent-sessions").visible());
        // The page takes the focus the closed tab held.
        assert_eq!(window.find("recent-sessions").focused(), Some(true));
        // Both sessions had been connected, so both are listed.
        assert!(window.find(("recent-session", WEB_01)).visible());
        assert!(window.find(("recent-session", STAGING_API)).visible());
        assert_eq!(window.find("status-connection").label(), Some("未连接"));

        window.click(("recent-session", WEB_01), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("terminal", WEB_01)).visible());
        assert!(window.try_find("recent-sessions").is_none());
        assert_eq!(
            window.find("status-connection").label(),
            Some("已连接 web-01")
        );
    })
    .unwrap();

    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let recent: Vec<_> = store
            .recent_sessions()
            .map(|session| session.name.to_string())
            .collect();
        // Reconnecting moved web-01 to the front.
        assert_eq!(recent, ["web-01", "staging-api"]);
    });
}

#[gpui_kit::test]
fn close_shortcut_closes_the_displayed_tab_down_to_none(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("session-tree")
            .double_click(("session-row", DB_01), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // The new tab is displayed, with its input focused.
        assert!(window.find(("terminal", DB_01)).visible());
        window.press("cmd-w", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("terminal", DB_01)).is_none());
        assert!(window.try_find("recent-sessions").is_none());
        // Keep closing whichever tab the dock displays next.
        window.press("cmd-w", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.press("cmd-w", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("terminal", WEB_01)).is_none());
        assert!(window.try_find(("terminal", STAGING_API)).is_none());
        assert!(window.find("recent-sessions").visible());
        // With nothing open the shortcut does nothing.
        window.press("cmd-w", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("recent-sessions").visible());
    })
    .unwrap();
}

#[gpui_kit::test]
async fn creates_independent_local_terminals_from_button_and_shortcut(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_workspace_with_factory(cx, factory.clone());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-local-terminal", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window
            .try_find("status-connection")
            .map(|element| element.label() == Some("运行中 本地终端"))
            .unwrap_or(false)
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("local-terminal", 1_u64)).visible());
        assert_eq!(window.find(("local-terminal", 1_u64)).focused(), Some(true));
        #[cfg(target_os = "macos")]
        window.press("cmd-t", cx);
        #[cfg(not(target_os = "macos"))]
        window.press("ctrl-t", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window
            .try_find("status-connection")
            .map(|element| element.label() == Some("运行中 本地终端"))
            .unwrap_or(false)
            && window.try_find(("local-terminal", 2_u64)).is_some()
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("local-terminal", 2_u64)).visible());
        assert_eq!(
            window.find("status-connection").label(),
            Some("运行中 本地终端")
        );
        window.click(("close-local-terminal", 2_u64), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("local-terminal", 2_u64)).is_none());
        assert!(window.find(("local-terminal", 1_u64)).visible());
    })
    .unwrap();

    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert!(workspace.local_terminal(LocalTerminalId(1)).is_some());
        assert!(workspace.local_terminal(LocalTerminalId(2)).is_none());
        assert!(workspace.store().read(cx).active().is_none());
        assert_eq!(factory.starts.load(Ordering::SeqCst), 2);
    });
}

#[gpui_kit::test]
async fn tab_keys_are_sent_to_the_focused_terminal(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, _) = open_workspace_with_factory(cx, factory.clone());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-local-terminal", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window
            .try_find("status-connection")
            .is_some_and(|element| element.label() == Some("运行中 本地终端"))
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(("local-terminal", 1_u64)).focused(), Some(true));
        window.press("tab", cx);
        window.press("shift-tab", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        factory.written_text() == "\t\x1b[Z"
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(("local-terminal", 1_u64)).focused(), Some(true));
    })
    .unwrap();
}

#[gpui_kit::test]
async fn exited_local_terminal_keeps_its_tab_and_restarts_fresh(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::exit_first());
    let (handle, workspace) = open_workspace_with_factory(cx, factory.clone());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-local-terminal", cx);
    })
    .unwrap();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find(("local-terminal-exit", 1_u64)).is_some()
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("local-terminal", 1_u64)).visible());
        assert!(window.find(("local-terminal-exit", 1_u64)).visible());
        cx.write_to_clipboard(ClipboardItem::new_string("退出后不可粘贴".into()));
        #[cfg(target_os = "macos")]
        window.press("cmd-v", cx);
        #[cfg(not(target_os = "macos"))]
        window.press("ctrl-shift-v", cx);
        assert!(factory.written_text().is_empty());

        let bounds = window.find(("local-terminal", 1_u64)).bounds();
        window.drag(
            point(bounds.left() + px(1.), bounds.top() + px(8.)),
            point(bounds.right() - px(1.), bounds.top() + px(8.)),
            cx,
        );
        #[cfg(target_os = "macos")]
        window.press("cmd-c", cx);
        #[cfg(not(target_os = "macos"))]
        window.press("ctrl-shift-c", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("run:1$ alpha.txt 会议纪要.md".into())
        );
        window.click(("restart-local-terminal", 1_u64), cx);
    })
    .unwrap();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find(("local-terminal-exit", 1_u64)).is_none()
            && window
                .try_find("status-connection")
                .map(|element| element.label() == Some("运行中 本地终端"))
                .unwrap_or(false)
    })
    .await;

    cx.update(|cx| {
        let panel = workspace
            .read(cx)
            .local_terminal(LocalTerminalId(1))
            .unwrap()
            .clone();
        assert_eq!(
            panel.read(cx).status(cx).lifecycle(),
            &TerminalLifecycle::Running
        );
        assert_eq!(factory.starts.load(Ordering::SeqCst), 2);
    });
}

#[gpui_kit::test]
async fn local_terminal_start_failure_is_kept_with_a_chinese_error(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::fail_first());
    let (handle, workspace) = open_workspace_with_factory(cx, factory);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-local-terminal", cx);
    })
    .unwrap();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find(("local-terminal-exit", 1_u64)).is_some()
    })
    .await;

    cx.update(|cx| {
        let panel = workspace
            .read(cx)
            .local_terminal(LocalTerminalId(1))
            .unwrap()
            .clone();
        assert!(matches!(
            panel.read(cx).status(cx).lifecycle(),
            TerminalLifecycle::Failed(error) if error.contains("测试启动失败")
        ));
        assert!(
            panel
                .read(cx)
                .terminal()
                .read(cx)
                .screen_text(cx)
                .contains("终端错误：测试启动失败")
        );
    });
}

#[gpui_kit::test]
async fn terminal_selection_copies_only_on_command_and_finishes_outside_view(
    cx: &mut TestAppContext,
) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, _) = open_workspace_with_factory(cx, factory);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-local-terminal", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window
            .try_find("status-connection")
            .is_some_and(|element| element.label() == Some("运行中 本地终端"))
    })
    .await;

    cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string("原剪贴板".into())));
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.right_click(("local-terminal", 1_u64), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let mut popup = window.within("popup-menu");
        assert_eq!(popup.find(0_usize).label(), Some("复制"));
        popup.click(0_usize, cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("原剪贴板".into())
        );
        window.click("session-search", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let bounds = window.find(("local-terminal", 1_u64)).bounds();
        let from = point(bounds.left() + px(1.), bounds.top() + px(8.));
        let to = point(bounds.right() - px(1.), bounds.top() + px(8.));
        window.drag(from, to, cx);

        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("原剪贴板".into())
        );
        #[cfg(target_os = "macos")]
        window.press("cmd-c", cx);
        #[cfg(not(target_os = "macos"))]
        window.press("ctrl-shift-c", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("run:1$ alpha.txt 会议纪要.md".into())
        );

        window.right_click(("local-terminal", 1_u64), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let mut popup = window.within("popup-menu");
        assert_eq!(popup.find(0_usize).label(), Some("复制"));
        assert_eq!(popup.find(1_usize).label(), Some("粘贴"));
        popup.click(0_usize, cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let bounds = window.find(("local-terminal", 1_u64)).bounds();
        let from = point(bounds.left() + px(100.), bounds.top() + px(8.));
        let outside = point(px(20.), bounds.top() + px(8.));
        let to = point(bounds.right() - px(1.), bounds.top() + px(8.));
        window.drag(from, outside, cx);
        #[cfg(target_os = "macos")]
        window.press("cmd-c", cx);
        #[cfg(not(target_os = "macos"))]
        window.press("ctrl-shift-c", cx);
        let selection_after_release = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .expect("outside release leaves a valid selection");

        cx.write_to_clipboard(ClipboardItem::new_string("哨兵".into()));
        window.dispatch_event(
            MouseMoveEvent {
                position: to,
                pressed_button: Some(MouseButton::Left),
                modifiers: Default::default(),
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
        #[cfg(target_os = "macos")]
        window.press("cmd-c", cx);
        #[cfg(not(target_os = "macos"))]
        window.press("ctrl-shift-c", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(selection_after_release)
        );
        window.click(("close-local-terminal", 1_u64), cx);
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
async fn cjk_input_is_sent_once_and_resize_commands_are_deduplicated(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_workspace_with_factory(cx, factory.clone());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-local-terminal", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window
            .try_find("status-connection")
            .is_some_and(|element| element.label() == Some("运行中 本地终端"))
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.input("输入一次", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        factory.written_text() == "输入一次"
    })
    .await;

    cx.update(|cx| {
        let terminal = workspace
            .read(cx)
            .local_terminal(LocalTerminalId(1))
            .expect("local terminal exists")
            .read(cx)
            .terminal()
            .clone();
        let screen = terminal.read(cx).screen_text(cx);
        assert_eq!(screen.matches("输入一次").count(), 1);
    });

    let resizes = factory
        .resizes
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert!(!resizes.is_empty());
    assert!(resizes.windows(2).all(|sizes| sizes[0] != sizes[1]));
}

#[gpui_kit::test]
async fn new_group_from_the_toolbar_appears_in_the_tree(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-group", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("commit").visible());
        // An unnamed group is rejected and the dialog stays open.
        window.click("group-name", cx);
        window.click("commit", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("form-error").visible());
        window.click("group-name", cx);
        window.input("预发", cx);
        window.click("commit", cx);
    })
    .unwrap();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;

    let created = cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let created = store.groups().last().expect("group inserted");
        assert_eq!(created.name.as_ref(), "预发");
        assert_eq!(created.parent, None);
        created.id
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let tree = window.within("session-tree");
        assert!(tree.find(("group-row", created.0)).visible());
    })
    .unwrap();
}

/// The action a group row's 新建会话… menu entry dispatches.
#[gpui_kit::test]
async fn a_new_session_in_a_group_starts_out_in_that_group(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(NewSessionInGroup(GroupId(DEVELOPMENT))), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("session-name", cx);
        window.input("dev-02", cx);
        window.click("session-host", cx);
        window.input("192.168.1.21", cx);
        window.click("commit", cx);
    })
    .unwrap();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;

    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let created = store
            .sessions()
            .iter()
            .find(|session| session.name == "dev-02")
            .expect("dev-02 inserted");
        // The form opened with 开发 pre-selected and nothing changed it.
        assert_eq!(created.group, Some(GroupId(DEVELOPMENT)));
    });
}

#[gpui_kit::test]
async fn renaming_a_group_keeps_the_sessions_under_it(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(RenameGroup(GroupId(PRODUCTION))), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("commit").visible());
        window.click("group-name", cx);
        window.press("cmd-a", cx);
        window.input("生产环境", cx);
        window.click("commit", cx);
    })
    .unwrap();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;

    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let group = store.group(GroupId(PRODUCTION)).expect("group kept");
        assert_eq!(group.name.as_ref(), "生产环境");
        assert_eq!(store.group_path(GroupId(PRODUCTION)), "生产环境");
        // The three sessions still belong to it.
        assert_eq!(
            store
                .sessions()
                .iter()
                .filter(|s| s.group == Some(GroupId(PRODUCTION)))
                .count(),
            3
        );
    });
}

#[gpui_kit::test]
async fn deleting_a_group_removes_its_sessions_and_closes_their_tabs(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    // db-01 joins web-01, which starts connected, in having an open tab.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("session-tree")
            .double_click(("session-row", DB_01), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert!(workspace.terminal(SessionId(WEB_01)).is_some());
        assert!(workspace.terminal(SessionId(DB_01)).is_some());
    });

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(DeleteGroup(GroupId(PRODUCTION))), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("ok", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert!(workspace.terminal(SessionId(WEB_01)).is_none());
        assert!(workspace.terminal(SessionId(DB_01)).is_none());
        // staging-api is in another group and keeps its tab.
        assert!(workspace.terminal(SessionId(STAGING_API)).is_some());

        let store = workspace.store().read(cx);
        assert!(store.group(GroupId(PRODUCTION)).is_none());
        assert_eq!(store.groups().len(), 2);
        let names: Vec<_> = store
            .sessions()
            .iter()
            .map(|session| session.name.as_ref())
            .collect();
        assert_eq!(names, ["staging-api", "qa-runner", "dev-box"]);
    });
}

#[gpui_kit::test]
async fn groups_and_sessions_are_read_back_from_the_database(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("shellr.db");
    let store = SessionStore::load(SessionDatabase::open(&path).expect("database opened"))
        .expect("store loaded");
    // A first launch starts with nothing at all.
    assert_eq!(store.groups().len(), 0);
    assert_eq!(store.sessions().len(), 0);
    let (handle, workspace) = open_workspace_with_store(cx, store);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-group", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("group-name", cx);
        window.input("生产", cx);
        window.click("commit", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;

    let group = cx.update(|cx| workspace.read(cx).store().read(cx).groups()[0].id);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // A closed dialog leaves nothing focused, and an action only reaches
        // handlers on the focused element's path.
        window.click("session-search", cx);
        window.dispatch_action(Box::new(NewSessionInGroup(group)), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("session-name", cx);
        window.input("web-01", cx);
        window.click("session-host", cx);
        window.input("10.0.1.12", cx);
        window.click("commit", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;

    let session = cx.update(|cx| workspace.read(cx).store().read(cx).sessions()[0].id);

    // Connecting is what puts a session on the start page's recent list.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("session-tree")
            .double_click(("session-row", session.0), cx);
    })
    .unwrap();
    cx.run_until_parked();

    // Everything above went through the real write path; read it back with a
    // second connection to the same file.
    let reloaded = SessionStore::load(SessionDatabase::open(&path).expect("database reopened"))
        .expect("store reloaded");
    assert_eq!(reloaded.groups().len(), 1);
    assert_eq!(reloaded.groups()[0].name.as_ref(), "生产");
    assert_eq!(reloaded.sessions().len(), 1);
    let saved = &reloaded.sessions()[0];
    assert_eq!(saved.name.as_ref(), "web-01");
    assert_eq!(saved.host.as_ref(), "10.0.1.12");
    assert_eq!(saved.port, 22);
    assert_eq!(saved.group, Some(group));
    // Runtime state is not persisted, but the last connection time is.
    assert!(!saved.state.is_connected());
    assert_eq!(
        reloaded.recent_sessions().map(|s| s.id).collect::<Vec<_>>(),
        [session]
    );
}
