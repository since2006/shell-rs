//! Exercise the real startup shell, settings and password gate with isolated
//! keychain records and injected workspace transports.
use crate::support::*;
use gpui_kit::AnyView;
use shellrs::app_lock::{AppLock, InMemoryLockStore, LockRecordStore};
use shellrs::workspace::AppShell;
use std::cell::Cell;
use std::rc::Rc;

fn open_locked_workspace(
    cx: &mut TestAppContext,
    records: Arc<dyn LockRecordStore>,
) -> (WindowHandle<Root>, Entity<AppLock>, Rc<Cell<usize>>) {
    let (handle, lock, created, _) =
        open_locked_workspace_with_settings(cx, records, settings_store());
    (handle, lock, created)
}

fn open_locked_workspace_with_settings(
    cx: &mut TestAppContext,
    records: Arc<dyn LockRecordStore>,
    settings: SettingsStore,
) -> (
    WindowHandle<Root>,
    Entity<AppLock>,
    Rc<Cell<usize>>,
    Entity<SettingsStore>,
) {
    if cx.update(|cx| AppLock::global(cx).is_none()) {
        init_app(cx);
    }
    cx.update(|cx| cx.set_reduce_motion(true));
    let lock = cx.update(|cx| {
        let lock = cx.new(|cx| AppLock::new(records, cx));
        AppLock::install(lock.clone(), cx);
        lock
    });
    let background_cli = settings.settings().is_external_cli_allowed(true);
    let settings = cx.update(|cx| cx.new(|_| settings));
    let initial_settings = settings.clone();
    let created = Rc::new(Cell::new(0));
    let count = created.clone();
    let gate = lock.clone();
    let handle = cx.open_window(size(px(1100.), px(760.)), move |window, cx| {
        let shell = cx.new(|cx| {
            AppShell::new(
                gate,
                None,
                background_cli,
                move |_, _, window, cx| {
                    count.set(count.get() + 1);
                    let store = cx.new(|_| HostStore::empty());
                    let settings = initial_settings;
                    let workspace = cx.new(|cx| {
                        Workspace::new_with_services(
                            store,
                            settings,
                            Arc::new(FixedRemoteTerminalTransportProvider::new(Arc::new(
                                FakeTerminalFactory::default(),
                            ))),
                            fake_telnet_terminals(),
                            fake_serial_terminals(),
                            fake_local_terminals(),
                            Arc::new(FakeSftpProvider::default()),
                            Arc::new(FakeLocalDirectory::default()),
                            Arc::new(FakeConnectionTester::default()),
                            Arc::new(FakeForwardProvider::default()),
                            window,
                            cx,
                        )
                    });
                    AnyView::from(workspace)
                },
                window,
                cx,
            )
        });
        Root::new(shell, window, cx)
    });
    cx.run_until_parked();
    (handle, lock, created, settings)
}

fn app_settings(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(OpenSettings), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-7", cx)
    });
}

fn enter(window: &mut gpui_kit::Window, id: &'static str, text: &str, cx: &mut App) {
    window.click(id, cx);
    window.input(text, cx);
}

fn wait_for_password(cx: &mut TestAppContext, lock: &Entity<AppLock>) {
    // Real Argon2 runs on the background executor. Drain until its UI result
    // has been handled, without real servers, timers or the system keychain.
    cx.run_until_parked();
    cx.update(|cx| assert!(!lock.read(cx).is_busy()));
}

