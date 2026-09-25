use super::{ExplorerId, ExplorerPanel};
use crate::app::ExplorerDispatch as _;
use crate::{
    app::{ExplorerAction, ExplorerCommand},
    sftp::{TransferAnswer, TransferChoice, TransferDirection, TransferQuestionKind},
};
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    dialog::{DialogAction, DialogClose, DialogFooter},
    form::{Field, Form},
    h_flex,
    input::{Input, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{cell::Cell, path::PathBuf, rc::Rc};

impl ExplorerPanel {
    pub(super) fn open_upload(
        &mut self,
        paths: Vec<PathBuf>,
        target: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty()
            || self.is_transferring()
            || self.connection_state() != crate::session::ConnectionState::Connected
            || window.has_active_dialog(cx)
        {
            return;
        }
        let target_input = cx.new(|cx| InputState::new(window, cx).default_value(&target));
        let form = cx.new(|_| UploadForm {
            paths: paths.clone(),
            target: target_input.clone(),
            endpoint: self.endpoint().into(),
            error: None,
        });
        let dispatch = self.dispatch.clone();
        let sid = self.id();
        let generation = self.generation();
        let focus = window.focused(cx).unwrap_or_else(|| self.focus_handle(cx));
        let owner = cx.entity().downgrade();
        self.dialog_open = true;
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("上传文件")
                .child(form.clone())
                .overlay_closable(false)
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().trigger(|b| b.label("取消")))
                        .child(
                            DialogAction::new()
                                .child(Button::new("upload-confirm").primary().label("上传")),
                        ),
                )
                .on_ok({
                    let form = form.clone();
                    let dispatch = dispatch.clone();
                    let paths = paths.clone();
                    move |_, window, cx| {
                        let target = form.read(cx).target.read(cx).value().to_string();
                        if target.is_empty() {
                            form.update(cx, |form, cx| {
                                form.error = Some("请输入目标目录".into());
                                cx.notify();
                            });
                            return false;
                        }
                        dispatch.dispatch_explorer_action(
                            &ExplorerAction::new(
                                sid,
                                ExplorerCommand::BeginUpload {
                                    paths: paths.clone(),
                                    target,
                                },
                            )
                            .with_generation(generation),
                            window,
                            cx,
                        );
                        true
                    }
                })
                .on_close({
                    let focus = focus.clone();
                    let owner = owner.clone();
                    move |_, window, cx| {
                        let owner = owner.clone();
                        window.defer(cx, move |_, cx| {
                            let _ = owner.update(cx, |panel, _| panel.dialog_open = false);
                        });
                        window.focus(&focus, cx);
                    }
                })
        });
    }
    /// 下载: confirm the remote items and choose the local directory, which
    /// starts as the local pane's.
    pub(super) fn open_download(
        &mut self,
        paths: Vec<String>,
        target: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty()
            || self.is_transferring()
            || self.connection_state() != crate::session::ConnectionState::Connected
            || window.has_active_dialog(cx)
        {
            return;
        }
        let target_input = cx.new(|cx| InputState::new(window, cx).default_value(&target));
        let form = cx.new(|_| DownloadForm {
            paths: paths.clone(),
            target: target_input,
            endpoint: self.endpoint().into(),
            error: None,
        });
        let dispatch = self.dispatch.clone();
        let sid = self.id();
        let generation = self.generation();
        let focus = window.focused(cx).unwrap_or_else(|| self.focus_handle(cx));
        let owner = cx.entity().downgrade();
        self.dialog_open = true;
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("下载")
                .child(form.clone())
                .overlay_closable(false)
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().trigger(|b| b.label("取消")))
                        .child(
                            DialogAction::new()
                                .child(Button::new("download-confirm").primary().label("下载")),
                        ),
                )
                .on_ok({
                    let form = form.clone();
                    let dispatch = dispatch.clone();
                    let paths = paths.clone();
                    move |_, window, cx| {
                        let target = form.read(cx).target.read(cx).value().to_string();
                        if !std::path::Path::new(&target).is_absolute() {
                            form.update(cx, |form, cx| {
                                form.error = Some("请输入本机目录的完整路径".into());
                                cx.notify();
                            });
                            return false;
                        }
                        dispatch.dispatch_explorer_action(
                            &ExplorerAction::new(
                                sid,
                                ExplorerCommand::BeginDownload {
                                    paths: paths.clone(),
                                    target,
                                },
                            )
                            .with_generation(generation),
                            window,
                            cx,
                        );
                        true
                    }
                })
                .on_close({
                    let focus = focus.clone();
                    let owner = owner.clone();
                    move |_, window, cx| {
                        let owner = owner.clone();
                        window.defer(cx, move |_, cx| {
                            let _ = owner.update(cx, |panel, _| panel.dialog_open = false);
                        });
                        window.focus(&focus, cx);
                    }
                })
        });
    }
    pub(super) fn open_question(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(question) = self.question.clone() else {
            return;
        };
        let form = cx.new(|_| ConflictForm {
            apply_all: false,
            kind: question.kind(),
        });
        let answered = Rc::new(Cell::new(false));
        let dispatch = self.dispatch.clone();
        let sid = self.id();
        let generation = self.generation();
        let focus = window.focused(cx).unwrap_or_else(|| self.focus_handle(cx));
        self.dialog_open = true;
        let verb = self.transfer_direction().verb();
        let (title, primary_label, primary_choice): (SharedString, SharedString, _) =
            match question.kind() {
                TransferQuestionKind::Conflict => (
                    "同名文件已存在".into(),
                    "覆盖".into(),
                    TransferChoice::Overwrite,
                ),
                TransferQuestionKind::Resume => (
                    format!("发现未完成的{verb}").into(),
                    format!("继续{verb}").into(),
                    TransferChoice::Resume,
                ),
                TransferQuestionKind::InvalidResume => (
                    "无法继续此文件".into(),
                    format!("重新{verb}").into(),
                    TransferChoice::Restart,
                ),
                TransferQuestionKind::Error => (
                    format!("无法{verb}此项目").into(),
                    "重试".into(),
                    TransferChoice::Retry,
                ),
            };
        let cancel_label: SharedString = format!("取消{verb}").into();
        let restart_label: SharedString = format!("重新{verb}").into();
        window.open_dialog(cx, move |dialog, _, _| {
            let answer_button = |id: &'static str, label: SharedString, choice: TransferChoice| {
                let answered = answered.clone();
                let dispatch = dispatch.clone();
                let form = form.clone();
                let request_id = question.id();
                Button::new(id).label(label).on_click(move |_, window, cx| {
                    answered.set(true);
                    let all = form.read(cx).apply_all;
                    dispatch.dispatch_explorer_action(
                        &ExplorerAction::new(
                            sid,
                            ExplorerCommand::Answer {
                                request_id,
                                answer: TransferAnswer::new(choice, all),
                            },
                        )
                        .with_generation(generation),
                        window,
                        cx,
                    );
                    window.close_dialog(cx);
                })
            };
            dialog
                .title(title.clone())
                .overlay_closable(false)
                .child(
                    v_flex()
                        .gap_3()
                        .child(div().text_sm().child(question.path().to_string()))
                        .child(div().text_sm().child(question.message().to_string()))
                        .child(form.clone()),
                )
                .footer(
                    DialogFooter::new()
                        .child(answer_button(
                            "transfer-question-cancel",
                            cancel_label.clone(),
                            TransferChoice::Cancel,
                        ))
                        .child(answer_button(
                            "transfer-question-skip",
                            "跳过".into(),
                            TransferChoice::Skip,
                        ))
                        .when(question.kind() == TransferQuestionKind::Resume, |footer| {
                            footer.child(answer_button(
                                "transfer-question-restart",
                                restart_label.clone(),
                                TransferChoice::Restart,
                            ))
                        })
                        .child(
                            DialogAction::new().child(
                                Button::new("transfer-question-confirm")
                                    .primary()
                                    .label(primary_label.clone()),
                            ),
                        ),
                )
                .on_ok({
                    let answered = answered.clone();
                    let dispatch = dispatch.clone();
                    let form = form.clone();
                    let request_id = question.id();
                    move |_, window, cx| {
                        answered.set(true);
                        let all = form.read(cx).apply_all;
                        dispatch.dispatch_explorer_action(
                            &ExplorerAction::new(
                                sid,
                                ExplorerCommand::Answer {
                                    request_id,
                                    answer: TransferAnswer::new(primary_choice, all),
                                },
                            )
                            .with_generation(generation),
                            window,
                            cx,
                        );
                        true
                    }
                })
                .on_close({
                    let answered = answered.clone();
                    let dispatch = dispatch.clone();
                    let request_id = question.id();
                    let focus = focus.clone();
                    move |_, window, cx| {
                        if !answered.replace(true) {
                            dispatch.dispatch_explorer_action(
                                &ExplorerAction::new(
                                    sid,
                                    ExplorerCommand::Answer {
                                        request_id,
                                        answer: TransferAnswer::new(TransferChoice::Cancel, false),
                                    },
                                )
                                .with_generation(generation),
                                window,
                                cx,
                            );
                        }
                        window.focus(&focus, cx);
                    }
                })
        });
    }
}
struct UploadForm {
    paths: Vec<PathBuf>,
    target: Entity<InputState>,
    endpoint: String,
    error: Option<String>,
}
impl Render for UploadForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .child(div().text_sm().child(format!(
                "上传 {} 个项目到 {}",
                self.paths.len(),
                self.endpoint
            )))
            .child(
                div()
                    .id("upload-source-list")
                    .max_h_32()
                    .overflow_y_scroll()
                    .text_sm()
                    .children(
                        self.paths
                            .iter()
                            .map(|p| div().child(p.display().to_string())),
                    ),
            )
            .child(
                Form::new().child(
                    Field::new()
                        .label("目标目录")
                        .required(true)
                        .child(Input::new(&self.target).id("upload-target").small()),
                ),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .id("upload-form-error")
                        .test_support()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
    }
}
struct DownloadForm {
    paths: Vec<String>,
    target: Entity<InputState>,
    endpoint: String,
    error: Option<String>,
}
impl DownloadForm {
    fn browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let choice = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择下载到的目录".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = choice.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.target.update(cx, |input, cx| {
                        input.set_value(path.to_string_lossy().into_owned(), window, cx)
                    });
                    this.error = None;
                    cx.notify();
                });
            }
        })
        .detach();
    }
}
impl Render for DownloadForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .child(div().text_sm().child(format!(
                "从 {} 下载 {} 个项目",
                self.endpoint,
                self.paths.len()
            )))
            .child(
                div()
                    .id("download-source-list")
                    .max_h_32()
                    .overflow_y_scroll()
                    .text_sm()
                    .children(self.paths.iter().map(|p| div().child(p.clone()))),
            )
            .child(
                Form::new().child(
                    Field::new().label("下载到").required(true).child(
                        h_flex()
                            .gap_2()
                            .child(
                                Input::new(&self.target)
                                    .id("download-target")
                                    .small()
                                    .flex_1(),
                            )
                            .child(
                                Button::new("download-browse")
                                    .small()
                                    .label("浏览…")
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.browse(window, cx)),
                                    ),
                            ),
                    ),
                ),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .id("download-form-error")
                        .test_support()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
    }
}
struct ConflictForm {
    apply_all: bool,
    kind: TransferQuestionKind,
}
impl Render for ConflictForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().when(self.kind == TransferQuestionKind::Conflict, |this| {
            this.child(
                Checkbox::new("transfer-apply-all")
                    .label("应用于本批剩余冲突")
                    .checked(self.apply_all)
                    .on_change(cx.listener(|this, value, _, cx| {
                        this.apply_all = *value;
                        cx.notify();
                    })),
            )
        })
    }
}
/// Session-scoped close confirmation dispatches through the same workspace handler.
pub fn confirm_close_transfer(
    explorer: ExplorerId,
    generation: u64,
    direction: TransferDirection,
    dispatch: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let focus = window.focused(cx);
    let verb = direction.verb();
    window.open_alert_dialog(cx, move |dialog, _, _| {
        dialog
            .title(format!("停止{verb}并关闭？"))
            .description(format!(
                "{verb}进度会保留，下次选择相同来源和目标目录时可以继续{verb}。"
            ))
            .button_props(
                gpui_kit::component::dialog::DialogButtonProps::default()
                    .ok_text(format!("停止{verb}并关闭"))
                    .cancel_text(format!("继续{verb}")),
            )
            .show_cancel(true)
            .on_cancel({
                let focus = focus.clone();
                move |_, window, cx| {
                    if let Some(focus) = &focus {
                        window.focus(focus, cx);
                    }
                    true
                }
            })
            .on_ok({
                let dispatch = dispatch.clone();
                move |_, window, cx| {
                    dispatch.dispatch_explorer_action(
                        &ExplorerAction::new(explorer, ExplorerCommand::CloseConfirmed)
                            .with_generation(generation),
                        window,
                        cx,
                    );
                    true
                }
            })
    });
}
