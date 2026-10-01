use std::borrow::Cow;

use gpui_kit::assets::icon_assets;
use gpui_kit::{AssetSource, Result, SharedString};

// Icons beyond the default component bundle, embedded from the Lucide
// catalog shipped with `gpui-kit-assets`.
icon_assets!(
    ExtraIcons,
    [
        Server,
        Terminal,
        Upload,
        Download,
        Trash,
        Pencil,
        RefreshCw,
        Plug,
        Unplug,
        FolderTree,
        FolderPlus,
        Eraser,
        ClipboardPaste,
        ClipboardCopy,
        House,
        FolderUp,
        FolderRoot,
        FolderSymlink,
        FileSymlink,
        FilePlus,
        Bookmark,
        SquarePen,
        Settings,
        Palette,
        SquareTerminal,
        CircleMinus,
        Play,
        Square,
        ArrowLeftRight,
        ArrowRightToLine,
        ArrowLeftToLine,
        Laptop,
        Lock,
        CircleAlert,
        Target,
        KeyRound,
        RectangleEllipsis,
        FileKey,
        UserRoundKey,
        CircleArrowUp
    ]
);

/// Names of the icons in `ExtraIcons`, usable anywhere an `Icon` is built.
pub use gpui_kit::assets::IconName as CatalogIcon;

/// Operating-system marks for the session tree, one file per
/// [`crate::session::HostOs`] variant.
///
/// These are brand logos, not Lucide glyphs, because an approximation cannot
/// say "this is a Debian box". The artwork comes from Simple Icons (CC0); the
/// Windows mark is a plain four-pane square drawn here, since Simple Icons no
/// longer ships one. GPUI paints an SVG as a tinted mask, so single-path
/// monochrome files are all that is needed.
macro_rules! os_icons {
    ($($path:literal => $file:literal,)*) => {
        const OS_ICONS: &[(&str, &[u8])] = &[
            $(($path, include_bytes!(concat!("../../assets/icons/os/", $file, ".svg")))),*
        ];
    };
}

os_icons! {
    "icons/os/ubuntu.svg" => "ubuntu",
    "icons/os/debian.svg" => "debian",
    "icons/os/fedora.svg" => "fedora",
    "icons/os/redhat.svg" => "redhat",
    "icons/os/centos.svg" => "centos",
    "icons/os/rockylinux.svg" => "rockylinux",
    "icons/os/almalinux.svg" => "almalinux",
    "icons/os/archlinux.svg" => "archlinux",
    "icons/os/alpinelinux.svg" => "alpinelinux",
    "icons/os/opensuse.svg" => "opensuse",
    "icons/os/gentoo.svg" => "gentoo",
    "icons/os/kalilinux.svg" => "kalilinux",
    "icons/os/manjaro.svg" => "manjaro",
    "icons/os/raspberrypi.svg" => "raspberrypi",
    "icons/os/linux.svg" => "linux",
    "icons/os/apple.svg" => "apple",
    "icons/os/windows.svg" => "windows",
    "icons/os/freebsd.svg" => "freebsd",
    "icons/os/openbsd.svg" => "openbsd",
    "icons/os/netbsd.svg" => "netbsd",
}

/// Asset source for the application: the default component bundle plus the
/// extra icons above.
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = OS_ICONS.iter().find(|(name, _)| *name == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.extend(
            OS_ICONS
                .iter()
                .map(|(name, _)| *name)
                .filter(|name| name.starts_with(path))
                .map(SharedString::from),
        );
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::AppAssets;
    use crate::session::HostOs;
    use gpui_kit::AssetSource as _;

    #[test]
    fn every_operating_system_has_an_icon_in_the_bundle() {
        for os in HostOs::ALL {
            let path = os.icon_path();
            let bytes = AppAssets
                .load(path)
                .unwrap_or_else(|error| panic!("{path} 加载失败：{error}"))
                .unwrap_or_else(|| panic!("{path} 不在资源包里"));
            assert!(bytes.starts_with(b"<svg"), "{path} 不是 SVG");
        }
    }

    #[test]
    fn the_bundle_lists_the_operating_system_icons() {
        let listed = AppAssets.list("icons/os/").unwrap();
        assert_eq!(listed.len(), HostOs::ALL.len());
    }
}
