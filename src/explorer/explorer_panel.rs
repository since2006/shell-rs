use super::{
    ExplorerId, FilePane, LoadIntent, PaneSide,
    pane_operations::{PaneOperation, PendingOperation},
};
use crate::app::ExplorerDispatch as _;
use crate::{
    app::{CatalogIcon, CenterTab, CloseExplorer, ExplorerAction, ExplorerCommand, RenameExplorer},
    connection::{ConnectionPrompt, ConnectionPromptReply},
    session::{BookmarkSide, ConnectionState, SessionId, SessionStore},
    sftp::{
        DownloadRequest, RemotePath, SftpCommand, SftpEvent, SharedLocalDirectoryProvider,
        SharedSftpTransportProvider, TransferDirection, TransferPhase, TransferProgress,
        TransferQuestion, UploadRequest,
    },
    shared::{ClosableTabTitle, HostMark, RenamableTab, close_tab_items},
};
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dock::{BasePanel, Panel, PanelEvent, TabGroup},
    h_flex,
    menu::PopupMenu,
    notification::Notification,
    progress::Progress,
    resizable::{h_resizable, resizable_panel},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
#[derive(Clone, Debug)]
pub enum ExplorerPanelEvent {
    Activated(ExplorerId),
    Closed(ExplorerId, SessionId),
    StateChanged(ExplorerId, SessionId),
    PromptRequested(ExplorerId, SessionId, u64, ConnectionPrompt),
}
pub struct ExplorerPanel {
    id: ExplorerId,
    session_id: SessionId,
    generation: u64,
    endpoint: String,
    pub(super) store: Entity<SessionStore>,
    local: Entity<FilePane>,
    pub(super) remote: Entity<FilePane>,
    pub(super) local_provider: SharedLocalDirectoryProvider,
    /// File operations waiting for their result, by request id.
    pub(super) operations: HashMap<u64, PendingOperation>,
    pub(super) next_operation: u64,
    commands: async_channel::Sender<SftpCommand>,
    state: ConnectionState,
    progress: Option<TransferProgress>,
    details: bool,
    transfer_pending: bool,
    pub(super) question: Option<TransferQuestion>,
    pub(super) dialog_open: bool,
    pub(super) dispatch: FocusHandle,
    focus_handle: FocusHandle,
    tab_group: Option<WeakEntity<TabGroup>>,
    /// A title the user gave this tab. Lives as long as the tab, like the
    /// rest of the layout.
    custom_title: Option<SharedString>,
    /// The pane that held focus last, which takes it back on activation.
    last_remote: bool,
    _subscriptions: Vec<Subscription>,
    _events: Task<()>,
}
impl ExplorerPanel {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: ExplorerId,
        session_id: SessionId,
        store: Entity<SessionStore>,
        provider: SharedSftpTransportProvider,
        local_provider: SharedLocalDirectoryProvider,
        dispatch: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let session = store
            .read(cx)
            .session(session_id)
            .cloned()
            .expect("workspace checked session");
        let endpoint = format!("{}@{}:{}", session.user, session.host, session.port);
        let home = local_provider.home().to_string_lossy().into_owned();
        let places = local_provider
            .places()
            .into_iter()
            .map(|(title, path)| (title.into(), path.to_string_lossy().into_owned()))
            .collect();
        let local = cx.new(|cx| {
            FilePane::new(
                PaneSide::Local,
                id,
                session_id,
                home.clone(),
                places,
                store.clone(),
                dispatch.clone(),
                window,
                cx,
            )
        });
        let remote = cx.new(|cx| {
            FilePane::new(
                PaneSide::Remote,
                id,
                session_id,
                String::new(),
                Vec::new(),
                store.clone(),
                dispatch.clone(),
                window,
                cx,
            )
        });
        local.update(cx, |pane, cx| {
            pane.load_local(home, LoadIntent::Reload, local_provider.clone(), window, cx)
        });
        let (commands, receiver) = async_channel::unbounded();
        let (sender, events) = async_channel::unbounded();
        let transport = provider.create(&session);
        let failed = sender.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("shellr-sftp".into())
            .spawn(move || {
                if let Err(error) = transport.run(receiver, sender.clone()) {
                    let _ = sender.send_blocking(SftpEvent::Disconnected(error.to_string()));
                }
            })
        {
            let _ = failed.try_send(SftpEvent::Disconnected(format!("无法启动 SFTP：{error}")));
        }
        // Poll snapshots as the terminal engine does. Worker threads never wake
        // a foreground-only GPUI task directly.
        let events_task = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                let pending: Vec<_> = (0..128).map_while(|_| events.try_recv().ok()).collect();
                if !pending.is_empty()
                    && this
                        .update_in(cx, |this, window, cx| {
                            for event in pending {
                                this.on_event(event, window, cx);
                            }
                        })
                        .is_err()
                {
                    break;
                }
                if events.is_closed() && events.is_empty() {
                    break;
                }
            }
        });
        let local_focus = local.read(cx).focus_handle(cx);
        let remote_focus = remote.read(cx).focus_handle(cx);
        let subscriptions = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            // Focus listeners run inside a draw, where a view's `notify` is
            // dropped without asking for another frame: a click on empty list
            // space would switch panes unseen. Deferring runs it after the draw.
            cx.on_focus_in(&local_focus, window, |_, window, cx| {
                cx.defer_in(window, |this, _, cx| this.set_current_pane(false, cx))
            }),
            cx.on_focus_in(&remote_focus, window, |_, window, cx| {
                cx.defer_in(window, |this, _, cx| this.set_current_pane(true, cx))
            }),
            cx.on_app_quit(|this, _| {
                this.send(SftpCommand::Shutdown);
                async {}
            }),
        ];
        Self {
            id,
            session_id,
            generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed),
            endpoint,
            store,
            local,
            remote,
            local_provider,
            operations: HashMap::new(),
            next_operation: 0,
            commands,
            state: ConnectionState::Connecting,
            progress: None,
            details: false,
            transfer_pending: false,
            question: None,
            dialog_open: false,
            dispatch,
            focus_handle: cx.focus_handle(),
            tab_group: None,
            custom_title: None,
            last_remote: true,
            _subscriptions: subscriptions,
            _events: events_task,
        }
    }
    pub fn id(&self) -> ExplorerId {
        self.id
    }
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn endpoint(&self) -> &str {
        &self.endpoint
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
    pub fn pane(&self, remote: bool) -> &Entity<FilePane> {
        if remote { &self.remote } else { &self.local }
    }
    /// The pane whose file list holds keyboard focus.
    pub fn focused_pane(&self, window: &Window, cx: &App) -> Option<bool> {
        [false, true].into_iter().find(|remote| {
            self.pane(*remote)
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx)
        })
    }
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus_handle.contains_focused(window, cx)
    }
    pub fn connection_state(&self) -> ConnectionState {
        self.state
    }
    pub fn progress(&self) -> Option<&TransferProgress> {
        self.progress.as_ref()
    }
    /// The direction of the current or last batch; the next one is upload
    /// until a download starts.
    pub fn transfer_direction(&self) -> TransferDirection {
        self.progress
            .as_ref()
            .map(TransferProgress::direction)
            .unwrap_or_default()
    }
    pub fn is_transferring(&self) -> bool {
        self.transfer_pending
            || self
                .progress
                .as_ref()
                .is_some_and(TransferProgress::is_active)
    }
    pub fn send(&self, command: SftpCommand) {
        let _ = self.commands.try_send(command);
    }
    pub fn reply_to_prompt(&self, request_id: u64, reply: ConnectionPromptReply) {
        self.send(SftpCommand::PromptReply { request_id, reply });
    }
    pub fn disconnect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.send(SftpCommand::Disconnect);
        self.state = ConnectionState::Disconnected;
        self.remote
            .update(cx, |pane, cx| pane.disconnected("SFTP 已断开".into(), cx));
        self.close_upload_dialog(window, cx);
        self.sync_available(cx);
        cx.emit(ExplorerPanelEvent::StateChanged(self.id, self.session_id));
        cx.notify();
    }
    pub fn close_upload_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog_open {
            self.dialog_open = false;
            self.question = None;
            if window.has_active_dialog(cx) {
                window.close_dialog(cx);
            }
        }
    }
    fn sync_available(&mut self, cx: &mut Context<Self>) {
        let state = self.state;
        let transfer = state == ConnectionState::Connected && !self.is_transferring();
        // A stopped transfer offers its own 继续 instead.
        let reconnect = state == ConnectionState::Disconnected && !self.is_transferring();
        self.local.update(cx, |pane, cx| {
            pane.set_available(ConnectionState::Connected, transfer, false, cx)
        });
        self.remote.update(cx, |pane, cx| {
            pane.set_available(state, transfer, reconnect, cx)
        });
    }
    /// WinSCP's current pane: the one used last, whose path label stands out
    /// and which takes focus back when the tab is shown again.
    fn set_current_pane(&mut self, remote: bool, cx: &mut Context<Self>) {
        self.last_remote = remote;
        self.local
            .update(cx, |pane, cx| pane.set_current(!remote, cx));
        self.remote
            .update(cx, |pane, cx| pane.set_current(remote, cx));
    }
    fn on_event(&mut self, event: SftpEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            SftpEvent::Connecting => {
                self.generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
                self.state = ConnectionState::Connecting;
                cx.emit(ExplorerPanelEvent::StateChanged(self.id, self.session_id));
            }
            SftpEvent::Connected { home } => {
                self.state = ConnectionState::Connected;
                self.remote
                    .update(cx, |pane, _| pane.set_home(home.to_string()));
                let path = self.remote.read(cx).path();
                self.navigate(true, path, LoadIntent::Reload, window, cx);
                cx.emit(ExplorerPanelEvent::StateChanged(self.id, self.session_id));
            }
            SftpEvent::Disconnected(message) => {
                self.state = ConnectionState::Disconnected;
                self.abandon_remote_operations(cx);
                self.remote
                    .update(cx, |pane, cx| pane.disconnected(message, cx));
                cx.emit(ExplorerPanelEvent::StateChanged(self.id, self.session_id));
            }
            SftpEvent::Prompt(prompt) => cx.emit(ExplorerPanelEvent::PromptRequested(
                self.id,
                self.session_id,
                self.generation,
                prompt,
            )),
            SftpEvent::Listed { request_id, result } => self.remote.update(cx, |pane, cx| {
                pane.apply_listing(request_id, result, window, cx)
            }),
            SftpEvent::Operated { request_id, result } => {
                self.finish_operation(request_id, result, window, cx)
            }
            SftpEvent::Progress(progress) => {
                let complete = progress.phase() == TransferPhase::Completed;
                let direction = progress.direction();
                self.transfer_pending = false;
                self.progress = Some(progress);
                if !self.is_transferring() {
                    self.close_upload_dialog(window, cx);
                }
                // Show what arrived: the remote pane after an upload, the local
                // one after a download.
                if complete {
                    match direction {
                        TransferDirection::Download => self.reload(false, window, cx),
                        TransferDirection::Upload if self.state == ConnectionState::Connected => {
                            self.reload(true, window, cx)
                        }
                        TransferDirection::Upload => {}
                    }
                }
            }
            SftpEvent::Question(question) => {
                self.question = Some(question);
                cx.spawn_in(window, async move |this, cx| {
                    loop {
                        let done = this
                            .update_in(cx, |this, window, cx| {
                                if this.question.is_none() {
                                    return true;
                                }
                                if window.has_active_dialog(cx) {
                                    return false;
                                }
                                this.open_question(window, cx);
                                true
                            })
                            .unwrap_or(true);
                        if done {
                            break;
                        }
                        cx.background_executor()
                            .timer(Duration::from_millis(50))
                            .await;
                    }
                })
                .detach();
            }
            SftpEvent::Idle => {
                self.transfer_pending = false;
            }
            SftpEvent::Notice(message) => {
                window.push_notification(Notification::error(message), cx);
            }
        }
        self.sync_available(cx);
        cx.notify();
    }
    pub fn navigate(
        &mut self,
        remote: bool,
        path: String,
        intent: LoadIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Nothing would answer; the pane would wait forever.
        if remote && self.state != ConnectionState::Connected {
            return;
        }
        let pane = self.pane(remote).clone();
        let path = pane.read(cx).expanded_path(&path);
        if remote {
            let id = pane.update(cx, |pane, cx| pane.begin_load(intent, cx));
            match RemotePath::new(path) {
                Ok(path) => self.send(SftpCommand::List {
                    request_id: id,
                    path,
                }),
                Err(e) => pane.update(cx, |pane, cx| {
                    pane.apply_listing(id, Err(e.to_string()), window, cx)
                }),
            }
        } else {
            pane.update(cx, |pane, cx| {
                pane.load_local(path, intent, self.local_provider.clone(), window, cx)
            });
        }
    }
    /// Re-read a pane's current directory, keeping its history.
    pub fn reload(&mut self, remote: bool, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.pane(remote).read(cx).path();
        self.navigate(remote, path, LoadIntent::Reload, window, cx);
    }
    pub fn execute(
        &mut self,
        command: &ExplorerCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            ExplorerCommand::Navigate { remote, path } => {
                self.navigate(*remote, path.clone(), LoadIntent::Visit, window, cx)
            }
            ExplorerCommand::Up { remote } => {
                let path = self.pane(*remote).read(cx).parent_path();
                self.navigate(*remote, path, LoadIntent::Visit, window, cx);
            }
            ExplorerCommand::Root { remote } => {
                let path = self.pane(*remote).read(cx).root_path();
                self.navigate(*remote, path, LoadIntent::Visit, window, cx);
            }
            ExplorerCommand::Home { remote } => {
                let path = self.pane(*remote).read(cx).home();
                if !path.is_empty() {
                    self.navigate(*remote, path, LoadIntent::Visit, window, cx);
                }
            }
            ExplorerCommand::Back { remote } => {
                if let Some(path) = self.pane(*remote).read(cx).back_target() {
                    self.navigate(*remote, path, LoadIntent::Back, window, cx);
                }
            }
            ExplorerCommand::Forward { remote } => {
                if let Some(path) = self.pane(*remote).read(cx).forward_target() {
                    self.navigate(*remote, path, LoadIntent::Forward, window, cx);
                }
            }
            ExplorerCommand::Refresh { remote } => self.reload(*remote, window, cx),
            ExplorerCommand::AddBookmark { remote, path } => {
                let path = path
                    .clone()
                    .unwrap_or_else(|| self.pane(*remote).read(cx).path());
                let (id, side) = (self.session_id, BookmarkSide::from_remote(*remote));
                self.store
                    .update(cx, |store, cx| store.add_bookmark(id, side, &path, cx));
            }
            ExplorerCommand::RemoveBookmark { remote, path } => {
                let (id, side) = (self.session_id, BookmarkSide::from_remote(*remote));
                self.store
                    .update(cx, |store, cx| store.remove_bookmark(id, side, path, cx));
            }
            ExplorerCommand::MoveBookmark { remote, path, to } => {
                let (id, side) = (self.session_id, BookmarkSide::from_remote(*remote));
                self.store
                    .update(cx, |store, cx| store.move_bookmark(id, side, path, *to, cx));
            }
            ExplorerCommand::Open { remote } => {
                let pane = self.pane(*remote).clone();
                if let Some(entry) = pane.read(cx).cursor_entry(cx)
                    && entry.is_dir()
                {
                    let path = if entry.is_parent() {
                        pane.read(cx).parent_path()
                    } else {
                        pane.read(cx).child_path_of(&entry.name)
                    };
                    self.navigate(*remote, path, LoadIntent::Visit, window, cx);
                }
            }
            ExplorerCommand::OpenDirectory { remote } => {
                self.set_current_pane(*remote, cx);
                self.open_directory(*remote, window, cx);
            }
            ExplorerCommand::CopyPath { remote } => {
                let path = self.pane(*remote).read(cx).path();
                if !path.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(path));
                }
            }
            ExplorerCommand::FocusPane { remote } => {
                let focus = self.pane(*remote).read(cx).focus_handle(cx);
                window.focus(&focus, cx);
            }
            ExplorerCommand::MoveCursor {
                remote,
                motion,
                extend,
            } => self
                .pane(*remote)
                .clone()
                .update(cx, |pane, cx| pane.move_cursor(*motion, *extend, cx)),
            ExplorerCommand::ToggleSelection { remote } => self
                .pane(*remote)
                .clone()
                .update(cx, |pane, cx| pane.toggle_selection(cx)),
            ExplorerCommand::SelectAll { remote } => self
                .pane(*remote)
                .clone()
                .update(cx, |pane, cx| pane.select_all(cx)),
            ExplorerCommand::Transfer { remote: false } => {
                let paths = self.local.read(cx).upload_sources(cx);
                let target = self.remote.read(cx).path();
                self.open_upload(paths, target, window, cx);
            }
            ExplorerCommand::Transfer { remote: true } => {
                let remote = self.remote.read(cx);
                let paths = remote
                    .selected_names(cx)
                    .iter()
                    .map(|name| remote.child_path_of(name))
                    .collect();
                let target = self.local.read(cx).path();
                self.open_download(paths, target, window, cx);
            }
            ExplorerCommand::DownloadPaths { paths, target } => {
                self.open_download(paths.clone(), target.clone(), window, cx)
            }
            ExplorerCommand::BeginDownload { paths, target } => {
                if self.is_transferring() || self.state != ConnectionState::Connected {
                    return;
                }
                let request = paths
                    .iter()
                    .map(|path| RemotePath::new(path.clone()))
                    .collect::<anyhow::Result<Vec<_>>>()
                    .and_then(|paths| DownloadRequest::new(paths, target.into()));
                match request {
                    Ok(request) => {
                        self.transfer_pending = true;
                        self.details = false;
                        self.send(SftpCommand::Download(request));
                    }
                    Err(error) => {
                        window.push_notification(Notification::error(error.to_string()), cx)
                    }
                }
            }
            ExplorerCommand::Delete { remote } => self.confirm_delete(*remote, window, cx),
            ExplorerCommand::BeginDelete { remote, names } => {
                self.start_operation(*remote, PaneOperation::Delete(names.clone()), window, cx)
            }
            ExplorerCommand::Rename { remote } => self.open_rename(*remote, window, cx),
            ExplorerCommand::CommitRename { remote, from, to } => self.start_operation(
                *remote,
                PaneOperation::Rename {
                    from: from.clone(),
                    to: to.clone(),
                },
                window,
                cx,
            ),
            ExplorerCommand::New { remote, kind } => self.open_new(*remote, *kind, window, cx),
            ExplorerCommand::CommitNew { remote, kind, name } => self.start_operation(
                *remote,
                PaneOperation::Create {
                    kind: *kind,
                    name: name.clone(),
                },
                window,
                cx,
            ),
            ExplorerCommand::Properties { remote } => self.open_properties(*remote, window, cx),
            ExplorerCommand::ApplyPermissions {
                remote,
                names,
                edit,
                recursive,
                add_x_to_dirs,
            } => self.start_operation(
                *remote,
                PaneOperation::Permissions {
                    names: names.clone(),
                    edit: *edit,
                    recursive: *recursive,
                    add_x_to_dirs: *add_x_to_dirs,
                },
                window,
                cx,
            ),
            ExplorerCommand::ChooseFiles => {
                if self.is_transferring() || self.state != ConnectionState::Connected {
                    return;
                }
                let choice = cx.prompt_for_paths(PathPromptOptions {
                    files: true,
                    directories: true,
                    multiple: true,
                    prompt: Some("选择要上传的文件或目录".into()),
                });
                let target = self.remote.read(cx).path();
                cx.spawn_in(window, async move |this, cx| {
                    if let Ok(Ok(Some(paths))) = choice.await {
                        let _ = this.update_in(cx, |this, window, cx| {
                            this.dispatch.dispatch_explorer_action(
                                &ExplorerAction::new(
                                    this.id,
                                    ExplorerCommand::UploadPaths { paths, target },
                                ),
                                window,
                                cx,
                            );
                        });
                    }
                })
                .detach();
            }
            ExplorerCommand::UploadPaths { paths, target } => {
                self.open_upload(paths.clone(), target.clone(), window, cx)
            }
            ExplorerCommand::BeginUpload { paths, target } => {
                if self.is_transferring() || self.state != ConnectionState::Connected {
                    return;
                }
                match RemotePath::new(target.clone())
                    .and_then(|path| UploadRequest::new(paths.clone(), path))
                {
                    Ok(request) => {
                        self.transfer_pending = true;
                        self.details = false;
                        self.send(SftpCommand::Upload(request));
                    }
                    Err(error) => {
                        window.push_notification(Notification::error(error.to_string()), cx)
                    }
                }
            }
            ExplorerCommand::Answer { request_id, answer } => {
                if self
                    .question
                    .as_ref()
                    .is_some_and(|q| q.id() == *request_id)
                {
                    self.question = None;
                    self.dialog_open = false;
                    self.send(SftpCommand::Answer {
                        request_id: *request_id,
                        answer: *answer,
                    });
                }
            }
            ExplorerCommand::CancelTransfer => self.send(SftpCommand::Cancel),
            ExplorerCommand::ResumeTransfer => {
                if !self.is_transferring() {
                    self.transfer_pending = true;
                    self.send(SftpCommand::Resume);
                }
            }
            ExplorerCommand::DiscardTransfer => {
                if !self.is_transferring() {
                    self.send(SftpCommand::Discard);
                }
            }
            ExplorerCommand::ToggleDetails => self.details = !self.details,
            ExplorerCommand::CloseConfirmed => {}
        }
        self.sync_available(cx);
        cx.notify();
    }
}
impl Drop for ExplorerPanel {
    fn drop(&mut self) {
        self.send(SftpCommand::Shutdown);
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
    /// Closing goes through `CloseExplorer`, which asks first while an upload
    /// runs. The dock's own 「关闭」 would skip that question.
    fn closable(&self, _: &App) -> bool {
        false
    }
    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if active {
            let session_id = self.session_id;
            self.store
                .update(cx, |store, cx| store.set_active(Some(session_id), cx));
            // Focus a file list, not the panel root, so list shortcuts work
            // without a click first. Both stay inside this panel.
            let focus = self.pane(self.last_remote).read(cx).focus_handle(cx);
            window.focus(&focus, cx);
            cx.emit(ExplorerPanelEvent::Activated(self.id));
        }
    }
    fn on_added_to(&mut self, group: WeakEntity<TabGroup>, _: &mut Window, _: &mut Context<Self>) {
        self.tab_group = Some(group);
    }
    fn on_removed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.disconnect(window, cx);
        self.send(SftpCommand::Shutdown);
        self.tab_group = None;
        cx.emit(ExplorerPanelEvent::Closed(self.id, self.session_id));
    }
}
impl Panel for ExplorerPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let session = self.store.read(cx).session(self.session_id);
        let name = session
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "SFTP".into());
        let os = session.and_then(|s| s.os);
        let (id, group, panel) = (self.id, self.tab_group.clone(), cx.entity_id());
        // The host's mark, as on the session's terminal tabs.
        let mark = HostMark::new(("explorer-tab-os", id.0), name, os).small();
        ClosableTabTitle::new(("explorer-tab", id.0), mark, self.tab_title(cx))
            .closable(("close-explorer", id.0), Box::new(CloseExplorer(id)))
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
impl RenamableTab for ExplorerPanel {
    fn default_title(&self, cx: &App) -> SharedString {
        let name = self
            .store
            .read(cx)
            .session(self.session_id)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "SFTP".into());
        format!("{name} · SFTP").into()
    }
    fn tab_title(&self, cx: &App) -> SharedString {
        self.custom_title
            .clone()
            .unwrap_or_else(|| self.default_title(cx))
    }
    fn set_custom_title(&mut self, title: Option<SharedString>, cx: &mut Context<Self>) {
        self.custom_title = title;
        cx.notify();
    }
}
/// The commands of an SFTP tab, shared by its context menu and the tab bar's
/// 「…」 menu.
fn tab_menu(
    menu: PopupMenu,
    id: ExplorerId,
    group: Option<WeakEntity<TabGroup>>,
    panel: EntityId,
    cx: &App,
) -> PopupMenu {
    let menu = menu
        .menu_with_icon(
            "重命名标签…",
            Icon::new(CatalogIcon::Pencil),
            Box::new(RenameExplorer(id)),
        )
        .separator();
    close_tab_items(menu, CenterTab::Explorer(id), group, panel, cx)
}
impl Render for ExplorerPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sid = self.id;
        let command_button = |id: &'static str, label: SharedString, command: ExplorerCommand| {
            let dispatch = self.dispatch.clone();
            Button::new(id)
                .ghost()
                .small()
                .label(label)
                .on_click(move |_, w, cx| {
                    dispatch.dispatch_explorer_action(
                        &ExplorerAction::new(sid, command.clone()),
                        w,
                        cx,
                    )
                })
        };
        v_flex()
            .id(("explorer", sid.0))
            .test_support()
            .track_focus(&self.focus_handle)
            .size_full()
            .min_w_0()
            .child(
                div().flex_1().min_h_0().child(
                    h_resizable(("explorer-panes", sid.0))
                        .child(
                            resizable_panel()
                                .size(px(480.))
                                .size_range(px(320.)..Pixels::MAX)
                                .child(self.local.clone()),
                        )
                        .child(
                            resizable_panel()
                                .size_range(px(280.)..Pixels::MAX)
                                .child(self.remote.clone()),
                        ),
                ),
            )
            .when_some(self.progress.as_ref(), |this, progress| {
                let verb = progress.direction().verb();
                let status = match progress.phase() {
                    TransferPhase::Scanning => "正在扫描".to_string(),
                    TransferPhase::Transferring => format!("正在{verb}"),
                    TransferPhase::Waiting => "等待处理".to_string(),
                    TransferPhase::Reconnecting => "正在重连".to_string(),
                    TransferPhase::Stopped => format!("已停止 · 可继续{verb}"),
                    TransferPhase::Completed => {
                        if progress.failed() > 0 {
                            format!("{verb}结束 · 部分失败")
                        } else {
                            format!("{verb}结束")
                        }
                    }
                };
                let value = if progress.phase() == TransferPhase::Completed {
                    100.
                } else if progress.total_bytes() > 0 {
                    progress.completed_bytes() as f32 / progress.total_bytes() as f32 * 100.
                } else {
                    0.
                };
                this.child(
                    v_flex()
                        .id("transfer-progress")
                        .test_support()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    div()
                                        .id("transfer-status")
                                        .test_support()
                                        .text_sm()
                                        .child(status),
                                )
                                .child(div().flex_1())
                                .when(progress.is_active(), |this| {
                                    this.child(command_button(
                                        "cancel-transfer",
                                        "取消".into(),
                                        ExplorerCommand::CancelTransfer,
                                    ))
                                })
                                .when(progress.phase() == TransferPhase::Stopped, |this| {
                                    this.child(command_button(
                                        "resume-transfer",
                                        format!("继续{verb}").into(),
                                        ExplorerCommand::ResumeTransfer,
                                    ))
                                    .child(command_button(
                                        "discard-transfer",
                                        "丢弃续传进度".into(),
                                        ExplorerCommand::DiscardTransfer,
                                    ))
                                })
                                .child(command_button(
                                    "transfer-details",
                                    "详情".into(),
                                    ExplorerCommand::ToggleDetails,
                                )),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_ellipsis()
                                .child(progress.current().to_string()),
                        )
                        .child(
                            Progress::new("transfer-bytes")
                                .value(value)
                                .accessibility_label(format!("{verb}进度"))
                                .small(),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!(
                                    "{} / {} · {}/秒 · 成功 {} · 跳过 {} · 失败 {}",
                                    super::format_size(progress.completed_bytes()),
                                    super::format_size(progress.total_bytes()),
                                    super::format_size(progress.bytes_per_second()),
                                    progress.succeeded(),
                                    progress.skipped(),
                                    progress.failed()
                                )),
                        )
                        .when(self.details, |this| {
                            this.child(
                                div()
                                    .id("transfer-detail-list")
                                    .max_h_32()
                                    .overflow_y_scroll()
                                    .text_xs()
                                    .children(
                                        progress
                                            .details()
                                            .iter()
                                            .map(|detail| div().child(detail.clone())),
                                    ),
                            )
                        }),
                )
            })
    }
}
