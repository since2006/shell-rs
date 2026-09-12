use gpui_kit::component::{Root, WindowExt as _, notification::Notification};
use gpui_kit::*;

use shellr::session::{SessionDatabase, SessionStore};

fn main() {
    gpui_kit::application()
        .with_assets(shellr::app::AppAssets)
        .run(|cx| {
            shellr::init(cx);
            cx.activate(true);

            let options = shellr::workspace::window_options(cx);
            cx.spawn(async move |cx| {
                cx.open_window(options, |window, cx| {
                    window.activate_window();
                    window.set_window_title("shellr");
                    let (store, problem) = open_store();
                    let store = cx.new(|_| store);
                    let workspace =
                        cx.new(|cx| shellr::workspace::Workspace::new(store, window, cx));
                    if let Some(problem) = problem {
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
    let secrets = shellr::secrets::system_secret_store();
    match load_store() {
        Ok(store) => (store.with_secrets(secrets), None),
        Err(error) => {
            eprintln!("shellr: 无法打开本地数据库：{error}");
            (
                SessionStore::empty().with_secrets(secrets),
                Some(format!("无法打开本地数据库，本次运行的改动不会被保存：{error}").into()),
            )
        }
    }
}

fn load_store() -> anyhow::Result<SessionStore> {
    let path = shellr::app::database_path()?;
    Ok(SessionStore::load(SessionDatabase::open(&path)?)?)
}
