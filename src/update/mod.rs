//! 在线升级：检查、下载、校验并安装新版本。
//!
//! The manifest and packages come from `dl.shellrs.com` (`feed`), are
//! believed only after `verify`, and `install` puts the new version in
//! place the way each platform's package wants. Nothing here knows about
//! the window: the workspace wires the updater to the title bar, the
//! settings page and the restart.

pub mod build_info;
pub mod error;
pub mod feed;
pub mod install;
// Each platform's installer also builds in the tests elsewhere, as far as
// its system calls allow, so that a Mac runs them all.
#[cfg(any(target_os = "linux", all(test, unix)))]
pub mod install_linux;
#[cfg(any(target_os = "macos", test))]
pub mod install_macos;
#[cfg(any(windows, test))]
pub mod install_windows;
pub mod manifest;
pub mod platform;
pub mod status;
pub mod update_dialog;
pub mod updater;
pub mod verify;

pub use build_info::Channel;
pub use error::UpdateError;
pub use feed::{HttpFeed, UpdateFeed};
pub use install::{InstallKind, Installer, Launch, Relaunch, Staged, Unsupported};
pub use manifest::{Manifest, Offer, Release};
pub use status::{RestartImpact, Tone, UpdateBadge, restart_note};
pub use update_dialog::{ImpactCounter, open_update_dialog};
pub use updater::{
    ManualUpdate, Phase, Stage, UpdateServices, UpdateSnapshot, Updater, UpdaterEvent,
    start_handed_over,
};
pub use verify::{Opened, TrustedKeys};
