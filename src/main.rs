use gpui_kit::component::{Root, WindowExt as _, notification::Notification};
use gpui_kit::*;

use shellrs::session::{SessionDatabase, SessionStore};
use shellrs::settings::SettingsStore;

fn main() {
    gpui_kit::application()
        .with_assets(shellrs::app::AppAssets)
        .run(|cx| {
            shellrs::init(cx);
            cx.activate(true);

            let options = shellrs::workspace::window_options(cx);
            cx.spawn(async move |cx| {
                cx.open_window(options, |window, cx| {
                    window.activate_window();
                    window.set_window_title("ShellRS");
                    let (store, problem) = open_store();
                    let store = cx.new(|_| store);
                    let (settings, settings_problem) = open_settings();
                    let settings = cx.new(|_| settings);
                    let workspace = cx
                        .new(|cx| shellrs::workspace::Workspace::new(store, settings, window, cx));
                    for problem in [problem, settings_problem].into_iter().flatten() {
                        window.push_notification(Notification::error(problem), cx);
                    }
                    cx.new(|cx| Root::new(workspace, window, cx))
                })
                .expect("failed to open window");
            })
            .detach();
        });
}

/// Load the sessions saved on disk. If the database cannot be opened the
/// window still comes up, on a store that forgets everything when it closes;
/// the returned message says so.
///
/// Either way the store gets the system keychain, which is where passwords
/// live. A machine without one still runs; the session form says so and
/// disables its password field.
fn open_store() -> (SessionStore, Option<SharedString>) {
    let secrets = shellrs::secrets::system_secret_store();
    match load_store() {
        Ok(store) => (store.with_secrets(secrets), None),
        Err(error) => {
            eprintln!("shellrs: 无法打开本地数据库：{error}");
            (
                SessionStore::empty().with_secrets(secrets),
                Some(format!("无法打开本地数据库，本次运行的改动不会被保存：{error}").into()),
            )
        }
    }
}

/// Load the settings file. Without a data directory the settings still
/// work, only for this run; the returned message says so.
fn open_settings() -> (SettingsStore, Option<SharedString>) {
    match shellrs::app::settings_path() {
        Ok(path) => SettingsStore::load(path),
        Err(error) => (
            SettingsStore::in_memory(),
            Some(format!("无法保存设置，本次运行的改动不会被保存：{error}").into()),
        ),
    }
}

fn load_store() -> anyhow::Result<SessionStore> {
    let path = shellrs::app::database_path()?;
    Ok(SessionStore::load(SessionDatabase::open(&path)?)?)
}