#[gpui_kit::test]
fn app_lock_settings_and_startup_gate(cx: &mut TestAppContext) {
    let records = Arc::new(InMemoryLockStore::default());
    let (handle, lock, created) = open_locked_workspace(cx, records.clone());
    assert_eq!(created.get(), 1);
    app_settings(cx, handle);
    in_frame(cx, handle, |window, cx| window.click("app-lock-enable", cx));
    in_frame(cx, handle, |window, cx| {
        enter(window, "app-lock-new-password", "first password", cx);
        enter(window, "app-lock-repeat-password", "different password", cx);
        window.click("app-lock-commit", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("app-lock-error").label(),
            Some("两次输入的密码不一致。")
        );
    });
    in_frame(cx, handle, |window, cx| {
        window.click("app-lock-repeat-password", cx);
        window.press("secondary-a", cx);
        window.input("first password", cx);
        window.click("app-lock-commit", cx);
    });
    wait_for_password(cx, &lock);
    assert!(records.read().unwrap().is_some());
    // Closing a dialog loses focus; clicking the settings field restores it.
    in_frame(cx, handle, |window, cx| window.click("app-lock-now", cx));
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("app-lock-screen").visible());
        assert!(window.try_find("settings").is_none());
        window.dispatch_action(Box::new(OpenSettings), cx);
        window.press("escape", cx);
        assert!(window.find("app-lock-screen").visible());
        enter(window, "app-lock-password", "incorrect", cx);
        window.press("enter", cx);
    });
    wait_for_password(cx, &lock);
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("app-lock-error").visible());
        assert!(window.try_find("settings").is_none());
        enter(window, "app-lock-password", "first password", cx);
        window.press("enter", cx);
    });
    wait_for_password(cx, &lock);
    in_frame(cx, handle, |window, _| {
        assert!(window.find("settings").visible())
    });
    assert_eq!(created.get(), 1, "manual unlock preserves the workspace");

    in_frame(cx, handle, |window, cx| window.click("app-lock-change", cx));
    in_frame(cx, handle, |window, cx| {
        enter(window, "app-lock-password", "first password", cx);
        enter(window, "app-lock-new-password", "second password", cx);
        enter(window, "app-lock-repeat-password", "second password", cx);
        assert_eq!(
            window.find("app-lock-new-password").bounds().left(),
            window.find("app-lock-repeat-password").bounds().left()
        );
        window.click("app-lock-commit", cx);
    });
    wait_for_password(cx, &lock);

    // A fresh launch has no workspace until the password is accepted.
    let (reopened, fresh_lock, fresh_created) = open_locked_workspace(cx, records.clone());
    cx.update(|_| shellrs::i18n::set_locale("en"));
    assert_eq!(fresh_created.get(), 0);
    in_frame(cx, reopened, |window, cx| {
        assert!(window.find("app-lock-screen").visible());
        assert!(window.try_find("host-search").is_none());
        assert_eq!(window.find("app-lock-password").focused(), Some(true));
        enter(window, "app-lock-password", "first password", cx);
        window.click("app-lock-unlock", cx);
    });
    wait_for_password(cx, &fresh_lock);
    assert_eq!(fresh_created.get(), 0);
    in_frame(cx, reopened, |window, cx| {
        assert_eq!(
            window.find("app-lock-error").label(),
            Some("The app lock password is incorrect. Try again.")
        );
        enter(window, "app-lock-password", "second password", cx);
        window.click("app-lock-unlock", cx);
    });
    wait_for_password(cx, &fresh_lock);
    assert_eq!(fresh_created.get(), 1);
    app_settings(cx, reopened);
    in_frame(cx, reopened, |window, cx| {
        window.click("app-lock-disable", cx)
    });
    in_frame(cx, reopened, |window, cx| {
        enter(window, "app-lock-password", "second password", cx);
        window.click("app-lock-commit", cx);
    });
    wait_for_password(cx, &fresh_lock);
    cx.update(|cx| assert!(!fresh_lock.read(cx).is_enabled()));
}

#[gpui_kit::test]
fn a_broken_keychain_record_never_opens_the_workspace(cx: &mut TestAppContext) {
    let records = Arc::new(InMemoryLockStore::default());
    records.write("broken").unwrap();
    let (handle, _, created) = open_locked_workspace(cx, records);
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("app-lock-load-error").visible());
        assert!(window.try_find("app-lock-unlock").is_none());
        window.press("escape", cx);
        window.dispatch_action(Box::new(OpenSettings), cx);
        assert!(window.find("app-lock-screen").visible());
    });
    assert_eq!(created.get(), 0);
}

