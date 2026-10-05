use std::path::PathBuf;

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::{DialogAction, DialogFooter},
    dock::{BasePanel, Panel, PanelEvent, TabGroup},
    h_flex,
    input::{Editor, EditorState, InputEvent, Position, Rope, RopeExt as _},
    menu::PopupMenu,
    notification::Notification,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{EditorId, EditorKey, cursor_label, format_label, indentation, language_for};
use crate::app::{
    CatalogIcon, CenterTab, CloseEditor, EDITOR_CONTEXT, EditorAction, EditorCommand,
    EditorShortcut,
};
use crate::explorer::{ExplorerId, ExplorerPanel, FileLocation};
use crate::host::{HostId, HostStore};
use crate::sftp::{
    FileStamp, ReadFailure, SaveFailure, SharedLocalDirectoryProvider, TextFile, TextFormat,
    read_local_text, write_local_text,
};
use crate::shared::{ClosableTabTitle, HostMark, close_tab_items, confirm_danger, soft_tag};

/// Where an editor reads and writes its file. A remote file goes through
/// the SFTP tab that opened it, over that tab's connection; a local one
/// straight to the disk.
pub enum EditorSource {
    Remote {
        explorer: WeakEntity<ExplorerPanel>,
        explorer_id: ExplorerId,
        host: HostId,
    },
    Local {
        provider: SharedLocalDirectoryProvider,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorPanelEvent {
    Activated(EditorId),
    Closed(EditorId),
    /// What the window's status line shows for this tab changed: the
    /// cursor, or whether there are changes.
    Changed(EditorId),
    /// The file was written; panes showing its folder read it again.
    Saved(EditorId),
    /// A save asked to close the tab after it is done, and it is.
    CloseRequested(EditorId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Saving,
    Reloading,
}

/// A center tab that edits one text file.
pub struct EditorPanel {
    id: EditorId,
    source: EditorSource,
    location: FileLocation,
    store: Entity<HostStore>,
    state: Entity<EditorState>,
    /// The text as last read or saved, to tell whether there are changes.
    saved: Rope,
    format: TextFormat,
    /// What the file looked like when read or saved; `None` once a save
    /// broke off halfway, after which the file is no longer what was read.
    stamp: Option<FileStamp>,
    dirty: bool,
    phase: Phase,
    /// Close the tab once the save under way succeeds (保存 when closing).
    close_after_save: bool,
    cursor: Position,
    /// The workspace's focus handle, which commands from dialogs and
    /// buttons are dispatched on.
    dispatch: FocusHandle,
    tab_group: Option<WeakEntity<TabGroup>>,
    _task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EditorPanel {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: EditorId,
        source: EditorSource,
        location: FileLocation,
        file: TextFile,
        store: Entity<HostStore>,
        dispatch: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let language = language_for(&location.name());
        let (text, format, stamp) = file.into_parts();
        let tab = indentation(language, &text);
        let state = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(language)
                .tab_size(tab)
                .default_value(text)
        });
        let saved = state.read(cx).text().clone();
        let subscriptions = vec![
            cx.subscribe(&state, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.update_dirty(cx);
                }
            }),
            // The cursor moves without a change of text.
            cx.observe(&state, |this, state, cx| {
                let cursor = state.read(cx).cursor_position();
                if cursor != this.cursor {
                    this.cursor = cursor;
                    cx.emit(EditorPanelEvent::Changed(this.id));
                }
            }),
            cx.observe(&store, |_, _, cx| cx.notify()),
        ];
        Self {
            id,
            source,
            location,
            store,
            state,
            saved,
            format,
            stamp: Some(stamp),
            dirty: false,
            phase: Phase::Idle,
            close_after_save: false,
            cursor: Position::default(),
            dispatch,
            tab_group: None,
            _task: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn id(&self) -> EditorId {
        self.id
    }
    /// The file this tab edits, to find it when the file is opened again.
    pub fn key(&self) -> EditorKey {
        match (&self.source, &self.location) {
            (EditorSource::Remote { host, .. }, FileLocation::Remote(path)) => {
                EditorKey::Remote(*host, path.clone())
            }
            _ => EditorKey::Local(PathBuf::from(self.location.path())),
        }
    }
    pub fn location(&self) -> &FileLocation {
        &self.location
    }
    pub fn name(&self) -> String {
        self.location.name()
    }
    /// The SFTP tab a remote file is read and written through.
    pub fn explorer_id(&self) -> Option<ExplorerId> {
        match &self.source {
            EditorSource::Remote { explorer_id, .. } => Some(*explorer_id),
            EditorSource::Local { .. } => None,
        }
    }
    pub fn host_id(&self) -> Option<HostId> {
        match &self.source {
            EditorSource::Remote { host, .. } => Some(*host),
            EditorSource::Local { .. } => None,
        }
    }
    /// There are changes not saved yet.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }
    pub fn state(&self) -> &Entity<EditorState> {
        &self.state
    }
    pub fn tab_group(&self) -> Option<WeakEntity<TabGroup>> {
        self.tab_group.clone()
    }
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.state
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx)
    }
    /// 「行 12，列 5」 for the window's status line.
    pub fn cursor_label(&self) -> String {
        cursor_label(self.cursor.line, self.cursor.character)
    }
    /// 「UTF-8 · LF」 for the window's status line.
    pub fn format_label(&self) -> String {
        format_label(self.format)
    }

    pub fn execute(&mut self, command: EditorCommand, window: &mut Window, cx: &mut Context<Self>) {
        match command {
            EditorCommand::Save => self.save(false, window, cx),
            EditorCommand::Overwrite { close } => {
                self.close_after_save = close;
                self.save(true, window, cx);
            }
            EditorCommand::SaveAndClose => {
                if self.dirty {
                    self.close_after_save = true;
                    self.save(false, window, cx);
                } else {
                    cx.emit(EditorPanelEvent::CloseRequested(self.id));
                }
            }
            EditorCommand::Reload => {
                if self.dirty {
                    let action = EditorAction::new(self.id, EditorCommand::ReloadConfirmed);
                    let dispatch = self.dispatch.clone();
                    confirm_danger(
                        format!("放弃对“{}”的修改并重新加载？", self.name()).into(),
                        Some("编辑器里没有保存的修改会丢失。".into()),
                        "重新加载",
                        std::rc::Rc::new(move |window, cx| send(&dispatch, &action, window, cx)),
                        window,
                        cx,
                    );
                } else {
                    self.reload(window, cx);
                }
            }
            EditorCommand::ReloadConfirmed => self.reload(window, cx),
            // The workspace closes the tab.
            EditorCommand::CloseConfirmed => {}
            EditorCommand::CopyPath => {
                cx.write_to_clipboard(ClipboardItem::new_string(self.location.path()))
            }
        }
    }

    fn update_dirty(&mut self, cx: &mut Context<Self>) {
        let dirty = {
            let text = self.state.read(cx).text();
            text.len() != self.saved.len() || *text != self.saved
        };
        if dirty != self.dirty {
            self.dirty = dirty;
            cx.emit(EditorPanelEvent::Changed(self.id));
            cx.notify();
        }
    }

    /// Write the text back; `overwrite` skips the check that nobody changed
    /// the file since.
    fn save(&mut self, overwrite: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.phase != Phase::Idle || !self.dirty && !overwrite {
            if self.close_after_save && !self.dirty {
                self.close_after_save = false;
                cx.emit(EditorPanelEvent::CloseRequested(self.id));
            }
            return;
        }
        // Typing goes on while the file is written; what is saved is this.
        let snapshot = self.state.read(cx).text().clone();
        let bytes = self.format.encode(&snapshot.to_string());
        let expected = if overwrite { None } else { self.stamp };
        let write = match &self.source {
            EditorSource::Remote { explorer, .. } => {
                let FileLocation::Remote(path) = &self.location else {
                    return;
                };
                let Some(explorer) = explorer.upgrade() else {
                    self.close_after_save = false;
                    return;
                };
                let path = path.clone();
                // `None`: not connected, and the SFTP tab said so.
                match explorer.update(cx, |explorer, cx| {
                    explorer.write_remote_file(path, bytes, expected, window, cx)
                }) {
                    Some(write) => write,
                    None => {
                        self.close_after_save = false;
                        return;
                    }
                }
            }
            EditorSource::Local { provider } => {
                let (provider, path) = (provider.clone(), PathBuf::from(self.location.path()));
                cx.background_spawn(async move {
                    write_local_text(provider.as_ref(), &path, &bytes, expected)
                })
            }
        };
        self.phase = Phase::Saving;
        cx.notify();
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = write.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.finish_save(snapshot, result, window, cx)
            });
        }));
    }

    fn finish_save(
        &mut self,
        snapshot: Rope,
        result: Result<FileStamp, SaveFailure>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.phase = Phase::Idle;
        let close = std::mem::take(&mut self.close_after_save);
        let name = self.name();
        match result {
            Ok(stamp) => {
                self.stamp = Some(stamp);
                self.saved = snapshot;
                self.update_dirty(cx);
                cx.emit(EditorPanelEvent::Saved(self.id));
                if close && !self.dirty {
                    cx.emit(EditorPanelEvent::CloseRequested(self.id));
                }
            }
            Err(SaveFailure::Changed) => {
                let place = if self.location.is_remote() {
                    "服务器上的"
                } else {
                    "本机上的"
                };
                let action = EditorAction::new(self.id, EditorCommand::Overwrite { close });
                let dispatch = self.dispatch.clone();
                confirm_danger(
                    format!("“{name}”在打开后已被修改").into(),
                    Some(
                        format!(
                            "{place}这个文件在你打开或上次保存之后被改过或删除了。覆盖会用编辑器里的内容替换它。"
                        )
                        .into(),
                    ),
                    "覆盖",
                    std::rc::Rc::new(move |window, cx| send(&dispatch, &action, window, cx)),
                    window,
                    cx,
                );
            }
            Err(SaveFailure::Interrupted(message)) => {
                // What is on disk is neither what was read nor what was meant.
                self.stamp = None;
                window.push_notification(
                    Notification::error(format!(
                        "{message}。文件可能只写了一部分，请重新连接后再保存一次。"
                    ))
                    .title(format!("没有保存完“{name}”")),
                    cx,
                );
            }
            Err(SaveFailure::Failed(message)) => window.push_notification(
                Notification::error(message).title(format!("无法保存“{name}”")),
                cx,
            ),
        }
        cx.emit(EditorPanelEvent::Changed(self.id));
        cx.notify();
    }

    /// Read the file again, changes or not.
    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.phase != Phase::Idle {
            return;
        }
        let read = match &self.source {
            EditorSource::Remote { explorer, .. } => {
                let Some(explorer) = explorer.upgrade() else {
                    return;
                };
                let location = self.location.clone();
                match explorer.update(cx, |explorer, cx| explorer.read_file(&location, window, cx))
                {
                    Some(read) => read,
                    None => return,
                }
            }
            EditorSource::Local { provider } => {
                let (provider, path) = (provider.clone(), PathBuf::from(self.location.path()));
                cx.background_spawn(async move { read_local_text(provider.as_ref(), &path) })
            }
        };
        self.phase = Phase::Reloading;
        cx.notify();
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = read.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.finish_reload(result, window, cx)
            });
        }));
    }

    fn finish_reload(
        &mut self,
        result: Result<TextFile, ReadFailure>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.phase = Phase::Idle;
        match result {
            Ok(file) => {
                let (text, format, stamp) = file.into_parts();
                let line = self.cursor.line;
                self.state.update(cx, |state, cx| {
                    state.set_value(text, window, cx);
                    // Back to the line the cursor was on, as near as the
                    // new text allows.
                    let last = state.text().lines_len().saturating_sub(1) as u32;
                    state.set_cursor_position(Position::new(line.min(last), 0), window, cx);
                });
                self.saved = self.state.read(cx).text().clone();
                self.format = format;
                self.stamp = Some(stamp);
                self.update_dirty(cx);
            }
            Err(failure) => window.push_notification(
                Notification::error(failure.to_string())
                    .title(format!("无法重新加载“{}”", self.name())),
                cx,
            ),
        }
        cx.emit(EditorPanelEvent::Changed(self.id));
        cx.notify();
    }

    /// Ask what to do with the changes before the tab closes: save them,
    /// throw them away, or keep the tab.
    pub fn confirm_close(&self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.name();
        let dispatch = self.dispatch.clone();
        let save = EditorAction::new(self.id, EditorCommand::SaveAndClose);
        let discard = EditorAction::new(self.id, EditorCommand::CloseConfirmed);
        let focus = self.state.read(cx).focus_handle(cx);
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(format!("保存对“{name}”的修改？"))
                .overlay_closable(false)
                .child(div().text_sm().child("不保存的话，这些修改会丢失。"))
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("editor-close-cancel")
                                .label("取消")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("editor-close-discard")
                                .danger()
                                .label("放弃修改")
                                .on_click({
                                    let (dispatch, discard) = (dispatch.clone(), discard.clone());
                                    move |_, window, cx| {
                                        window.close_dialog(cx);
                                        send(&dispatch, &discard, window, cx);
                                    }
                                }),
                        )
                        .child(
                            DialogAction::new()
                                .child(Button::new("editor-close-save").primary().label("保存")),
                        ),
                )
                .on_ok({
                    let (dispatch, save) = (dispatch.clone(), save.clone());
                    move |_, window, cx| {
                        send(&dispatch, &save, window, cx);
                        true
                    }
                })
                .on_close({
                    let focus = focus.clone();
                    move |_, window, cx| window.focus(&focus, cx)
                })
        });
    }

    fn display_path(&self, cx: &App) -> String {
        match &self.source {
            EditorSource::Remote { host, .. } => {
                let host = self
                    .store
                    .read(cx)
                    .host(*host)
                    .map(|host| host.name.to_string())
                    .unwrap_or_default();
                format!("{host}:{}", self.location.path())
            }
            EditorSource::Local { .. } => self.location.path(),
        }
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.id;
        let path: SharedString = self.display_path(cx).into();
        let leading = match &self.source {
            EditorSource::Remote { host, .. } => {
                let host = self.store.read(cx).host(*host);
                HostMark::new(
                    ("editor-host", id.0),
                    host.map(|host| host.name.clone()).unwrap_or_default(),
                    host.and_then(|host| host.os),
                )
                .small()
                .into_any_element()
            }
            EditorSource::Local { .. } => Icon::new(CatalogIcon::Laptop)
                .small()
                .text_color(cx.theme().muted_foreground)
                .into_any_element(),
        };
        let state = match self.phase {
            Phase::Saving => Some(("正在保存…", cx.theme().muted_foreground)),
            Phase::Reloading => Some(("正在重新加载…", cx.theme().muted_foreground)),
            Phase::Idle if self.dirty => Some(("已修改", cx.theme().warning)),
            Phase::Idle => None,
        };
        let command = |command: EditorCommand| {
            let (dispatch, action) = (self.dispatch.clone(), EditorAction::new(id, command));
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                send(&dispatch, &action, window, cx)
            }
        };
        h_flex()
            .id(("editor-toolbar", id.0))
            .flex_shrink_0()
            .gap_2()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(leading)
            .child(
                div()
                    .id(("editor-path", id.0))
                    .test_support()
                    .aria_label(path.clone())
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis_middle()
                    .text_sm()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_color(cx.theme().muted_foreground)
                    .tooltip({
                        let path = path.clone();
                        move |window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new(path.clone())
                                .build(window, cx)
                        }
                    })
                    .child(path),
            )
            .when_some(state, |row, (text, color)| {
                row.child(
                    div()
                        .id(("editor-state", id.0))
                        .test_support()
                        .aria_label(text)
                        .flex_shrink_0()
                        .child(soft_tag(text, color)),
                )
            })
            .child(
                Button::new(("editor-reload", id.0))
                    .ghost()
                    .small()
                    .icon(Icon::new(CatalogIcon::RefreshCw))
                    .label("重新加载")
                    .disabled(self.phase != Phase::Idle)
                    .on_click(command(EditorCommand::Reload)),
            )
            .child(
                Button::new(("editor-save", id.0))
                    .small()
                    .icon(Icon::new(CatalogIcon::Save))
                    .label("保存")
                    .disabled(!self.dirty || self.phase != Phase::Idle)
                    .tooltip_with_action(
                        "保存",
                        &EditorShortcut(EditorCommand::Save),
                        Some(EDITOR_CONTEXT),
                    )
                    .on_click(command(EditorCommand::Save)),
            )
    }
}

