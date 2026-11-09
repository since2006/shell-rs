// A release build on Windows is a GUI program, so opening it does not bring
// up a console window as well. Debug builds keep theirs for the logs.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use gpui_kit::component::{Root, notification::Notification};
use gpui_kit::*;

use shellrs::host::{HostDatabase, HostStore};
use shellrs::settings::{SettingsProblem, SettingsStore};

fn main() {
    let args = shellrs::cli::command_line_arguments().unwrap_or_default();
    // `ShellRS ssh://user@host:port` or `ShellRS -url … -newtab 名称`: a
    // bastion host opening ShellRS the way it opens Xshell. On every build,
    // a Windows release build above all, where bastion hosts run it.
    let link = shellrs::cli::link_arguments(&args);
    // `shellrs hosts list`, `shellrs exec …`: the command, not the app. Not in a
    // Windows release build, which has nowhere to print: there the command
    // is `shellrs-cli.exe`, which 设置 → 外部 CLI puts on the PATH.
    #[cfg(any(not(windows), debug_assertions))]
    if link.is_none() && !args.is_empty() {
        std::process::exit(shellrs::cli::main(args));
    }
    // One ShellRS per data directory. Each keeps the hosts in memory and
    // writes its changes through, so a second one would write over the
    // first one's; opened again, ShellRS brings the running one forward
    // and hands it the link.
    if shellrs::cli::activate_running_app(&shellrs::app::cli_socket_path(), link.clone()) {
        return;
    }
    let app = gpui_kit::application().with_assets(shellrs::app::AppAssets);
    // macOS: the Dock icon of a ShellRS whose window was closed, which only
    // hid it (see `hide_when_closed`).
    app.on_reopen(|cx| {
        if let Some(shellrs::app::MainWindow(window)) = cx.try_global().copied() {
            window
                .update(cx, |_, window, cx| shellrs::app::bring_forward(window, cx))
                .ok();
        }
    });
    app.run(|cx| {
        // Read before anything is built: the settings choose the language
        // every text is in, and 应用 › 窗口 where the window opens.
        let (settings, settings_problem) = open_settings();
        let locale = settings.settings().language.resolved();
        shellrs::i18n::set_locale(locale);
        // Defaults that become the user's own, the example highlight rules
        // a first start fills in, are made while reading: read again, now
        // in the language just chosen.
        let (settings, settings_problem) = if locale == shellrs::i18n::LOCALES[0] {
            (settings, settings_problem)
        } else {
            open_settings()
        };
        shellrs::init(cx);
        // 显示或隐藏 ShellRS takes its key from the system; tests never do.
        shellrs::app::listen_for_system_hotkey(cx);
        // ShellRS's menu and 窗口, whose items have ⌘H and ⌘M.
        #[cfg(target_os = "macos")]
        shellrs::app::show_menu_bar(cx);
        shellrs::app::show_logo_when_unbundled(cx);
        cx.activate(true);

        let saved = shellrs::workspace::WindowState::load(&shellrs::app::window_state_path());
        let options =
            shellrs::workspace::window_options(saved.as_ref(), settings.settings().window, cx);
        cx.spawn(async move |cx| {
            let window = cx
                .open_window(options, |window, cx| {
                    window.activate_window();
                    window.set_window_title("ShellRS");
                    #[cfg(windows)]
                    shellrs::app::listen_for_tray(window, cx);
                    shellrs::app::hide_when_closed(window, cx);
                    // Drawn by the processor (a virtual machine without 3D),
                    // ShellRS draws fewer frames.
                    shellrs::app::note_renderer(window, cx);
                    let lock = cx.new(|cx| {
                        shellrs::app_lock::AppLock::new(
                            std::sync::Arc::new(shellrs::app_lock::KeychainLockStore),
                            cx,
                        )
                    });
                    shellrs::app_lock::AppLock::install(lock.clone(), cx);
                    shellrs::settings::apply(settings.settings(), window, cx);
                    let server =
                        match shellrs::cli::CliServer::reserve(shellrs::app::cli_socket_path()) {
                            Ok(server) => server,
                            Err(error) => {
                                let failure =
                                    cx.new(|_| shellrs::workspace::StartupFailure::new(&error));
                                return cx.new(|cx| Root::new(failure, window, cx));
                            }
                        };
                    let background_cli = settings.settings().is_external_cli_allowed(true);
                    let shell = cx.new(|cx| {
                        shellrs::workspace::AppShell::new(
                            lock,
                            Some(server),
                            background_cli,
                            move |server, pending_links, window, cx| {
                                let (store, problem) = open_store();
                                let store = cx.new(|_| store);
                                let settings = cx.new(|_| settings);
                                let workspace = cx.new(|cx| {
                                    shellrs::workspace::Workspace::new_with_cli_server(
                                        store, settings, server, window, cx,
                                    )
                                });
                                let settings_problem =
                                    settings_problem.map(|problem| problem.message());
                                for problem in [problem, settings_problem].into_iter().flatten() {
                                    shellrs::workspace::notify_once_open(
                                        Notification::error(problem),
                                        window,
                                        cx,
                                    );
                                }
                                for link in link.into_iter().chain(pending_links) {
                                    shellrs::workspace::open_link_once_open(
                                        &workspace, link, window, cx,
                                    );
                                }
                                workspace.into()
                            },
                            window,
                            cx,
                        )
                    });
                    cx.new(|cx| Root::new(shell, window, cx))
                })
                .expect("failed to open window");
            cx.update(|cx| cx.set_global(shellrs::app::MainWindow(window.into())));
        })
        .detach();
    });
    // On Windows the event loop ends and `run` returns: an update that is
    // to be installed now starts its setup, which waits for this process.
    shellrs::update::start_handed_over();
}

/// Load the hosts saved on disk. If the database cannot be opened the
/// window still comes up, on a store that forgets everything when it closes;
/// the returned message says so.
///
/// Either way the store gets the system keychain, which is where passwords
/// live. A machine without one still runs; the host form says so and
/// disables its password field. Pasted and generated private keys go
/// beside the database; the credentials follow them when the data folder
/// has moved.
fn open_store() -> (HostStore, Option<SharedString>) {
    let secrets = shellrs::secrets::system_secret_store();
    match load_store() {
        // Keys are kept only with a database to remember them: a store
        // that forgets everything would leave them behind unused.
        Ok(store) => {
            let mut store = store
                .with_secrets(secrets)
                .with_key_dir(shellrs::app::keys_dir())
                .with_terminal_output_dir(shellrs::app::terminal_output_dir());
            let problem = store.adopt_moved_keys();
            (store, problem)
        }
        Err(error) => {
            eprintln!("shellrs: cannot open the local database: {error}");
            (
                HostStore::empty().with_secrets(secrets),
                Some(shellrs::app::database_unavailable(&error)),
            )
        }
    }
}

/// Load the settings file. Without a data directory the settings still
/// work, only for this run; the returned problem says so.
fn open_settings() -> (SettingsStore, Option<SettingsProblem>) {
    match shellrs::app::settings_path() {
        Ok(path) => SettingsStore::load(path),
        Err(error) => (
            SettingsStore::in_memory(),
            Some(SettingsProblem::Unsaved(error.to_string())),
        ),
    }
}

fn load_store() -> anyhow::Result<HostStore> {
    let path = shellrs::app::database_path()?;
    Ok(HostStore::load(HostDatabase::open(&path)?)?)
}