#[gpui_kit::test]
fn app_lock_idle_timeout_tracks_input_and_restarts_after_unlock(cx: &mut TestAppContext) {
    use gpui_kit::ScrollDelta;
    use std::time::Duration;

    let (handle, lock, _) = open_locked_workspace(cx, Arc::new(InMemoryLockStore::default()));
    app_settings(cx, handle);
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .within("settings")
                .within("group-1")
                .try_find("increment")
                .is_none()
        );
    });
    in_frame(cx, handle, |window, cx| window.click("app-lock-enable", cx));
    in_frame(cx, handle, |window, cx| {
        enter(window, "app-lock-new-password", "idle password", cx);
        enter(window, "app-lock-repeat-password", "idle password", cx);
        window.click("app-lock-commit", cx);
    });
    wait_for_password(cx, &lock);
    let advance = |seconds, cx: &mut TestAppContext| {
        cx.executor().advance_clock(Duration::from_secs(seconds));
        cx.run_until_parked();
    };
    let locked = |cx: &mut TestAppContext| cx.update(|cx| lock.read(cx).is_locked());
    // The default is zero: even a long idle period does not lock.
    advance(120, cx);
    assert!(!locked(cx));
    in_frame(cx, handle, |window, cx| {
        window
            .within("settings")
            .within("group-1")
            .within("item-1")
            .click("increment", cx);
    });
    advance(50, cx);
    assert!(!locked(cx));
    in_frame(cx, handle, |window, cx| window.press("tab", cx));
    advance(50, cx);
    assert!(!locked(cx), "keyboard input resets the timeout");
    in_frame(cx, handle, |window, cx| window.hover("app-lock-now", cx));
    advance(50, cx);
    assert!(!locked(cx), "pointer movement resets the timeout");
    in_frame(cx, handle, |window, cx| {
        window.scroll(
            "app-lock-now",
            ScrollDelta::Pixels(point(px(0.), px(-10.))),
            cx,
        )
    });
    advance(50, cx);
    assert!(!locked(cx), "scrolling resets the timeout");
    in_frame(cx, handle, |window, cx| window.click("app-lock-change", cx));
    advance(50, cx);
    // A rendered frame or a notification is not user input.
    in_frame(cx, handle, |window, cx| {
        window.push_notification(Notification::info("background update"), cx)
    });
    advance(11, cx);
    assert!(locked(cx));
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("app-lock-screen").visible());
        assert!(window.try_find("settings").is_none());
        enter(window, "app-lock-password", "idle password", cx);
        window.click("app-lock-unlock", cx);
    });
    wait_for_password(cx, &lock);
    in_frame(cx, handle, |window, _| {
        assert!(window.find("app-lock-new-password").visible())
    });
    advance(59, cx);
    assert!(!locked(cx), "unlock starts a full new interval");
    advance(2, cx);
    assert!(locked(cx));
    in_frame(cx, handle, |window, cx| {
        enter(window, "app-lock-password", "idle password", cx);
        window.click("app-lock-unlock", cx);
    });
    wait_for_password(cx, &lock);
    in_frame(cx, handle, |window, cx| window.press("escape", cx));
    in_frame(cx, handle, |window, cx| {
        window
            .within("settings")
            .within("group-1")
            .within("item-1")
            .click("decrement", cx);
    });
    advance(120, cx);
    assert!(!locked(cx), "setting zero disables the timeout again");
}

#[gpui_kit::test]
fn app_lock_allows_background_cli_only_when_explicitly_enabled(cx: &mut TestAppContext) {
    let records = Arc::new(InMemoryLockStore::default());
    let mut configured = settings_store();
    configured.update_unnotified(|settings| settings.external_cli.enabled = true);
    let (handle, lock, _, settings) =
        open_locked_workspace_with_settings(cx, records.clone(), configured);
    app_settings(cx, handle);
    in_frame(cx, handle, |window, cx| window.click("app-lock-enable", cx));
    in_frame(cx, handle, |window, cx| {
        enter(window, "app-lock-new-password", "background password", cx);
        enter(
            window,
            "app-lock-repeat-password",
            "background password",
            cx,
        );
        window.click("app-lock-commit", cx);
    });
    wait_for_password(cx, &lock);
    in_frame(cx, handle, |window, cx| {
        let mut row = window.within("settings");
        let mut group = row.within("group-1");
        assert_eq!(group.within("item-2").find("check").checked(), Some(false));
        group.within("item-2").click("check", cx);
    });
    cx.update(|cx| assert!(settings.read(cx).settings().app_lock_allow_cli));
    in_frame(cx, handle, |window, cx| window.click("app-lock-now", cx));
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("app-lock-screen").visible());
        window.dispatch_action(Box::new(OpenSettings), cx);
        assert!(window.try_find("settings").is_none());
    });
    cx.update(|cx| {
        assert!(lock.read(cx).is_locked());
        assert!(settings.read(cx).settings().is_external_cli_allowed(true));
    });
    in_frame(cx, handle, |window, cx| {
        enter(window, "app-lock-password", "background password", cx);
        window.click("app-lock-unlock", cx);
    });
    wait_for_password(cx, &lock);
    in_frame(cx, handle, |window, cx| {
        window
            .within("settings")
            .within("group-1")
            .within("item-2")
            .click("check", cx)
    });
    in_frame(cx, handle, |window, cx| window.click("app-lock-now", cx));
    cx.update(|cx| {
        assert!(lock.read(cx).is_locked());
        assert!(!settings.read(cx).settings().is_external_cli_allowed(true));
    });

    // With both permissions saved, startup can prepare the background
    // workspace while keeping its entire interface behind the password gate.
    let configured = || {
        let mut settings = settings_store();
        settings.update_unnotified(|settings| {
            settings.external_cli.enabled = true;
            settings.app_lock_allow_cli = true;
        });
        settings
    };
    let (restarted, restart_lock, created, _) =
        open_locked_workspace_with_settings(cx, records.clone(), configured());
    assert_eq!(created.get(), 1);
    cx.update(|cx| assert!(restart_lock.read(cx).is_locked()));
    in_frame(cx, restarted, |window, cx| {
        assert!(window.find("app-lock-screen").visible());
        assert!(window.try_find("host-search").is_none());
        assert_eq!(window.find("app-lock-password").focused(), Some(true));
        window.dispatch_action(Box::new(OpenSettings), cx);
        assert!(window.try_find("settings").is_none());
    });
    in_frame(cx, restarted, |window, cx| {
        enter(window, "app-lock-password", "background password", cx);
        window.click("app-lock-unlock", cx);
    });
    wait_for_password(cx, &restart_lock);
    app_settings(cx, restarted);
    in_frame(cx, restarted, |window, _| {
        assert!(window.find("settings").visible())
    });
    let mut cli_off = configured();
    cli_off.update_unnotified(|settings| settings.external_cli.enabled = false);
    let (_, _, created, _) = open_locked_workspace_with_settings(cx, records, cli_off);
    assert_eq!(
        created.get(),
        0,
        "the global CLI switch still applies at startup"
    );

    let corrupt = Arc::new(InMemoryLockStore::default());
    corrupt.write("broken").unwrap();
    let (_, _, created, _) = open_locked_workspace_with_settings(cx, corrupt, configured());
    assert_eq!(
        created.get(),
        0,
        "an unreadable lock never authorizes background startup"
    );
}

