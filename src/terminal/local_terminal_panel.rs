use std::borrow::Cow;
use std::rc::Rc;
use std::sync::LazyLock;

use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _,
    button::Button,
    dock::{BasePanel, Panel, PanelEvent, TabGroup},
    h_flex,
    menu::PopupMenu,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use regex::Regex;

use crate::app::{
    CatalogIcon, CenterTab, CloseLocalTerminal, RestartLocalTerminal, SendCommandsLocal,
};
use crate::i18n::t;
use crate::shared::{ClosableTabTitle, close_tab_items};

use super::{
    LocalTerminalId, SessionLogIdentity, SharedTerminalTransportFactory, TerminalEvent,
    TerminalLifecycle, TerminalMenuItems, TerminalNotice, TerminalStatus, TerminalView,
    ZmodemDirection, ZmodemReply,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalTerminalPanelEvent {
    Input(LocalTerminalId, super::TerminalInputEvent),
    Activated(LocalTerminalId),
    Closed(LocalTerminalId),
    StatusChanged(LocalTerminalId),
    Notice(LocalTerminalId, TerminalNotice),
    /// `rz` or `sz` started (a shell of this computer can ssh on): the
    /// user chooses, and the answer goes to [`LocalTerminalPanel::zmodem_reply`].
    ZmodemAsk(LocalTerminalId, ZmodemDirection),
    /// The shell is in another folder now.
    WorkingDirectory(LocalTerminalId, std::path::PathBuf),
}

/// What 项目 calls one of its terminals. Its tab shows the name until the
/// shell or a program titles it, as a terminal app's tab does, or the
/// `title` it had last time until then; a name `fixed` by hand shows over
/// any title.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedName {
    pub name: SharedString,
    pub fixed: bool,
    pub title: Option<SharedString>,
}

/// A local login-shell Dock tab backed by the shared terminal view.
pub struct LocalTerminalPanel {
    id: LocalTerminalId,
    /// Its name in 项目, for a terminal kept there.
    name: Option<SavedName>,
    terminal: Entity<TerminalView>,
    tab_group: Option<WeakEntity<TabGroup>>,
    broadcast: Option<super::BroadcastSelection>,
    _subscriptions: Vec<Subscription>,
}

impl LocalTerminalPanel {
    pub fn new(
        id: LocalTerminalId,
        factory: SharedTerminalTransportFactory,
        name: Option<SavedName>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let fixed = name.as_ref().map(|name| name.name.clone());
        let identity = SessionLogIdentity {
            name: fixed.clone().unwrap_or_else(|| default_title(id)),
            address: "local".into(),
        };
        let terminal = cx.new(|cx| {
            let mut terminal = TerminalView::new(
                ("local-terminal", id.0),
                move |_| fixed.clone().unwrap_or_else(|| default_title(id)),
                identity,
                factory,
                window,
                cx,
            );
            terminal.set_menu_items(local_menu_items(id));
            terminal
        });
        let subscriptions = vec![
            cx.observe(&terminal, move |_, _, cx| {
                cx.emit(LocalTerminalPanelEvent::StatusChanged(id));
                cx.notify();
            }),
            cx.subscribe(
                &terminal,
                move |_, _, event: &TerminalEvent, cx| match event {
                    TerminalEvent::Input(input) => {
                        cx.emit(LocalTerminalPanelEvent::Input(id, input.clone()));
                    }
                    TerminalEvent::Notice(notice) => {
                        cx.emit(LocalTerminalPanelEvent::Notice(id, notice.clone()));
                    }
                    TerminalEvent::ZmodemAsk(direction) => {
                        cx.emit(LocalTerminalPanelEvent::ZmodemAsk(id, *direction));
                    }
                    TerminalEvent::WorkingDirectory(folder) => {
                        cx.emit(LocalTerminalPanelEvent::WorkingDirectory(
                            id,
                            folder.clone(),
                        ));
                    }
                    _ => {}
                },
            ),
        ];

        Self {
            id,
            name,
            terminal,
            tab_group: None,
            broadcast: None,
            _subscriptions: subscriptions,
        }
    }