/// Dispatch an editor command on the workspace, after the current update:
/// from a dialog or a button, where nothing in the editor may hold focus.
fn send(dispatch: &FocusHandle, action: &EditorAction, window: &mut Window, cx: &mut App) {
    let (focus, action) = (dispatch.clone(), action.clone());
    window.defer(cx, move |window, cx| {
        focus.dispatch_action(&action, window, cx)
    });
}

impl EventEmitter<PanelEvent> for EditorPanel {}
impl EventEmitter<EditorPanelEvent> for EditorPanel {}

impl Focusable for EditorPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.state.read(cx).focus_handle(cx)
    }
}

impl BasePanel for EditorPanel {
    fn panel_name(&self) -> &'static str {
        "EditorPanel"
    }

    /// Closing goes through `CloseEditor`, which asks about changes; see
    /// `ClosableTabTitle`.
    fn closable(&self, _: &App) -> bool {
        false
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if active {
            let focus = self.state.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
            cx.emit(EditorPanelEvent::Activated(self.id));
        }
    }

    fn on_added_to(&mut self, group: WeakEntity<TabGroup>, _: &mut Window, _: &mut Context<Self>) {
        self.tab_group = Some(group);
    }

    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.tab_group = None;
        self._task = None;
        cx.emit(EditorPanelEvent::Closed(self.id));
    }
}

