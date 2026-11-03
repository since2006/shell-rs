use super::{
    ExplorerId, FileLocation, FilePane, FilePaneEvent, LoadIntent, PaneSide, QueueId, QueueState,
    Removal, TransferJob, TransferQueue,
    file_edit::{Opening, PendingFile},
    find_window::{FIND_REQUESTS, FindWindow},
    pane_operations::PendingOperation,
    preview::PreviewKind,
};
use crate::app::ExplorerDispatch as _;
use crate::{
    app::{
        CatalogIcon, CenterTab, CloseExplorer, CopyHostAddress, ExplorerAction, ExplorerCommand,
        OpenExplorer, RenameExplorer,
    },
    connection::{ConnectionPrompt, ConnectionPromptReply, Latency},
    host::{BookmarkSide, ConnectionState, Host, HostId, HostLogin, HostStore},
    i18n::t,
    sftp::{
        DownloadRequest, RemotePath, SftpCommand, SftpEvent, SftpRoute,
        SharedLocalDirectoryProvider, SharedSftpTransportProvider, TextFile, TransferDirection,
        TransferPhase, TransferQuestion, UploadRequest, describe_sftp_error,
    },
    shared::{ClosableTabTitle, HostMark, LatencyLabel, RenamableTab, close_tab_items},
};
use gpui_kit::component::{
    Disableable as _, Icon, IconName, Sizable as _, WindowExt as _,
    button::Button,
    dialog::DialogButtonProps,
    dock::{BasePanel, Panel, PanelEvent, TabGroup},
    menu::PopupMenu,
    notification::Notification,
    resizable::{ResizableState, h_resizable, resizable_panel, v_resizable},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
#[derive(Clone, Debug)]
pub enum ExplorerPanelEvent {
    Activated(ExplorerId),
    Closed(ExplorerId, HostId),
    StateChanged(ExplorerId, HostId),
    /// What the window's status line says for this tab changed without the
    /// connection changing: a directory could not be read, or can again.
    StatusChanged(ExplorerId),
    PromptRequested(ExplorerId, HostId, u64, ConnectionPrompt),
    /// A file was read for the editor and can be edited.
    EditFile(ExplorerId, FileLocation, Rc<TextFile>),
}
pub struct ExplorerPanel {
    id: ExplorerId,
    host_id: HostId,
    /// The host as it was when the tab opened, which it shows; the worker
    /// logs in, and in again, as it was then too. Editing or deleting the
    /// host leaves the tab alone.
    host: Host,
    generation: u64,
    endpoint: String,
    pub(super) store: Entity<HostStore>,
    local: Entity<FilePane>,
    pub(super) remote: Entity<FilePane>,
    pub(super) local_provider: SharedLocalDirectoryProvider,
    /// File operations waiting for their result, by request id.
    pub(super) operations: HashMap<u64, PendingOperation>,
    pub(super) next_operation: u64,
    /// Reads and writes for the editor waiting for the worker, by request
    /// id; the files being read for a new editor tab.
    pub(super) files: HashMap<u64, PendingFile>,
    pub(super) opening: HashSet<FileLocation>,
    /// 查找文件's window, while it is open, and the name it last searched
    /// for, which the next one starts with.
    pub(super) find: Option<FindWindow>,
    pub(super) find_mask: String,
    pub(super) commands: async_channel::Sender<SftpCommand>,
    state: ConnectionState,
    /// The connection's latest round trip; none until the first one after
    /// connecting comes back.
    latency: Option<Latency>,
    /// Batches waiting, running and ended, WinSCP's queue. The engine runs
    /// one at a time and the panel hands it the next.
    pub(super) queue: TransferQueue,
    /// The engine is working on a command: a batch, a resume or a discard.
    /// Cleared by its `Idle`, which is when the next batch may go.
    pub(super) engine_busy: bool,
    pub(super) question: Option<TransferQuestion>,
    /// A transfer question is on screen, as opposed to a dialog of the
    /// user's own such as the confirmation of the next upload.
    pub(super) question_shown: bool,
    pub(super) dialog_open: bool,
    pub(super) dispatch: FocusHandle,
    focus_handle: FocusHandle,
    tab_group: Option<WeakEntity<TabGroup>>,
    /// A title the user gave this tab. Lives as long as the tab, like the
    /// rest of the layout.
    custom_title: Option<SharedString>,
    /// The pane that held focus last, which takes it back on activation.
    last_remote: bool,
    /// The right sidebar's 文件 for a remote terminal, on the terminal's
    /// connection: the remote pane alone, with no tab of its own. The local
    /// pane is never shown, nor listed; it only holds where downloads go.
    sidebar: bool,
    /// The sidebar's 目录跟随: it goes where the terminal's shell reports
    /// it is, as `follow_directory` hears of it.
    follow: bool,
    /// A folder to go to once connected, reported while it was not.
    follow_target: Option<String>,
    /// Where the user put the divider between the panes, and the one above
    /// the queue. Kept here, not in element state, which goes the first
    /// frame another tab is shown instead: coming back would reset them.
    panes_split: Entity<ResizableState>,
    queue_split: Entity<ResizableState>,
    _subscriptions: Vec<Subscription>,
    _events: Task<()>,
}
impl ExplorerPanel {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: ExplorerId,
        host: Host,
        login: HostLogin,
        store: Entity<HostStore>,
        provider: SharedSftpTransportProvider,
        route: SftpRoute,
        local_provider: SharedLocalDirectoryProvider,
        dispatch: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let host_id = host.id;
        let endpoint = login.endpoint();
        let sidebar = matches!(route, SftpRoute::Terminal(_));
        let home = if sidebar {
            local_provider.downloads()
        } else {
            local_provider.home()
        };
        let home = home.to_string_lossy().into_owned();
        let local = cx.new(|cx| {
            FilePane::new(
                PaneSide::Local,
                id,
                host_id,
                home.clone(),
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
                host_id,
                String::new(),
                store.clone(),
                dispatch.clone(),
                window,
                cx,
            )
        });
        // Listing a folder such as Downloads makes macOS ask for access,
        // which a pane nobody sees has no business doing.
        if sidebar {
            remote.update(cx, |pane, cx| pane.set_compact(cx));
        } else {
            local.update(cx, |pane, cx| {
                pane.load_local(home, LoadIntent::Reload, local_provider.clone(), window, cx)
            });
        }
        let (commands, receiver) = async_channel::unbounded();
        let (sender, events) = async_channel::unbounded();
        let transport = provider.create(&login, route);
        let failed = sender.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("shellrs-sftp".into())
            .spawn(move || {
                if let Err(error) = transport.run(receiver, sender.clone()) {
                    let _ =
                        sender.send_blocking(SftpEvent::Disconnected(describe_sftp_error(&error)));
                }
            })
        {
            let _ = failed.try_send(SftpEvent::Disconnected(
                t!("explorer.connection.start_failed", error = error).to_string(),
            ));
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
        let problem_changed = |this: &mut Self, _, _: &FilePaneEvent, cx: &mut Context<Self>| {
            cx.emit(ExplorerPanelEvent::StatusChanged(this.id))
        };
        let subscriptions = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.subscribe(&local, problem_changed),
            cx.subscribe(&remote, problem_changed),
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
            // The main window closed, and the workspace with it.
            cx.on_release(|this, cx| this.close_find(cx)),
        ];
        Self {
            id,
            host_id,
            host,
            generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed),
            endpoint,
            store,
            local,
            remote,
            local_provider,
            operations: HashMap::new(),
            next_operation: 0,
            files: HashMap::new(),
            opening: HashSet::new(),
            find: None,
            find_mask: String::new(),
            commands,
            state: ConnectionState::Connecting,
            latency: None,
            queue: TransferQueue::default(),
            engine_busy: false,
            question: None,
            question_shown: false,
            dialog_open: false,
            dispatch,
            focus_handle: cx.focus_handle(),
            tab_group: None,
            custom_title: None,
            last_remote: true,
            sidebar,
            follow: false,
            follow_target: None,
            panes_split: cx.new(|_| ResizableState::default()),
            queue_split: cx.new(|_| ResizableState::default()),
            _subscriptions: subscriptions,
            _events: events_task,
        }
    }
    pub fn id(&self) -> ExplorerId {
        self.id
    }
    pub fn host_id(&self) -> HostId {
        self.host_id
    }
    /// The host as the tab took it (see the field).
    pub fn host(&self) -> &Host {
        &self.host
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
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus_handle.contains_focused(window, cx)
    }
    pub fn connection_state(&self) -> ConnectionState {
        self.state
    }
    /// The right sidebar's 文件 rather than an SFTP tab.
    pub fn is_sidebar(&self) -> bool {
        self.sidebar
    }
    /// 目录跟随 is on.
    pub fn follows(&self) -> bool {
        self.follow
    }
    /// Turn 目录跟随 on or off, which its switch in the status line shows.
    pub fn set_follow(&mut self, follow: bool, cx: &mut Context<Self>) {
        self.follow = follow;
        if !follow {
            self.follow_target = None;
        }
        self.remote
            .update(cx, |pane, cx| pane.set_follow(follow, cx));
        cx.notify();
    }
    /// The terminal connected again: what it reported before is history.
    pub fn forget_follow_target(&mut self) {
        self.follow_target = None;
    }
    /// Go where the terminal's shell reports it is, when following: now if
    /// connected, else once connected. It never connects just for this.
    pub fn follow_directory(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.follow {
            return;
        }
        if self.state != ConnectionState::Connected {
            self.follow_target = Some(path);
        } else if self.remote.read(cx).path() != path {
            self.navigate(true, path, LoadIntent::Visit, window, cx);
        }
    }
    /// What the window's status line shows for this tab: its own connection,
    /// and what went wrong, remote side first.
    pub fn status(&self, cx: &App) -> ExplorerStatus {
        let problem = [&self.remote, &self.local]
            .into_iter()
            .find_map(|pane| pane.read(cx).problem().map(SharedString::from));
        ExplorerStatus {
            state: self.state,
            problem,
        }
    }
    /// 重新连接 is there unless it is under way, or a batch runs or waits
    /// stopped (that one goes on with its own 继续).
    pub fn can_reconnect(&self) -> bool {
        self.state != ConnectionState::Connecting && !self.engine_busy && !self.head_stopped()
    }
    /// Tell the user the remote side is disconnected, and why, with 重新连接
    /// right there. When the queue owns reconnecting (a stopped batch waits
    /// for 继续, or a transfer is reconnecting by itself), say so instead.
    pub(super) fn offer_reconnect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        let name = self.host.name.clone();
        let reason = self
            .remote
            .read(cx)
            .problem()
            .map(SharedString::from)
            .unwrap_or_else(|| t!("explorer.connection.lost"));
        let can_reconnect = self.can_reconnect();
        let description = if can_reconnect {
            t!("explorer.connection.lost_reconnect", reason = reason)
        } else if self.head_stopped() {
            t!("explorer.connection.lost_stopped_batch", reason = reason)
        } else {
            t!("explorer.connection.lost_reconnecting", reason = reason)
        };
        let sender = self.sender();
        let focus = window.focused(cx);
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let restore = {
                let focus = focus.clone();
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    if let Some(focus) = &focus {
                        window.focus(focus, cx);
                    }
                    true
                }
            };
            let dialog = dialog
                .title(t!("explorer.connection.lost_title", name = name))
                .description(description.clone());
            if can_reconnect {
                dialog
                    .button_props(
                        DialogButtonProps::default()
                            .ok_text(t!("explorer.connection.reconnect"))
                            .cancel_text(t!("common.cancel")),
                    )
                    .show_cancel(true)
                    .on_cancel(restore.clone())
                    .on_ok({
                        let (sender, restore) = (sender.clone(), restore.clone());
                        move |event, window, cx| {
                            sender.send(ExplorerCommand::Reconnect, window, cx);
                            restore(event, window, cx)
                        }
                    })
            } else {
                dialog
                    .button_props(DialogButtonProps::default().ok_text(t!("explorer.ok")))
                    .on_ok(restore)
            }
        });
    }
    /// What this tab's dialogs send their commands through.
    pub(super) fn sender(&self) -> ExplorerSender {
        ExplorerSender {
            dispatch: self.dispatch.clone(),
            explorer: self.id,
            generation: self.generation,
        }
    }
    /// The direction of the batch at the head, or the next one to go.
    pub fn transfer_direction(&self) -> TransferDirection {
        self.queue.direction().unwrap_or_default()
    }
    /// Anything not yet done: running, stopped or waiting its turn.
    pub fn is_transferring(&self) -> bool {
        self.queue.unfinished_count() > 0
    }
    /// A stopped batch holds the head, waiting for 继续.
    fn head_stopped(&self) -> bool {
        self.queue
            .head()
            .is_some_and(|head| head.state() == QueueState::Stopped)
    }
    /// Hand the engine the next batch if it is free for one. A batch that
    /// cannot be made into a request ends there and the next one goes.
    fn start_next_if_idle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        while !self.engine_busy && self.state == ConnectionState::Connected {
            let Some((_, job)) = self.queue.start_next() else {
                return;
            };
            match transfer_command(&job) {
                Ok(command) => {
                    self.engine_busy = true;
                    self.send(command);
                }
                Err(error) => {
                    let message = describe_sftp_error(&error);
                    self.queue
                        .abandon_head(Removal::NotStarted(message.clone()));
                    window.push_notification(Notification::error(message), cx);
                }
            }
        }
    }
    /// Put a confirmed batch on the queue, and start it if nothing runs.
    fn enqueue(&mut self, job: TransferJob, window: &mut Window, cx: &mut Context<Self>) {
        if self.state != ConnectionState::Connected {
            return;
        }
        // Refused now rather than when its turn comes.
        if let Err(error) = transfer_command(&job) {
            window.push_notification(Notification::error(describe_sftp_error(&error)), cx);
            return;
        }
        self.queue.push(job);
        self.start_next_if_idle(window, cx);
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
        self.remote.update(cx, |pane, cx| {
            pane.disconnected(t!("explorer.connection.lost").to_string(), cx)
        });
        self.close_transfer_dialog(window, cx);
        self.sync_available(cx);
        cx.emit(ExplorerPanelEvent::StateChanged(self.id, self.host_id));
        cx.notify();
    }
    /// Close this tab's transfer confirmation or question, if one is open.
    fn close_transfer_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog_open {
            self.dialog_open = false;
            self.question = None;
            self.question_shown = false;
            if window.has_active_dialog(cx) {
                window.close_dialog(cx);
            }
        }
    }
    fn sync_available(&mut self, cx: &mut Context<Self>) {
        let state = self.state;
        // A batch confirmed while another runs waits its turn in the queue.
        // Disconnected, transfers are still asked for, and answered with the
        // offer to reconnect; only while connecting do they wait.
        let transfer = state != ConnectionState::Connecting;
        self.local.update(cx, |pane, cx| {
            pane.set_available(ConnectionState::Connected, transfer, cx)
        });
        self.remote
            .update(cx, |pane, cx| pane.set_available(state, transfer, cx));
    }
    /// WinSCP's current pane: the one used last, which takes focus back when
    /// the tab is shown again.
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
                self.latency = None;
                cx.emit(ExplorerPanelEvent::StateChanged(self.id, self.host_id));
            }
            SftpEvent::Connected { home } => {
                self.state = ConnectionState::Connected;
                let first = self.remote.read(cx).home().is_empty();
                self.remote
                    .update(cx, |pane, _| pane.set_home(home.to_string()));
                // Where the terminal is, if it said while this connected;
                // the first time without passing home in the history.
                match self.follow_target.take() {
                    Some(target) => {
                        let intent = if first {
                            LoadIntent::Reload
                        } else {
                            LoadIntent::Visit
                        };
                        self.navigate(true, target, intent, window, cx);
                    }
                    None => self.reload(true, window, cx),
                }
                cx.emit(ExplorerPanelEvent::StateChanged(self.id, self.host_id));
            }
            SftpEvent::Disconnected(message) => {
                self.state = ConnectionState::Disconnected;
                self.latency = None;
                self.abandon_remote_operations(cx);
                self.find_connection_lost(cx);
                self.remote
                    .update(cx, |pane, cx| pane.disconnected(message, cx));
                cx.emit(ExplorerPanelEvent::StateChanged(self.id, self.host_id));
            }
            SftpEvent::Prompt(prompt) => cx.emit(ExplorerPanelEvent::PromptRequested(
                self.id,
                self.host_id,
                self.generation,
                prompt,
            )),
            // The search's listings, and an answer to one stopped since.
            SftpEvent::Listed { request_id, result } if request_id >= FIND_REQUESTS => {
                self.find_listed(request_id, result, cx)
            }
            SftpEvent::Listed { request_id, result } => self.remote.update(cx, |pane, cx| {
                pane.apply_listing(request_id, result, window, cx)
            }),
            SftpEvent::Operated { request_id, result } => {
                self.finish_operation(request_id, result, window, cx)
            }
            SftpEvent::FileRead { request_id, result } => self.finish_file_read(request_id, result),
            SftpEvent::FileWritten { request_id, result } => {
                self.finish_file_write(request_id, result)
            }
            SftpEvent::BytesRead { request_id, result } => {
                self.finish_bytes_read(request_id, result)
            }
            SftpEvent::Progress(progress) => {
                let complete = progress.phase() == TransferPhase::Completed;
                let direction = progress.direction();
                self.queue.on_progress(progress);
                // A question outlives nothing: once the batch stops or ends,
                // one on screen goes, and one not shown yet never will be.
                let running = self
                    .queue
                    .head()
                    .is_some_and(|head| head.state() == QueueState::Active);
                if !running {
                    if self.question_shown {
                        self.close_transfer_dialog(window, cx);
                    }
                    self.question = None;
                }
                // Show what arrived: the remote pane after an upload, the local
                // one after a download.
                if complete {
                    self.reload(direction == TransferDirection::Upload, window, cx);
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
                self.engine_busy = false;
                self.start_next_if_idle(window, cx);
            }
            SftpEvent::Notice(message) => {
                window.push_notification(Notification::error(message), cx);
            }
            SftpEvent::Latency(latency) => self.latency = Some(latency),
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
        // The sidebar's local pane is only a download target.
        if !remote && self.sidebar {
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
                    pane.apply_listing(id, Err(describe_sftp_error(&e)), window, cx)
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
        // Disconnected, a command for the remote side says why nothing
        // happens and offers to reconnect, rather than doing nothing.
        if self.state == ConnectionState::Disconnected && needs_connection(command) {
            self.offer_reconnect(window, cx);
            return;
        }
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
                let (id, side) = (self.host_id, BookmarkSide::from_remote(*remote));
                self.store
                    .update(cx, |store, cx| store.add_bookmark(id, side, &path, cx));
            }
            ExplorerCommand::RemoveBookmark { remote, path } => {
                let (id, side) = (self.host_id, BookmarkSide::from_remote(*remote));
                self.store
                    .update(cx, |store, cx| store.remove_bookmark(id, side, path, cx));
            }
            ExplorerCommand::MoveBookmark { remote, path, to } => {
                let (id, side) = (self.host_id, BookmarkSide::from_remote(*remote));
                self.store
                    .update(cx, |store, cx| store.move_bookmark(id, side, path, *to, cx));
            }
            ExplorerCommand::Open { remote } => {
                let pane = self.pane(*remote).clone();
                let Some(entry) = pane.read(cx).cursor_entry(cx) else {
                    return;
                };
                if entry.is_dir() {
                    let path = if entry.is_parent() {
                        pane.read(cx).parent_path()
                    } else {
                        pane.read(cx).child_path_of(&entry.name)
                    };
                    self.navigate(*remote, path, LoadIntent::Visit, window, cx);
                } else {
                    // A picture is previewed; any other file (an SVG too,
                    // being text) opens in the editor, through the workspace,
                    // which shows the tab when the file is open already.
                    let path = Some(pane.read(cx).child_path_of(&entry.name));
                    let remote = *remote;
                    let command =
                        if PreviewKind::of(&entry.name).is_some_and(|kind| kind.preview_only()) {
                            ExplorerCommand::Preview { remote, path }
                        } else {
                            ExplorerCommand::Edit { remote, path }
                        };
                    self.dispatch.dispatch_explorer_action(
                        &ExplorerAction::new(self.id, command),
                        window,
                        cx,
                    );
                }
            }
            ExplorerCommand::Edit { remote, path } => {
                if let Some(location) = self.edit_target(*remote, path.as_deref(), cx) {
                    self.open_file(location, Opening::Edit, window, cx);
                }
            }
            ExplorerCommand::Preview { remote, path } => {
                if let Some((location, kind)) = self.preview_target(*remote, path.as_deref(), cx) {
                    self.open_file(location, Opening::Preview(kind), window, cx);
                }
            }
            ExplorerCommand::OpenDirectory { remote } => {
                self.set_current_pane(*remote, cx);
                self.open_directory(*remote, window, cx);
            }
            // Finding lives in SFTP tabs: its 定位 needs a pane on screen.
            ExplorerCommand::FindFiles | ExplorerCommand::Reveal { .. } if self.sidebar => {}
            ExplorerCommand::FindFiles => {
                self.set_current_pane(true, cx);
                self.open_find(window, cx);
            }
            ExplorerCommand::Reveal { path } => {
                let (directory, name) = match path.rsplit_once('/') {
                    Some(("", name)) => ("/".to_string(), name),
                    Some((directory, name)) => (directory.to_string(), name),
                    None => return,
                };
                if self.state == ConnectionState::Connected {
                    self.remote
                        .update(cx, |pane, _| pane.select_after_load(name.to_string()));
                    self.navigate(true, directory, LoadIntent::Visit, window, cx);
                }
                let focus = self.remote.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
            }
            ExplorerCommand::CopyPath { remote } => {
                let path = self.pane(*remote).read(cx).path();
                if !path.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(path));
                }
            }
            ExplorerCommand::CopySelectedPaths { remote } => {
                let pane = self.pane(*remote).read(cx);
                let paths: Vec<String> = pane
                    .selected_entries(cx)
                    .iter()
                    .map(|entry| {
                        if entry.is_parent() {
                            pane.parent_path()
                        } else {
                            pane.child_path_of(&entry.name)
                        }
                    })
                    .collect();
                if !paths.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(paths.join("\n")));
                }
            }
            ExplorerCommand::FocusPane { remote } => {
                let focus = self.pane(*remote || self.sidebar).read(cx).focus_handle(cx);
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
            ExplorerCommand::Enqueue(job) => self.enqueue(job.clone(), window, cx),
            ExplorerCommand::Delete { remote } => self.confirm_delete(*remote, window, cx),
            ExplorerCommand::DeletePaths { paths } => self.delete_paths(paths.clone(), window, cx),
            ExplorerCommand::Rename { remote } => self.open_rename(*remote, window, cx),
            ExplorerCommand::New { remote, kind } => self.open_new(*remote, *kind, window, cx),
            ExplorerCommand::Properties { remote } => self.open_properties(*remote, window, cx),
            ExplorerCommand::Operate { remote, operation } => {
                self.start_operation(*remote, operation.clone(), window, cx)
            }
            ExplorerCommand::ChooseFiles => {
                if self.state != ConnectionState::Connected {
                    return;
                }
                // Windows and Linux dialogs pick files or folders, never
                // both, and asking for folders hides the files (#60); there
                // folders upload by dragging them in.
                let choice = cx.prompt_for_paths(PathPromptOptions {
                    files: true,
                    directories: cx.can_select_mixed_files_and_dirs(),
                    multiple: true,
                    prompt: Some(t!("explorer.choose_upload")),
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
            ExplorerCommand::Answer { request_id, answer } => {
                if self
                    .question
                    .as_ref()
                    .is_some_and(|q| q.id() == *request_id)
                {
                    self.question = None;
                    self.question_shown = false;
                    self.dialog_open = false;
                    self.send(SftpCommand::Answer {
                        request_id: *request_id,
                        answer: *answer,
                    });
                }
            }
            ExplorerCommand::CancelTransfer => self.send(SftpCommand::Cancel),
            // Goes on with a stopped head; with none, it only reconnects,
            // and the queue moves on once that is done.
            ExplorerCommand::ResumeTransfer => {
                if !self.engine_busy {
                    self.engine_busy = true;
                    self.send(SftpCommand::Resume);
                }
            }
            ExplorerCommand::Reconnect => {
                if self.can_reconnect() {
                    if self.state == ConnectionState::Connected {
                        self.disconnect(window, cx);
                    }
                    // With no stopped batch, 继续 is a bare reconnect.
                    self.engine_busy = true;
                    self.send(SftpCommand::Resume);
                }
            }
            ExplorerCommand::DiscardTransfer => {
                if !self.engine_busy && self.head_stopped() {
                    self.queue.abandon_head(Removal::Discarded);
                    self.engine_busy = true;
                    self.send(SftpCommand::Discard);
                }
            }
            ExplorerCommand::SelectQueueEntry { id } => self.queue.select(QueueId(*id)),
            ExplorerCommand::ToggleQueueEntry { id } => self.queue.toggle_expanded(QueueId(*id)),
            ExplorerCommand::RemoveQueueEntry => {
                if let Some(id) = self.queue.removable()
                    && self.queue.remove(id)
                {
                    self.start_next_if_idle(window, cx);
                }
            }
            ExplorerCommand::ClearFinishedTransfers => self.queue.clear_finished(),
            ExplorerCommand::CloseConfirmed => {}
        }
        self.sync_available(cx);
        cx.notify();
    }
}
impl ExplorerPanel {
    /// End this explorer: its connection, its find window, and then its
    /// place in the workspace, which the `Closed` event clears. An SFTP tab
    /// does this as the dock removes it; the sidebar's 文件, which is in no
    /// dock, when its terminal goes.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_find(cx);
        self.disconnect(window, cx);
        self.send(SftpCommand::Shutdown);
        cx.emit(ExplorerPanelEvent::Closed(self.id, self.host_id));
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
    /// Closing goes through `CloseExplorer`, which asks first while a transfer
    /// runs. The dock's own 「关闭」 would skip that question.
    fn closable(&self, _: &App) -> bool {
        false
    }
    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if active {
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
        self.tab_group = None;
        self.close(window, cx);
    }
}
impl Panel for ExplorerPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (name, address, os) = (
            self.host.name.clone(),
            self.host.address.clone(),
            self.host.os,
        );
        let id = self.id;
        let menu = self.tab_menu(cx);
        let state = self.connection_state();
        // The host's mark, as on the host's terminal tabs.
        let mark = HostMark::new(("explorer-tab-os", id.0), name, address, os)
            .connection_state(state)
            .small();
        ClosableTabTitle::new(("explorer-tab", id.0), mark, self.tab_title(cx))
            .connection_state(state)
            .closable(("close-explorer", id.0), Box::new(CloseExplorer(id)))
            .context_menu(move |popup, _, cx| menu.build(popup, cx))
    }
    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        self.tab_menu(cx).build(menu, cx)
    }
    /// The connection's latest round trip, as on a terminal tab. Shown only
    /// while connected, so a dropped connection leaves no stale number.
    fn title_suffix(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<impl IntoElement> {
        let latency = self
            .latency
            .filter(|_| self.state == ConnectionState::Connected)?;
        Some(LatencyLabel::new(("sftp-latency", self.id.0), latency))
    }
    /// A terminal tab's buttons that apply here too.
    fn toolbar_buttons(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<Vec<Button>> {
        let (id, host_id) = (self.id, self.host_id);
        Some(vec![
            Button::new(("open-sftp", id.0))
                .icon(Icon::new(CatalogIcon::FolderTree))
                .label("SFTP")
                .tooltip(t!("explorer.tab.open_sftp_tip"))
                .on_click(move |_, window, cx| {
                    window.dispatch_action(Box::new(OpenExplorer(host_id)), cx)
                }),
            Button::new(("reconnect-sftp", id.0))
                .icon(Icon::new(CatalogIcon::RefreshCw))
                .tooltip(t!("explorer.connection.reconnect"))
                .disabled(!self.can_reconnect())
                .on_click(move |_, window, cx| {
                    window.dispatch_action(
                        Box::new(ExplorerAction::new(id, ExplorerCommand::Reconnect)),
                        cx,
                    )
                }),
        ])
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}
impl RenamableTab for ExplorerPanel {
    fn default_title(&self, _: &App) -> SharedString {
        format!("{} · SFTP", self.host.name).into()
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
/// Whether a command needs the SFTP connection: whatever reads or changes
/// the remote side, and every transfer. Selecting, bookmarks, copying a
/// path and the queue's own buttons do not.
fn needs_connection(command: &ExplorerCommand) -> bool {
    use ExplorerCommand as C;
    match command {
        C::Navigate { remote, .. }
        | C::Up { remote }
        | C::Refresh { remote }
        | C::Root { remote }
        | C::Home { remote }
        | C::Back { remote }
        | C::Forward { remote }
        | C::Open { remote }
        | C::Edit { remote, .. }
        | C::Preview { remote, .. }
        | C::OpenDirectory { remote }
        | C::Delete { remote }
        | C::Rename { remote }
        | C::New { remote, .. }
        | C::Properties { remote }
        | C::Operate { remote, .. } => *remote,
        C::FindFiles
        | C::Reveal { .. }
        | C::DeletePaths { .. }
        | C::Transfer { .. }
        | C::DownloadPaths { .. }
        | C::ChooseFiles
        | C::UploadPaths { .. }
        | C::Enqueue(_) => true,
        _ => false,
    }
}

/// Sends an SFTP tab's commands from its dialogs. They go through the
/// workspace stamped with the connection the dialog was opened on, so a
/// dialog that outlives that connection does nothing.
#[derive(Clone)]
pub(super) struct ExplorerSender {
    dispatch: FocusHandle,
    explorer: ExplorerId,
    generation: u64,
}

impl ExplorerSender {
    pub(super) fn send(&self, command: ExplorerCommand, window: &mut Window, cx: &mut App) {
        self.dispatch.dispatch_explorer_action(
            &ExplorerAction::new(self.explorer, command).with_generation(self.generation),
            window,
            cx,
        );
    }
}

/// The status of an SFTP tab for the window's status line.
pub struct ExplorerStatus {
    pub state: ConnectionState,
    /// What went wrong reading a directory, or why the connection dropped.
    pub problem: Option<SharedString>,
}

/// The commands of an SFTP tab, as a snapshot taken while the tab renders.
/// Its context menu and the tab bar's 「…」 menu both build from it, so the
/// two always list the same commands.
#[derive(Clone)]
struct TabMenu {
    id: ExplorerId,
    host_id: HostId,
    host_is_ip: bool,
    /// The host is still there to open SFTP on and copy the address of.
    host_exists: bool,
    can_reconnect: bool,
    group: Option<WeakEntity<TabGroup>>,
    panel: EntityId,
}

impl ExplorerPanel {
    fn tab_menu(&self, cx: &Context<Self>) -> TabMenu {
        TabMenu {
            id: self.id,
            host_id: self.host_id,
            host_is_ip: self.host.address_is_ip(),
            host_exists: self.store.read(cx).host(self.host_id).is_some(),
            can_reconnect: self.can_reconnect(),
            group: self.tab_group.clone(),
            panel: cx.entity_id(),
        }
    }
}

impl TabMenu {
    /// A terminal tab's commands that apply to an SFTP tab, then closing.
    fn build(&self, menu: PopupMenu, cx: &App) -> PopupMenu {
        let (id, host_id) = (self.id, self.host_id);
        let copy_host = if self.host_is_ip {
            t!("explorer.tab.copy_ip")
        } else {
            t!("explorer.tab.copy_host_name")
        };
        let menu = menu
            .menu_with_icon(
                t!("explorer.tab.rename"),
                Icon::new(CatalogIcon::Pencil),
                Box::new(RenameExplorer(id)),
            )
            .when(self.host_exists, |menu| {
                menu.menu_with_icon(
                    t!("explorer.tab.open_sftp"),
                    Icon::new(CatalogIcon::FolderTree),
                    Box::new(OpenExplorer(host_id)),
                )
                .menu_with_icon(
                    copy_host,
                    Icon::new(IconName::Copy),
                    Box::new(CopyHostAddress(host_id)),
                )
            })
            .menu_with_icon_and_disabled(
                t!("explorer.connection.reconnect"),
                Icon::new(CatalogIcon::RefreshCw),
                Box::new(ExplorerAction::new(id, ExplorerCommand::Reconnect)),
                !self.can_reconnect,
            )
            .separator();
        close_tab_items(
            menu,
            CenterTab::Explorer(id),
            self.group.clone(),
            self.panel,
            cx,
        )
    }
}
impl Render for ExplorerPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sid = self.id;
        let panes = if self.sidebar {
            self.remote.clone().into_any_element()
        } else {
            h_resizable(("explorer-panes", sid.0))
                .with_state(&self.panes_split)
                .child(
                    // Half and half when the tab opens; the state then keeps
                    // where the user puts the divider.
                    resizable_panel()
                        .flex_basis(relative(0.5))
                        .size_range(px(320.)..Pixels::MAX)
                        .child(self.local.clone()),
                )
                .child(
                    resizable_panel()
                        .flex_basis(relative(0.5))
                        .size_range(px(280.)..Pixels::MAX)
                        .child(self.remote.clone()),
                )
                .into_any_element()
        };
        // The queue always has its place, shown only with something in it,
        // so the panes keep their split when it comes and goes.
        v_flex()
            .id(("explorer", sid.0))
            .test_support()
            .track_focus(&self.focus_handle)
            .size_full()
            .min_w_0()
            .child(
                div().flex_1().min_h_0().child(
                    v_resizable(("explorer-queue", sid.0))
                        .with_state(&self.queue_split)
                        .child(resizable_panel().child(panes))
                        .child(
                            resizable_panel()
                                .size(px(180.))
                                .size_range(px(96.)..px(480.))
                                .visible(!self.queue.is_empty())
                                .child(self.render_queue(cx)),
                        ),
                ),
            )
    }
}

/// The engine's command for a queued batch; fails for paths it cannot take.
fn transfer_command(job: &TransferJob) -> anyhow::Result<SftpCommand> {
    match job {
        TransferJob::Upload { paths, target } => RemotePath::new(target.clone())
            .and_then(|target| UploadRequest::new(paths.clone(), target))
            .map(SftpCommand::Upload),
        TransferJob::Download { paths, target } => paths
            .iter()
            .map(|path| RemotePath::new(path.clone()))
            .collect::<anyhow::Result<Vec<_>>>()
            .and_then(|paths| DownloadRequest::new(paths, target.into()))
            .map(SftpCommand::Download),
    }
}

#[cfg(test)]
mod tests {
    use super::needs_connection;
    use crate::app::ExplorerCommand;

    #[test]
    fn remote_side_and_transfers_need_the_connection() {
        assert!(needs_connection(&ExplorerCommand::Refresh { remote: true }));
        assert!(!needs_connection(&ExplorerCommand::Refresh {
            remote: false
        }));
        assert!(needs_connection(&ExplorerCommand::Delete { remote: true }));
        assert!(!needs_connection(&ExplorerCommand::Delete {
            remote: false
        }));
        // Either way a transfer goes over it.
        assert!(needs_connection(&ExplorerCommand::Transfer {
            remote: false
        }));
        assert!(needs_connection(&ExplorerCommand::ChooseFiles));
        // Choosing, bookmarks and the queue do not.
        assert!(!needs_connection(&ExplorerCommand::SelectAll {
            remote: true
        }));
        assert!(!needs_connection(&ExplorerCommand::CopyPath {
            remote: true
        }));
        assert!(!needs_connection(&ExplorerCommand::CopySelectedPaths {
            remote: true
        }));
        assert!(!needs_connection(&ExplorerCommand::AddBookmark {
            remote: true,
            path: None
        }));
        assert!(!needs_connection(&ExplorerCommand::Reconnect));
    }
}
