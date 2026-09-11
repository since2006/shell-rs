use std::borrow::Cow;

use gpui_kit::assets::icon_assets;
use gpui_kit::{AssetSource, Result, SharedString};

// Icons beyond the default component bundle, embedded from the Lucide
// catalog shipped with `gpui-kit-assets`.
icon_assets!(
    ExtraIcons,
    [
        Server, Terminal, Upload, Download, Trash, Pencil, RefreshCw, Plug, Unplug, FolderTree,
        FolderPlus
    ]
);

/// Names of the icons in `ExtraIcons`, usable anywhere an `Icon` is built.
pub use gpui_kit::assets::IconName as CatalogIcon;

/// Asset source for the application: the default component bundle plus the
/// extra icons above.
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}