    /// 「本地终端 1」, or its name in 项目: what a notification calls it,
    /// whatever title the shell gives the tab.
    pub fn default_title(&self) -> SharedString {
        self.name
            .as_ref()
            .map_or_else(|| default_title(self.id), |name| name.name.clone())
    }

    pub fn tab_group(&self) -> Option<WeakEntity<TabGroup>> {
        self.tab_group.clone()
    }

    pub fn terminal(&self) -> &Entity<TerminalView> {
        &self.terminal
    }

    /// What the user chose for the transfer `rz` or `sz` started.
    pub fn zmodem_reply(&self, reply: ZmodemReply, cx: &App) {
        self.terminal.read(cx).zmodem_reply(reply, cx);
    }

    pub(crate) fn set_broadcast_selection(
        &mut self,
        selection: Option<super::BroadcastSelection>,
        cx: &mut Context<Self>,
    ) {
        if self.broadcast != selection {
            self.broadcast = selection;
            cx.notify();
        }
    }

    pub fn status(&self, cx: &App) -> TerminalStatus {
        self.terminal.read(cx).status(cx)
    }

    /// Start anew with `factory`, which runs the shell the settings name
    /// now.
    pub fn restart(
        &mut self,
        factory: SharedTerminalTransportFactory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.terminal.update(cx, |terminal, cx| {
            terminal.restart_with_factory(factory, cx)
        });
        let focus = self.terminal.read(cx).focus_handle();
        window.focus(&focus, cx);
    }

    /// Call it by its name in 项目, renamed there.
    pub fn set_name(&mut self, name: SavedName, cx: &mut Context<Self>) {
        if self.name.as_ref() != Some(&name) {
            self.name = Some(name);
            cx.notify();
        }
    }

    /// The title its shell or a program gives it, short: `None` before one
    /// does. 项目 shows it for a terminal's name, as the tab does.
    pub fn shell_title(&self, cx: &App) -> Option<SharedString> {
        let title = self.terminal.read(cx).title(cx)?;
        Some(
            short_title(&title, super::local_shell::host_name().as_deref())
                .into_owned()
                .into(),
        )
    }

    pub(crate) fn display_title(&self, cx: &App) -> SharedString {
        if let Some(name) = self.name.as_ref().filter(|name| name.fixed) {
            return name.name.clone();
        }
        self.shell_title(cx)
            .or_else(|| self.name.as_ref().and_then(|name| name.title.clone()))
            .unwrap_or_else(|| self.default_title())
    }
}

impl EventEmitter<PanelEvent> for LocalTerminalPanel {}
impl EventEmitter<LocalTerminalPanelEvent> for LocalTerminalPanel {}

impl Focusable for LocalTerminalPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.terminal.read(cx).focus_handle()
    }
}

impl BasePanel for LocalTerminalPanel {
    fn panel_name(&self) -> &'static str {
        "LocalTerminalPanel"
    }

    /// Closing goes through `CloseLocalTerminal`; see `ClosableTabTitle`.
    /// Saying no here also keeps the dock from adding a second 「关闭」 to the
    /// 「…」 menu, beside the tab's own close commands.
    fn closable(&self, _: &App) -> bool {
        false
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if active {
            let focus = self.terminal.read(cx).focus_handle();
            window.focus(&focus, cx);
            cx.emit(LocalTerminalPanelEvent::Activated(self.id));
        }
    }

    fn on_added_to(&mut self, group: WeakEntity<TabGroup>, _: &mut Window, _: &mut Context<Self>) {
        self.tab_group = Some(group);
    }

    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.tab_group = None;
        self.terminal
            .update(cx, |terminal, cx| terminal.shutdown(cx));
        cx.emit(LocalTerminalPanelEvent::Closed(self.id));
    }
}

