use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    dock::{BasePanel, Panel, PanelControl, PanelEvent},
    h_flex, v_flex,
};
use gpui_kit::*;

use crate::app::{CatalogIcon, OpenSettings};
use crate::credential::CredentialPanel;
use crate::forward::ForwardPanel;
use crate::host::HostPanel;
use crate::i18n::t;

/// Which list the left dock is showing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SidebarMode {
    #[default]
    Hosts,
    Forwards,
    Credentials,
}

/// The left dock's one panel: the host tree, the port-forwarding list or
/// the credential list, whichever the title bar's switch picked, over the
/// 设置 button they share.
///
/// One panel showing either list rather than two panels swapped in the dock:
/// swapping would take the displaced list out of the dock (and the focus
/// with it), and two panels in one tab group always draw a tab bar. Every
/// list stays alive while hidden, so each keeps its search, its selection
/// and its scroll position.
pub struct Sidebar {
    mode: SidebarMode,
    hosts: Entity<HostPanel>,
    forwards: Entity<ForwardPanel>,
    credentials: Entity<CredentialPanel>,
}

impl Sidebar {
    pub fn new(
        hosts: Entity<HostPanel>,
        forwards: Entity<ForwardPanel>,
        credentials: Entity<CredentialPanel>,
    ) -> Self {
        Self {
            mode: SidebarMode::default(),
            hosts,
            forwards,
            credentials,
        }
    }

    pub fn mode(&self) -> SidebarMode {
        self.mode
    }

    /// Show one of the lists and give it the focus. The list that was
    /// showing leaves the element tree, and a focus left behind on it would
    /// take every window-level shortcut's dispatch path with it.
    pub fn show(&mut self, mode: SidebarMode, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = mode;
        let handle = self.focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Put the keyboard in the search field of the list being shown.
    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.mode {
            SidebarMode::Hosts => self
                .hosts
                .update(cx, |panel, cx| panel.focus_search(window, cx)),
            SidebarMode::Forwards => self
                .forwards
                .update(cx, |panel, cx| panel.focus_search(window, cx)),
            SidebarMode::Credentials => self
                .credentials
                .update(cx, |panel, cx| panel.focus_search(window, cx)),
        }
    }
}

impl EventEmitter<PanelEvent> for Sidebar {}

impl Focusable for Sidebar {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self.mode {
            SidebarMode::Hosts => self.hosts.read(cx).focus_handle(cx),
            SidebarMode::Forwards => self.forwards.read(cx).focus_handle(cx),
            SidebarMode::Credentials => self.credentials.read(cx).focus_handle(cx),
        }
    }
}

impl BasePanel for Sidebar {
    fn panel_name(&self) -> &'static str {
        "Sidebar"
    }

    fn closable(&self, _: &App) -> bool {
        false
    }

    fn zoomable(&self, _: &App) -> bool {
        false
    }
}

/// The dock's title bar belongs to whichever list is showing.
impl Panel for Sidebar {
    fn title(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.mode {
            SidebarMode::Hosts => self
                .hosts
                .update(cx, |panel, cx| panel.title(window, cx).into_any_element()),
            SidebarMode::Forwards => self.forwards.read(cx).title().into_any_element(),
            SidebarMode::Credentials => self.credentials.read(cx).title().into_any_element(),
        }
    }

    fn toolbar_buttons(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Vec<Button>> {
        match self.mode {
            SidebarMode::Hosts => self
                .hosts
                .update(cx, |panel, cx| panel.toolbar_buttons(window, cx)),
            SidebarMode::Forwards => Some(self.forwards.read(cx).toolbar_buttons()),
            SidebarMode::Credentials => Some(self.credentials.read(cx).toolbar_buttons()),
        }
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        None
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Render for Sidebar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let list = match self.mode {
            SidebarMode::Hosts => self.hosts.clone().into_any_element(),
            SidebarMode::Forwards => self.forwards.clone().into_any_element(),
            SidebarMode::Credentials => self.credentials.clone().into_any_element(),
        };
        v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .child(div().flex_1().min_h_0().child(list))
            .child(
                // Pinned under the list, where desktop apps keep settings.
                h_flex()
                    .p_2()
                    .border_t_1()
                    .border_color(cx.theme().sidebar_border)
                    .child(
                        Button::new("open-settings")
                            .ghost()
                            .small()
                            .icon(Icon::new(CatalogIcon::Settings))
                            .label(t!("workspace.sidebar.settings"))
                            .tooltip_with_action(
                                t!("workspace.sidebar.open_settings"),
                                &OpenSettings,
                                None,
                            )
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(OpenSettings), cx)
                            }),
                    ),
            )
    }
}
