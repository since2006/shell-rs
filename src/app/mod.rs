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
    analytics_path, cli_endpoint, cli_socket_path, data_dir, database_path, keys_dir,
    settings_path, updates_dir, window_state_path,
};
pub use quit::{quit_held_back, set_quit_guard};
pub use shortcuts::{
    Refusal, SHORTCUTS, Shortcut, ShortcutGroup, ShortcutOverrides, apply_shortcuts, check,
    fixed_bindings, key_caps, key_text,
};
pub use window_hiding::{bring_forward, hide_when_closed};

use gpui_kit::component::Theme;
use gpui_kit::*;

use crate::i18n::t;

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

/// What the window says when the local database cannot be opened: the run
/// goes on, keeping its changes in memory only.
pub fn database_unavailable(error: &anyhow::Error) -> SharedString {
    t!("app.database_unavailable", error = error)
}

/// Initialize GPUI Kit and everything global to the application.
pub fn init(cx: &mut App) {
    gpui_kit::init(cx);
    // Before ours: rebuilding the keymap starts from gpui-kit's own.
    shortcuts::keep_kit_bindings(cx);
    deepen_list_hover(cx);
    // gpui-kit's own text in the language ShellRS's is in: the saved one,
    // which `main` set before anything was built.
    crate::i18n::set_locale(crate::i18n::locale());
    // The defaults; the settings bring the user's changes once read.
    apply_shortcuts(&ShortcutOverrides::default(), cx);
    cx.on_action(|_: &Quit, cx: &mut App| quit::quit(cx));
}

/// How much of a selected row's color hover gets in the dark appearance.
/// Selected rows there have the deepened hover color itself; hover with all
/// of it would look the same.
const DARK_HOVER_SHARE: f32 = 0.5;

/// Use a stronger version of the theme's neutral list hover color throughout
/// the app. Dark, the stronger one is for selected rows
/// (`selected_row_color`) and hover, in lists and tables, gets half of it.
pub(crate) fn deepen_list_hover(cx: &mut App) {
    let theme = Theme::global_mut(cx);
    let hover = theme.list_hover;
    let deeper = hover.alpha(((hover.a + 0.2) * 1.2).min(1.0));
    if theme.is_dark() {
        let faint = deeper.alpha(deeper.a * DARK_HOVER_SHARE);
        theme.list_hover = faint;
        theme.tokens.list_hover = faint.into();
        theme.table_hover = faint;
        theme.tokens.table_hover = faint.into();
    } else {
        theme.list_hover = deeper;
        theme.tokens.list_hover = deeper.into();
    }
    Theme::sync_base(cx);
}

/// The deepened list hover: light the hover itself, dark the full color
/// hover has only a share of.
fn deepened_hover(theme: &Theme) -> Hsla {
    let hover = theme.list_hover;
    if theme.is_dark() {
        hover.alpha((hover.a / DARK_HOVER_SHARE).min(1.0))
    } else {
        hover
    }
}

/// The selected row of the sidebar's lists (hosts, port forwards,
/// credentials) and of the recent hosts, so that it stands out from hover.
/// Light, three times the list hover's contrast against the sidebar; dark,
/// where that glares, the deepened hover.
pub(crate) fn selected_row_color(theme: &Theme) -> Hsla {
    let sidebar = theme.sidebar;
    let hover = sidebar.blend(deepened_hover(theme));
    let mut selected = hover.alpha(1.0);
    if theme.is_dark() {
        return selected;
    }
    selected.l = (sidebar.l + (hover.l - sidebar.l) * 3.0).clamp(0.0, 1.0);
    selected
}

#[cfg(test)]
mod tests {
    use gpui_kit::TestAppContext;
    use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};

    use super::{deepen_list_hover, deepened_hover};

    #[gpui_kit::test]
    fn dark_hover_is_fainter_than_a_selected_row(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::component::init(cx);
            for mode in [ThemeMode::Light, ThemeMode::Dark] {
                Theme::change(mode, None, cx);
                deepen_list_hover(cx);
                let theme = cx.theme();
                let selected = deepened_hover(theme);
                assert!(selected.a > 0.9, "{mode:?}: {selected:?}");
                match mode {
                    ThemeMode::Light => assert_eq!(theme.list_hover, selected),
                    ThemeMode::Dark => {
                        assert!((theme.list_hover.a * 2. - selected.a).abs() < 0.01);
                        assert_eq!(theme.table_hover, theme.list_hover);
                    }
                }
            }
        });
    }
}