impl Panel for LocalTerminalPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (id, group, panel) = (self.id, self.tab_group.clone(), cx.entity_id());
        let mark = super::BroadcastSelection::lead(
            self.broadcast.clone(),
            CenterTab::LocalTerminal(id),
            self.display_title(cx),
            Icon::new(CatalogIcon::Terminal).small(),
        );
        ClosableTabTitle::new(
            ("local-terminal-tab", self.id.0),
            mark,
            self.display_title(cx),
        )
        .closable(
            ("close-local-terminal", self.id.0),
            Box::new(CloseLocalTerminal(self.id)),
        )
        .context_menu(move |menu, _, cx| tab_menu(menu, id, group.clone(), panel, cx))
    }

    fn toolbar_buttons(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<Vec<Button>> {
        let id = self.id;
        Some(vec![
            Button::new(("restart-local-terminal-toolbar", id.0))
                .icon(Icon::new(CatalogIcon::RefreshCw))
                .tooltip(t!("terminal.local.restart"))
                .on_click(move |_, window, cx| {
                    window.dispatch_action(Box::new(RestartLocalTerminal(id)), cx)
                }),
        ])
    }

    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        tab_menu(menu, self.id, self.tab_group.clone(), cx.entity_id(), cx)
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Render for LocalTerminalPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let lifecycle = self.terminal.read(cx).lifecycle(cx);
        let show_restart = matches!(
            lifecycle,
            TerminalLifecycle::Exited { .. } | TerminalLifecycle::Failed(_)
        );
        let id = self.id;

        v_flex()
            .size_full()
            .child(div().flex_1().min_h_0().child(self.terminal.clone()))
            .when(show_restart, |column| {
                column.child(
                    h_flex()
                        .id(("local-terminal-exit", id.0))
                        .test_support()
                        .justify_between()
                        .px_3()
                        .py_2()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().muted)
                        .text_sm()
                        .child(lifecycle.to_string())
                        .child(
                            Button::new(("restart-local-terminal", id.0))
                                .small()
                                .icon(Icon::new(CatalogIcon::RefreshCw))
                                .label(t!("terminal.local.restart"))
                                .on_click(move |_, window, cx| {
                                    window.dispatch_action(Box::new(RestartLocalTerminal(id)), cx)
                                }),
                        ),
                )
            })
    }
}

/// A title up to this long shows whole, `xuz@mac:~/src` as it is: the
/// folder alone, `src` or `~`, says too little.
const WHOLE_TITLE_CHARS: usize = 30;

/// The shell's title, short enough to tell tabs apart by.
///
/// - Shells title a window `user@host:~/src/app` (oh-my-zsh, bash on
///   Debian and Fedora). On `this_host`, a title longer than
///   [`WHOLE_TITLE_CHARS`] shows as the folder alone, `app`: the user and
///   host are the same on every local tab. A host ssh'd to from here keeps
///   the whole title.
/// - The Windows console titles a window with the whole path of what runs
///   in it (`C:\Windows\system32\cmd.exe - ping localhost`), which leaves a
///   tab little else; each such path shows as the program's name, `cmd`.
fn short_title<'a>(title: &'a str, this_host: Option<&str>) -> Cow<'a, str> {
    static USER_HOST_FOLDER: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[^@\s]+@([^:\s]+): ?([~/].*)$").expect("pattern"));
    static PROGRAM_PATH: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)\b[a-z]:\\(?:[^\\/:*?"<>|\r\n]+\\)*([^\\/:*?"<>|\r\n]+?)\.exe\b"#)
            .expect("pattern")
    });
    if let Some(this_host) = this_host
        && title.chars().count() > WHOLE_TITLE_CHARS
        && let Some(parts) = USER_HOST_FOLDER.captures(title)
        && is_this_host(&parts[1], this_host)
    {
        let path = parts.get(2).expect("group").as_str();
        return match path.trim_end_matches('/') {
            "" => Cow::Borrowed("/"),
            path => Cow::Borrowed(path.rsplit('/').next().unwrap_or(path)),
        };
    }
    PROGRAM_PATH.replace_all(title, "$1")
}

/// Whether `host` in a title names this computer, `this_host`: whole, or
/// up to its first dot as prompts write it.
fn is_this_host(host: &str, this_host: &str) -> bool {
    let short = this_host.split('.').next().unwrap_or(this_host);
    host.eq_ignore_ascii_case(this_host) || host.eq_ignore_ascii_case(short)
}

/// 「本地终端 1」, in the interface language.
fn default_title(id: LocalTerminalId) -> SharedString {
    t!("terminal.local.title", number = id.0)
}

