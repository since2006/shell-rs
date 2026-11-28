//! 同步浏览, WinSCP's synchronized browsing: when one pane of an SFTP tab
//! changes folder, the other makes the same move. Into a folder, it goes
//! into the one of that name; up, it goes up as far; beside, it goes up to
//! where the two folders meet and down again. A jump to a bookmark, a typed
//! folder or one that shares nothing but the root with the last is the
//! pane's alone, and 主目录 sends both home, as WinSCP does. When the
//! other side has no such folder, it asks to make it; when it cannot
//! follow at all, browsing stops following and says why.
//!
//! The switch is kept for the host (`Host::sync_browsing`), as WinSCP keeps
//! it for the site.

use super::{ExplorerPanel, LoadIntent, PaneOperation, child_path, path_ancestors};
use crate::app::ExplorerCommand;
use crate::host::ConnectionState;
use crate::i18n::t;
use gpui_kit::component::{WindowExt as _, dialog::DialogButtonProps, notification::Notification};
use gpui_kit::*;

/// Where the other pane goes when one moves `from` one folder `to` another,
/// the other being in `other`. `moved_remote` is the side that moved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mirror {
    /// The same move from where the other pane is.
    To(String),
    /// The two folders share nothing but the root, or not even that: a
    /// jump, which the other pane does not make.
    Unrelated,
    /// The other pane would have to go further up than its root.
    Beyond,
}

pub fn mirror(from: &str, to: &str, moved_remote: bool, other: &str) -> Mirror {
    let from = path_ancestors(from, moved_remote);
    let to = path_ancestors(to, moved_remote);
    let shared = from
        .iter()
        .zip(&to)
        .take_while(|((_, a), (_, b))| same_path(a, b, moved_remote))
        .count();
    let along = shared == from.len() || shared == to.len();
    if shared == 0 || (shared == 1 && !along) {
        return Mirror::Unrelated;
    }
    let other_side = path_ancestors(other, !moved_remote);
    let up = from.len() - shared;
    if up >= other_side.len() {
        return Mirror::Beyond;
    }
    let (_, base) = &other_side[other_side.len() - 1 - up];
    Mirror::To(to[shared..].iter().fold(base.clone(), |folder, (name, _)| {
        child_path(&folder, name, !moved_remote)
    }))
}

/// Windows names files without regard to case; servers and the other
/// systems do not.
fn same_path(a: &str, b: &str, remote: bool) -> bool {
    if !remote && cfg!(windows) {
        a.to_lowercase() == b.to_lowercase()
    } else {
        a == b
    }
}

/// A pane's load that follows the other one, and where to.
pub(super) struct Mirroring {
    pub(super) remote: bool,
    pub(super) request_id: u64,
    pub(super) path: String,
    /// Made by 同步浏览 just now: failing again stops following.
    pub(super) created: bool,
}

impl ExplorerPanel {
    pub fn sync_browsing(&self) -> bool {
        self.sync_browsing
    }

    pub(super) fn set_sync_browsing(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.is_sidebar() || self.sync_browsing == on {
            return;
        }
        self.sync_browsing = on;
        self.leading = [false; 2];
        self.mirroring = None;
        let host = self.host_id();
        self.store
            .update(cx, |store, cx| store.set_host_sync_browsing(host, on, cx));
        for pane in [self.pane(false).clone(), self.pane(true).clone()] {
            pane.update(cx, |pane, cx| pane.set_sync_browsing(on, cx));
        }
        cx.notify();
    }

    /// Stop following, and say why: the other side could not.
    pub(super) fn stop_sync_browsing(
        &mut self,
        why: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.sync_browsing {
            return;
        }
        self.set_sync_browsing(false, cx);
        window.push_notification(Notification::warning(why), cx);
    }

    /// Go somewhere the other pane follows to, under 同步浏览: a move into,
    /// up or beside this folder.
    pub(super) fn browse(
        &mut self,
        remote: bool,
        path: String,
        intent: LoadIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.navigate(remote, path, intent, window, cx).is_some() {
            self.leading[usize::from(remote)] = self.sync_browsing;
        }
    }

