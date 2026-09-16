use super::{FilePane, PaneSide};
use crate::app::ExplorerDispatch as _;
use crate::{
    app::{CatalogIcon, CloseExplorer, EditSession, ExplorerAction, ExplorerCommand},
    connection::{ConnectionPrompt, ConnectionPromptReply},
    session::{ConnectionState, SessionId, SessionStore},
    sftp::{
        RemotePath, SftpCommand, SftpEvent, SharedLocalDirectoryProvider,
        SharedSftpTransportProvider, UploadPhase, UploadProgress, UploadQuestion, UploadRequest,
    },
    shared::ClosableTabTitle,
};
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, WindowExt as _,
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
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
#[derive(Clone, Debug)]
pub enum ExplorerPanelEvent {
    Activated(SessionId),
    Closed(SessionId),
    StateChanged(SessionId),
    PromptRequested(SessionId, u64, ConnectionPrompt),
}
pub struct ExplorerPanel {
    session_id: SessionId,
    generation: u64,
    endpoint: String,
    store: Entity<SessionStore>,
    local: Entity<FilePane>,
    remote: Entity<FilePane>,
    local_provider: SharedLocalDirectoryProvider,
    commands: async_channel::Sender<SftpCommand>,
    state: ConnectionState,
    message: String,
    progress: Option<UploadProgress>,
    details: bool,
    upload_pending: bool,
    pub(super) question: Option<UploadQuestion>,
    pub(super) dialog_open: bool,
    pub(super) dispatch: FocusHandle,
    focus_handle: FocusHandle,
    tab_group: Option<WeakEntity<TabGroup>>,
    _subscriptions: Vec<Subscription>,
    _events: Task<()>,
}
impl ExplorerPanel {
    pub fn new(
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
        let local = cx.new(|cx| {
            FilePane::new(
                PaneSide::Local,
                session_id,
                home.clone(),
                dispatch.clone(),
                window,
                cx,
            )
        });
        let remote = cx.new(|cx| {
            FilePane::new(
                PaneSide::Remote,
                session_id,
                String::new(),
                dispatch.clone(),
                window,
                cx,
            )
        });
        local.update(cx, |pane, cx| {
            pane.load_local(home, local_provider.clone(), window, cx)
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
        let subscriptions = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.on_app_quit(|this, _| {
                this.send(SftpCommand::Shutdown);
                async {}
            }),
        ];
        Self {
            session_id,
            generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed),
            endpoint,
            store,
            local,
            remote,
            local_provider,
            commands,
            state: ConnectionState::Connecting,
            message: "正在连接 SFTP…".into(),
            progress: None,
            details: false,
            upload_pending: false,
            question: None,
            dialog_open: false,
            dispatch,
            focus_handle: cx.focus_handle(),
            tab_group: None,
            _subscriptions: subscriptions,
            _events: events_task,
        }
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
    pub fn connection_state(&self) -> ConnectionState {
        self.state
    }
    pub fn progress(&self) -> Option<&UploadProgress> {
        self.progress.as_ref()
    }
    pub fn is_uploading(&self) -> bool {
        self.upload_pending
            || self
                .progress
                .as_ref()
                .is_some_and(UploadProgress::is_active)
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
        self.message = "SFTP 已断开".into();
        self.remote
            .update(cx, |pane, cx| pane.disconnected("SFTP 已断开".into(), cx));
        self.close_upload_dialog(window, cx);
        self.sync_available(cx);
        cx.emit(ExplorerPanelEvent::StateChanged(self.session_id));
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
        let enabled = self.state == ConnectionState::Connected && !self.is_uploading();
        self.local
            .update(cx, |pane, cx| pane.set_available(enabled, cx));
        self.remote
            .update(cx, |pane, cx| pane.set_available(enabled, cx));
    }
    fn on_event(&mut self, event: SftpEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            SftpEvent::Connecting => {
                self.generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
                self.state = ConnectionState::Connecting;
                self.message = "正在连接 SFTP…".into();
                cx.emit(ExplorerPanelEvent::StateChanged(self.session_id));
            }
            SftpEvent::Connected { home } => {
                self.state = ConnectionState::Connected;
                self.message = self.endpoint.clone();
                self.remote
                    .update(cx, |pane, _| pane.set_home(home.to_string()));
                let path = self.remote.read(cx).path();
                self.navigate(true, path, window, cx);
                cx.emit(ExplorerPanelEvent::StateChanged(self.session_id));
            }
            SftpEvent::Disconnected(message) => {
                self.state = ConnectionState::Disconnected;
                self.remote
                    .update(cx, |pane, cx| pane.disconnected(message.clone(), cx));
                self.message = message;
                cx.emit(ExplorerPanelEvent::StateChanged(self.session_id));
            }
            SftpEvent::Prompt(prompt) => cx.emit(ExplorerPanelEvent::PromptRequested(
                self.session_id,
                self.generation,
                prompt,
            )),
            SftpEvent::Listed { request_id, result } => self.remote.update(cx, |pane, cx| {
                pane.apply_listing(request_id, result, window, cx)
            }),
            SftpEvent::Progress(progress) => {
                let complete = progress.phase() == UploadPhase::Completed;
                self.upload_pending = false;
                self.progress = Some(progress);
                if !self.is_uploading() {
                    self.close_upload_dialog(window, cx);
                }
                if complete && self.state == ConnectionState::Connected {
                    let path = self.remote.read(cx).path();
                    self.navigate(true, path, window, cx);
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
                self.upload_pending = false;
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pane = if remote {
            self.remote.clone()
        } else {
            self.local.clone()
        };
        let path = pane.read(cx).expanded_path(&path);
        if remote {
            let id = pane.update(cx, |pane, cx| pane.begin_load(cx));
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
                pane.load_local(path, self.local_provider.clone(), window, cx)
            });
        }
    }
    pub fn execute(
        &mut self,
        command: &ExplorerCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            ExplorerCommand::Navigate { remote, path } => {
                self.navigate(*remote, path.clone(), window, cx)
            }
            ExplorerCommand::Up { remote } => {
                let pane = if *remote { &self.remote } else { &self.local };
                let path = pane.read(cx).parent_path();
                self.navigate(*remote, path, window, cx);
            }
            ExplorerCommand::Refresh { remote } => {
                let pane = if *remote { &self.remote } else { &self.local };
                let path = pane.read(cx).path();
                self.navigate(*remote, path, window, cx);
            }
            ExplorerCommand::Check {
                name,
                checked,
                extend,
            } => self
                .local
                .update(cx, |pane, cx| pane.check(name, *checked, *extend, cx)),
            ExplorerCommand::ToggleSelection => {
                self.local.update(cx, |pane, cx| pane.toggle_selected(cx))
            }
            ExplorerCommand::SelectAll => self.local.update(cx, |pane, cx| pane.select_all(cx)),
            ExplorerCommand::UploadSelected => {
                let paths = self.local.read(cx).upload_sources();
                let target = self.remote.read(cx).path();
                self.open_upload(paths, target, window, cx);
            }
            ExplorerCommand::ChooseFiles => {
                if self.is_uploading() || self.state != ConnectionState::Connected {
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
                                    this.session_id,
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
                if self.is_uploading() || self.state != ConnectionState::Connected {
                    return;
                }
                match RemotePath::new(target.clone())
                    .and_then(|path| UploadRequest::new(paths.clone(), path))
                {
                    Ok(request) => {
                        self.upload_pending = true;
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
            ExplorerCommand::CancelUpload => self.send(SftpCommand::Cancel),
            ExplorerCommand::ResumeUpload => {
                if !self.is_uploading() {
                    self.upload_pending = true;
                    self.send(SftpCommand::Resume);
                }
            }
            ExplorerCommand::DiscardUpload => {
                if !self.is_uploading() {
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
    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if active {
            let id = self.session_id;
            self.store
                .update(cx, |store, cx| store.set_active(Some(id), cx));
            window.focus(&self.focus_handle, cx);
            cx.emit(ExplorerPanelEvent::Activated(id));
        }
    }
    fn on_added_to(&mut self, group: WeakEntity<TabGroup>, _: &mut Window, _: &mut Context<Self>) {
        self.tab_group = Some(group);
    }
    fn on_removed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.disconnect(window, cx);
        self.send(SftpCommand::Shutdown);
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
            .map(|s| s.name.clone())
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sid = self.session_id;
        let command_button = |id: &'static str, label: &'static str, command: ExplorerCommand| {
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
                h_flex()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .text_sm()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(div().flex_1().min_w_0().child(self.message.clone()))
                    .when(
                        self.state == ConnectionState::Disconnected && !self.is_uploading(),
                        |this| {
                            this.child(command_button(
                                "reconnect-sftp",
                                "重新连接",
                                ExplorerCommand::ResumeUpload,
                            ))
                        },
                    ),
            )
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
                let status = match progress.phase() {
                    UploadPhase::Scanning => "正在扫描",
                    UploadPhase::Uploading => "正在上传",
                    UploadPhase::Waiting => "等待处理",
                    UploadPhase::Reconnecting => "正在重连",
                    UploadPhase::Stopped => "已停止 · 可继续上传",
                    UploadPhase::Completed => {
                        if progress.failed() > 0 {
                            "上传结束 · 部分失败"
                        } else {
                            "上传结束"
                        }
                    }
                };
                let value = if progress.phase() == UploadPhase::Completed {
                    100.
                } else if progress.total_bytes() > 0 {
                    progress.completed_bytes() as f32 / progress.total_bytes() as f32 * 100.
                } else {
                    0.
                };
                this.child(
                    v_flex()
                        .id("upload-progress")
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
                                        .id("upload-status")
                                        .test_support()
                                        .text_sm()
                                        .child(status),
                                )
                                .child(div().flex_1())
                                .when(progress.is_active(), |this| {
                                    this.child(command_button(
                                        "cancel-upload",
                                        "取消",
                                        ExplorerCommand::CancelUpload,
                                    ))
                                })
                                .when(progress.phase() == UploadPhase::Stopped, |this| {
                                    this.child(command_button(
                                        "resume-upload",
                                        "继续上传",
                                        ExplorerCommand::ResumeUpload,
                                    ))
                                    .child(command_button(
                                        "discard-upload",
                                        "丢弃续传进度",
                                        ExplorerCommand::DiscardUpload,
                                    ))
                                })
                                .child(command_button(
                                    "upload-details",
                                    "详情",
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
                            Progress::new("upload-bytes")
                                .value(value)
                                .accessibility_label("上传进度")
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
                                    .id("upload-detail-list")
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