#[derive(Default)]
struct UnavailableKeychain {
    denied: std::sync::atomic::AtomicBool,
    memory: InMemoryLockStore,
}

impl LockRecordStore for UnavailableKeychain {
    fn read(&self) -> anyhow::Result<Option<zeroize::Zeroizing<String>>> {
        anyhow::ensure!(
            !self.denied.load(std::sync::atomic::Ordering::SeqCst),
            "keychain access denied"
        );
        self.memory.read()
    }
    fn write(&self, record: &str) -> anyhow::Result<()> {
        self.memory.write(record)
    }
}

#[gpui_kit::test]
fn app_lock_keychain_failure_blocks_background_startup_and_retry_recovers(cx: &mut TestAppContext) {
    let mut settings = settings_store();
    settings.update_unnotified(|settings| {
        settings.external_cli.enabled = true;
        settings.app_lock_allow_cli = true;
    });
    let records = Arc::new(UnavailableKeychain::default());
    let (prepared, prepared_lock, _, _) =
        open_locked_workspace_with_settings(cx, records.clone(), settings_store());
    app_settings(cx, prepared);
    in_frame(cx, prepared, |window, cx| {
        window.click("app-lock-enable", cx)
    });
    in_frame(cx, prepared, |window, cx| {
        enter(window, "app-lock-new-password", "retry password", cx);
        enter(window, "app-lock-repeat-password", "retry password", cx);
        window.click("app-lock-commit", cx);
    });
    wait_for_password(cx, &prepared_lock);
    records
        .denied
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (handle, lock, created, _) =
        open_locked_workspace_with_settings(cx, records.clone(), settings);
    assert_eq!(created.get(), 0);
    cx.update(|cx| {
        assert!(lock.read(cx).is_locked());
        assert!(!lock.read(cx).is_ready());
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.find("app-lock-load-error").visible());
        assert!(window.find("app-lock-retry").visible());
        assert!(window.try_find("app-lock-unlock").is_none());
        assert!(window.try_find("host-search").is_none());
    });
    records
        .denied
        .store(false, std::sync::atomic::Ordering::SeqCst);
    in_frame(cx, handle, |window, cx| window.click("app-lock-retry", cx));
    assert_eq!(created.get(), 1);
    cx.update(|cx| {
        assert!(lock.read(cx).is_ready());
        assert!(lock.read(cx).is_locked());
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("app-lock-load-error").is_none());
        enter(window, "app-lock-password", "retry password", cx);
        window.click("app-lock-unlock", cx);
    });
    wait_for_password(cx, &lock);
    in_frame(cx, handle, |window, _| {
        assert!(window.find("host-search").visible());
        assert!(window.try_find("app-lock-screen").is_none());
    });
}