    /// Whether `path` is a folder above the pane's.
    pub(super) fn is_above(&self, remote: bool, path: &str, cx: &App) -> bool {
        let pane = self.pane(remote).read(cx);
        let path = pane.expanded_path(path);
        let chain = path_ancestors(&pane.path(), remote);
        chain
            .split_last()
            .is_some_and(|(_, above)| above.iter().any(|(_, folder)| *folder == path))
    }

    /// 主目录: under 同步浏览 the other pane goes to its own home too.
    pub(super) fn go_home(&mut self, remote: bool, window: &mut Window, cx: &mut Context<Self>) {
        let home = self.pane(remote).read(cx).home();
        if home.is_empty() {
            return;
        }
        self.navigate(remote, home, LoadIntent::Visit, window, cx);
        if self.sync_browsing {
            let other = self.pane(!remote).read(cx);
            let (home, path) = (other.home(), other.path());
            if !home.is_empty() && home != path {
                self.follow(!remote, home, false, window, cx);
            }
        }
    }

    /// A pane's load ended. One that followed the other is done, or asks
    /// for the folder it could not open; one the user made, under 同步浏览,
    /// takes the other pane along.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn pane_loaded(
        &mut self,
        remote: bool,
        request_id: u64,
        intent: LoadIntent,
        from: &str,
        path: &Result<String, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .mirroring
            .as_ref()
            .is_some_and(|m| m.remote == remote && m.request_id == request_id)
        {
            let mirroring = self.mirroring.take().expect("checked above");
            if let Err(error) = path {
                if mirroring.created {
                    let why = t!("explorer.sync_browsing.stopped_because", error = error);
                    self.stop_sync_browsing(why, window, cx);
                } else {
                    self.ask_to_create(mirroring.remote, mirroring.path, error, window, cx);
                }
            }
            return;
        }
        let leading = std::mem::take(&mut self.leading[usize::from(remote)]);
        if !leading || !self.sync_browsing || intent == LoadIntent::Reload {
            return;
        }
        let Ok(to) = path else {
            return;
        };
        let other = self.pane(!remote).read(cx).path();
        if from.is_empty() || from == to || other.is_empty() {
            return;
        }
        match mirror(from, to, remote, &other) {
            Mirror::To(target) if target == other => {}
            Mirror::To(target) => self.follow(!remote, target, false, window, cx),
            Mirror::Unrelated => {}
            Mirror::Beyond => {
                self.stop_sync_browsing(t!("explorer.sync_browsing.stopped"), window, cx)
            }
        }
    }

    /// Take a pane to `path`, following the other one.
    fn follow(
        &mut self,
        remote: bool,
        path: String,
        created: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.navigate(remote, path.clone(), LoadIntent::Visit, window, cx) {
            Some(request_id) => {
                self.mirroring = Some(Mirroring {
                    remote,
                    request_id,
                    path,
                    created,
                })
            }
            None => self.stop_sync_browsing(
                t!("explorer.sync_browsing.stopped_disconnected"),
                window,
                cx,
            ),
        }
    }

    /// The other side has no folder `path` to follow into: make it, or
    /// stop following, as WinSCP asks.
    fn ask_to_create(
        &mut self,
        remote: bool,
        path: String,
        error: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            let why = t!("explorer.sync_browsing.stopped_because", error = error);
            self.stop_sync_browsing(why, window, cx);
            return;
        }
        let description = if remote {
            t!(
                "explorer.sync_browsing.missing_remote",
                path = path,
                error = error
            )
        } else {
            t!(
                "explorer.sync_browsing.missing_local",
                path = path,
                error = error
            )
        };
        let sender = self.sender();
        let focus = window.focused(cx);
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let restore = {
                let focus = focus.clone();
                move |window: &mut Window, cx: &mut App| {
                    if let Some(focus) = &focus {
                        window.focus(focus, cx);
                    }
                }
            };
            dialog
                .title(t!("explorer.sync_browsing.create_title"))
                .description(description.clone())
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(t!("explorer.sync_browsing.create"))
                        .cancel_text(t!("explorer.sync_browsing.turn_off")),
                )
                .show_cancel(true)
                .on_ok({
                    let (sender, restore, path) = (sender.clone(), restore.clone(), path.clone());
                    move |_, window, cx| {
                        restore(window, cx);
                        let path = path.clone();
                        sender.send(
                            ExplorerCommand::CreateSyncFolder { remote, path },
                            window,
                            cx,
                        );
                        true
                    }
                })
                .on_cancel({
                    let (sender, restore) = (sender.clone(), restore.clone());
                    move |_, window, cx| {
                        restore(window, cx);
                        sender.send(ExplorerCommand::SyncBrowsing { on: false }, window, cx);
                        true
                    }
                })
        });
    }

    /// 创建 in that question: the folder and the ones above it, then into it.
    pub(super) fn create_sync_folder(
        &mut self,
        remote: bool,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.sync_browsing {
            return;
        }
        if remote && self.connection_state() != ConnectionState::Connected {
            let why = t!("explorer.sync_browsing.stopped_disconnected");
            self.stop_sync_browsing(why, window, cx);
        } else if !self.can_modify(remote, cx) {
            self.stop_sync_browsing(t!("explorer.sync_browsing.stopped"), window, cx);
        } else {
            self.start_operation(remote, PaneOperation::CreateFolders { path }, window, cx);
        }
    }

    /// The folder 同步浏览 asked for is made, or could not be.
    pub(super) fn sync_folder_created(
        &mut self,
        remote: bool,
        path: String,
        result: Result<(), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(()) if self.sync_browsing => self.follow(remote, path, true, window, cx),
            Ok(()) => {}
            Err(error) => {
                let why = t!("explorer.sync_browsing.stopped_because", error = error);
                self.stop_sync_browsing(why, window, cx)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Mirror, mirror};

    fn to(path: &str) -> Mirror {
        Mirror::To(path.into())
    }

    // Local paths as macOS and Linux write them; Windows has its own below.
    #[cfg(not(windows))]
    #[test]
    fn the_other_pane_makes_the_same_move() {
        // Into a folder, on either side.
        assert_eq!(
            mirror("/srv/site", "/srv/site/css", true, "/Users/me/site"),
            to("/Users/me/site/css")
        );
        assert_eq!(
            mirror("/Users/me/site", "/Users/me/site/css", false, "/srv/site"),
            to("/srv/site/css")
        );
        // Up, one folder or several, and to the root.
        assert_eq!(
            mirror("/srv/site/css", "/srv/site", true, "/Users/me/site/css"),
            to("/Users/me/site")
        );
        assert_eq!(
            mirror("/srv/site/css", "/srv", true, "/Users/me/site/css"),
            to("/Users/me")
        );
        assert_eq!(
            mirror("/srv/site", "/", true, "/Users/me/site"),
            to("/Users")
        );
        // Beside: up to where the two meet, then down.
        assert_eq!(
            mirror("/srv/site/css", "/srv/site/js", true, "/Users/me/site/css"),
            to("/Users/me/site/js")
        );
        assert_eq!(
            mirror(
                "/srv/site/css/a",
                "/srv/site/js",
                true,
                "/Users/me/site/css/a"
            ),
            to("/Users/me/site/js")
        );
        // Down from the root.
        assert_eq!(mirror("/", "/srv", true, "/Users/me"), to("/Users/me/srv"));
    }

    #[test]
    fn a_jump_elsewhere_is_not_followed_and_the_root_is_as_far_as_it_goes() {
        assert_eq!(
            mirror("/srv/site", "/etc/nginx", true, "/Users/me/site"),
            Mirror::Unrelated
        );
        assert_eq!(
            mirror("/Users/me/site", "/Volumes/USB", false, "/srv/site"),
            Mirror::Unrelated
        );
        assert_eq!(
            mirror("/srv/site/css", "/srv", true, "/Users"),
            Mirror::Beyond
        );
        assert_eq!(mirror("/srv/site", "/", true, "/"), Mirror::Beyond);
    }

    #[cfg(windows)]
    #[test]
    fn windows_folders_compare_without_case_and_drives_do_not_meet() {
        assert_eq!(
            mirror(r"C:\www\site", r"c:\WWW\site\css", false, "/srv/site"),
            to("/srv/site/css")
        );
        assert_eq!(
            mirror("/srv/site", "/srv/site/css", true, r"C:\www\site"),
            to(r"C:\www\site\css")
        );
        assert_eq!(
            mirror(r"C:\www\site", r"D:\backup", false, "/srv/site"),
            Mirror::Unrelated
        );
        assert_eq!(mirror("/srv/a/b", "/srv", true, r"C:\www"), Mirror::Beyond);
    }
}