/// What a local terminal adds to its context menu: 发送命令 at the bottom,
/// while the shell takes input.
fn local_menu_items(id: LocalTerminalId) -> TerminalMenuItems {
    TerminalMenuItems {
        copy: Rc::new(|menu, _| menu),
        commands: Rc::new(move |menu, lifecycle| {
            let connected = matches!(
                lifecycle,
                TerminalLifecycle::Starting | TerminalLifecycle::Running
            );
            menu.menu_with_icon_and_disabled(
                t!("terminal.menu.send_commands"),
                Icon::new(CatalogIcon::Send),
                Box::new(SendCommandsLocal(id)),
                !connected,
            )
        }),
    }
}

/// The commands of a local terminal tab, shared by its context menu and the
/// tab bar's 「…」 menu.
fn tab_menu(
    menu: PopupMenu,
    id: LocalTerminalId,
    group: Option<WeakEntity<TabGroup>>,
    panel: EntityId,
    cx: &App,
) -> PopupMenu {
    let menu = menu
        .menu_with_icon(
            t!("terminal.local.restart"),
            Icon::new(CatalogIcon::RefreshCw),
            Box::new(RestartLocalTerminal(id)),
        )
        .separator();
    close_tab_items(menu, CenterTab::LocalTerminal(id), group, panel, cx)
}

#[cfg(test)]
mod tests {
    use super::short_title;

    #[test]
    fn a_programs_path_in_the_title_shows_as_its_name() {
        for (title, shown) in [
            (
                r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
                "powershell",
            ),
            (r"C:\Windows\system32\cmd.exe", "cmd"),
            (
                r"C:\WINDOWS\system32\cmd.exe - ping  localhost",
                "cmd - ping  localhost",
            ),
            (r"C:\Program Files\PowerShell\7\pwsh.exe", "pwsh"),
            (r"管理员: C:\Windows\system32\cmd.exe", "管理员: cmd"),
            // What is not a program's path stays.
            ("xuz@mac: ~/src", "xuz@mac: ~/src"),
            (r"D:\work", r"D:\work"),
            ("vim notes.exe.txt", "vim notes.exe.txt"),
        ] {
            assert_eq!(short_title(title, None), shown, "{title}");
        }
    }

    #[test]
    fn this_computers_user_and_host_show_as_the_folder() {
        let here = Some("XuzdeMacBook-Pro.local");
        for (title, shown) in [
            (
                "xuz@XuzdeMacBook-Pro:~/Documents/Projects/GoProjects/go-dock",
                "go-dock",
            ),
            ("xuz@xuzdemacbook-pro: ~/Documents/src/", "src"),
            ("root@XuzdeMacBook-Pro.local:/var/log", "log"),
            ("xuz@XuzdeMacBook-Pro:~/My Projects/app one", "app one"),
            ("administrator@XuzdeMacBook-Pro.local:~", "~"),
            ("administrator@XuzdeMacBook-Pro.local:/", "/"),
            // A short title shows whole.
            ("xuz@XuzdeMacBook-Pro:~", "xuz@XuzdeMacBook-Pro:~"),
            ("xuz@XuzdeMacBook-Pro:~/src", "xuz@XuzdeMacBook-Pro:~/src"),
            // Another host, ssh'd to from here, keeps its name.
            (
                "root@web-01.example.com:/var/log/nginx",
                "root@web-01.example.com:/var/log/nginx",
            ),
            // Not a folder: what runs, or another kind of title.
            (
                "xuz@XuzdeMacBook-Pro: vim /etc/nginx/nginx.conf",
                "xuz@XuzdeMacBook-Pro: vim /etc/nginx/nginx.conf",
            ),
            ("claude", "claude"),
            ("ssh root@web-01", "ssh root@web-01"),
        ] {
            assert_eq!(short_title(title, here), shown, "{title}");
        }
        // With no name for this computer, nothing is taken for it.
        assert_eq!(
            short_title("xuz@XuzdeMacBook-Pro:~/Documents/src", None),
            "xuz@XuzdeMacBook-Pro:~/Documents/src"
        );
    }
}
