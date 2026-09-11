use gpui_kit::component::Root;
use gpui_kit::*;

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
                    let workspace = cx.new(|cx| shellr::workspace::Workspace::new(window, cx));
                    cx.new(|cx| Root::new(workspace, window, cx))
                })
                .expect("failed to open window");
            })
            .detach();
        });
}
