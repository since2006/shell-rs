//! Application-level wiring: actions, key bindings, assets.

mod actions;
mod app_icon;
mod assets;
mod paths;
mod quit;
mod shortcuts;
mod window_hiding;

pub use actions::*;
pub use app_icon::show_logo_when_unbundled;
pub use assets::{AppAssets, CatalogIcon, DOCKER_ICON};
pub use paths::{
    cli_endpoint, cli_socket_path, data_dir, database_path, keys_dir, settings_path, updates_dir,
    window_state_path,
};
pub use quit::{quit_held_back, set_quit_guard};
pub use shortcuts::{
    Refusal, SHORTCUTS, Shortcut, ShortcutGroup, ShortcutOverrides, apply_shortcuts, check,
    fixed_bindings, key_caps, key_text,
};
pub use window_hiding::{bring_forward, hide_when_closed};

use gpui_kit::component::Theme;
use gpui_kit::*;

/// Key context of the host panel, for bindings that only apply there.
pub const HOST_PANEL_CONTEXT: &str = "HostPanel";
pub const RECENT_HOSTS_CONTEXT: &str = "RecentHosts";
/// Key context of the port-forwarding list.
pub const FORWARD_PANEL_CONTEXT: &str = "ForwardPanel";
/// Key context of the credential list.
pub const CREDENTIAL_PANEL_CONTEXT: &str = "CredentialPanel";
/// Key contexts of the two SFTP file lists.
/// Key context of an editor tab.
pub const EDITOR_CONTEXT: &str = "FileEditor";
/// Key context of the image in a preview.
pub const IMAGE_PREVIEW_CONTEXT: &str = "ImagePreview";
pub const LOCAL_FILE_LIST_CONTEXT: &str = "LocalFileList";
pub const REMOTE_FILE_LIST_CONTEXT: &str = "RemoteFileList";

/// Initialize GPUI Kit and everything global to the application.
pub fn init(cx: &mut App) {
    gpui_kit::init(cx);
    // Before ours: rebuilding the keymap starts from gpui-kit's own.
    shortcuts::keep_kit_bindings(cx);
    deepen_list_hover(cx);
    gpui_kit::component::set_locale("zh-CN");
    // The defaults; the settings bring the user's changes once read.
    apply_shortcuts(&ShortcutOverrides::default(), cx);
    cx.on_action(|_: &Quit, cx: &mut App| quit::quit(cx));
}

/// Use a stronger version of the theme's neutral list hover color throughout
/// the app.
pub(crate) fn deepen_list_hover(cx: &mut App) {
    let theme = Theme::global_mut(cx);
    let hover = theme.list_hover;
    let deeper = hover.alpha(((hover.a + 0.2) * 1.2).min(1.0));
    theme.list_hover = deeper;
    theme.tokens.list_hover = deeper.into();
    Theme::sync_base(cx);
}

/// Give the host tree three times the list hover's contrast against its sidebar.
/// The recent-host list keeps the shared hover color unchanged.
pub(crate) fn host_tree_selection_color(theme: &Theme) -> Hsla {
    let sidebar = theme.sidebar;
    let hover = sidebar.blend(theme.list_hover);
    let mut selected = hover.alpha(1.0);
    selected.l = (sidebar.l + (hover.l - sidebar.l) * 3.0).clamp(0.0, 1.0);
    selected
}