impl Panel for EditorPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (id, group, panel) = (self.id, self.tab_group.clone(), cx.entity_id());
        ClosableTabTitle::new(
            ("editor-tab", id.0),
            Icon::new(CatalogIcon::FileText).small(),
            self.name(),
        )
        .modified(self.dirty)
        .closable(("close-editor", id.0), Box::new(CloseEditor(id)))
        .context_menu(move |menu, _, cx| tab_menu(menu, id, group.clone(), panel, cx))
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

/// The commands of an editor tab, shared by its context menu and the tab
/// bar's 「…」 menu.
fn tab_menu(
    menu: PopupMenu,
    id: EditorId,
    group: Option<WeakEntity<TabGroup>>,
    panel: EntityId,
    cx: &App,
) -> PopupMenu {
    let action = |command| Box::new(EditorAction::new(id, command));
    let menu = menu
        .menu_with_icon(
            "保存",
            Icon::new(CatalogIcon::Save),
            action(EditorCommand::Save),
        )
        .menu_with_icon(
            "重新加载",
            Icon::new(CatalogIcon::RefreshCw),
            action(EditorCommand::Reload),
        )
        .menu("复制路径", action(EditorCommand::CopyPath))
        .separator();
    close_tab_items(menu, CenterTab::Editor(id), group, panel, cx)
}

impl Render for EditorPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.id;
        v_flex()
            .id(("editor", id.0))
            .test_support()
            .key_context(EDITOR_CONTEXT)
            .size_full()
            .child(self.render_toolbar(cx))
            .child(
                div()
                    .id(("editor-text", id.0))
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .child(
                        Editor::new(&self.state)
                            .bordered(false)
                            .rounded_none()
                            .h_full()
                            .readonly(self.phase == Phase::Reloading)
                            .aria_label(format!("编辑 {}", self.name())),
                    ),
            )
    }
}
