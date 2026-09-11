use gpui_kit::component::{
    dock::{BasePanel, Panel, PanelEvent, TabGroup},
    menu::PopupMenu,
    resizable::{h_resizable, resizable_panel},
};
use gpui_kit::*;

use crate::app::{CatalogIcon, CloseExplorer, EditSession};
use crate::session::{SessionId, SessionStore};
use crate::shared::ClosableTabTitle;

use super::{DirTree, FilePane, Location, PaneSide, local_tree, remote_home, remote_tree};

/// Emitted to the workspace, which keeps its panel registry and its record of
/// the displayed center tab in step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExplorerPanelEvent {
    /// The panel became the displayed tab of its group.
    Activated(SessionId),
    Closed(SessionId),
}

/// A center tab with the WinSCP-style two-pane file browser for a session.
pub struct ExplorerPanel {
    session_id: SessionId,
    store: Entity<SessionStore>,
    local: Entity<FilePane>,
    remote: Entity<FilePane>,
    focus_handle: FocusHandle,
    tab_group: Option<WeakEntity<TabGroup>>,
    _subscriptions: Vec<Subscription>,
}

impl ExplorerPanel {
    pub fn new(
        session_id: SessionId,
        store: Entity<SessionStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let session = store.read(cx).session(session_id).cloned();
        let local_home = "/Users/xuz";
        let local = cx.new(|cx| {
            FilePane::new(
                PaneSide::Local,
                session_id,
                Location::new(local_tree(), local_home, Some(local_home)),
                window,
                cx,
            )
        });
        let (remote_dirs, home) = session
            .as_ref()
            .map(|session| (remote_tree(session), remote_home(session)))
            .unwrap_or_else(|| (DirTree::new(), "/".to_string()));
        let remote = cx.new(|cx| {
            FilePane::new(
                PaneSide::Remote,
                session_id,
                Location::new(remote_dirs, &home, Some(&home)),
                window,
                cx,
            )
        });
        let subscriptions = vec![cx.observe(&store, |_, _, cx| cx.notify())];

        Self {
            session_id,
            store,
            local,
            remote,
            focus_handle: cx.focus_handle(),
            tab_group: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub fn tab_group(&self) -> Option<WeakEntity<TabGroup>> {
        self.tab_group.clone()
    }

    pub fn local(&self) -> &Entity<FilePane> {
        &self.local
    }

    pub fn remote(&self) -> &Entity<FilePane> {
        &self.remote
    }
}

impl EventEmitter<PanelEvent> for ExplorerPanel {}
impl EventEmitter<ExplorerPanelEvent> for ExplorerPanel {}

impl Focusable for ExplorerPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl BasePanel for ExplorerPanel {
    fn panel_name(&self) -> &'static str {
        "ExplorerPanel"
    }

    fn set_active(&mut self, active: bool, _: &mut Window, cx: &mut Context<Self>) {
        if active {
            let id = self.session_id;
            self.store
                .update(cx, |store, cx| store.set_active(Some(id), cx));
            cx.emit(ExplorerPanelEvent::Activated(id));
        }
    }

    fn on_added_to(&mut self, group: WeakEntity<TabGroup>, _: &mut Window, _: &mut Context<Self>) {
        self.tab_group = Some(group);
    }

    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.tab_group = None;
        cx.emit(ExplorerPanelEvent::Closed(self.session_id));
    }
}

impl Panel for ExplorerPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self
            .store
            .read(cx)
            .session(self.session_id)
            .map(|session| session.name.clone())
            .unwrap_or_else(|| "SFTP".into());
        ClosableTabTitle::new(CatalogIcon::FolderTree, format!("{name} · SFTP")).closable(
            ("close-explorer", self.session_id.0),
            Box::new(CloseExplorer(self.session_id)),
        )
    }

    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> PopupMenu {
        menu.menu("编辑会话…", Box::new(EditSession(self.session_id)))
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Render for ExplorerPanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let sid = self.session_id.0;
        div()
            .id(("explorer", sid))
            .test_support()
            .track_focus(&self.focus_handle)
            .size_full()
            .child(
                // Pane sizes are an API boundary that takes `Pixels`; the
                // resize handle owns the hairline between the panes.
                h_resizable(("explorer-panes", sid))
                    .child(
                        resizable_panel()
                            .size(px(480.))
                            .size_range(px(240.)..Pixels::MAX)
                            .child(self.local.clone()),
                    )
                    .child(
                        resizable_panel()
                            .size_range(px(240.)..Pixels::MAX)
                            .child(self.remote.clone()),
                    ),
            )
    }
}
