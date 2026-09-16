use super::ExplorerPanel;
use crate::app::ExplorerDispatch as _;
use crate::{
    app::{ExplorerAction, ExplorerCommand},
    session::SessionId,
    sftp::{UploadAnswer, UploadChoice, UploadQuestionKind},
};
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    dialog::{DialogAction, DialogClose, DialogFooter},
    form::{Field, Form},
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
            || self.is_uploading()
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
        let sid = self.session_id();
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
        let sid = self.session_id();
        let generation = self.generation();
        let focus = window.focused(cx).unwrap_or_else(|| self.focus_handle(cx));
        self.dialog_open = true;
        let (title, primary_label, primary_choice) = match question.kind() {
            UploadQuestionKind::Conflict => ("同名文件已存在", "覆盖", UploadChoice::Overwrite),
            UploadQuestionKind::Resume => ("发现未完成的上传", "继续上传", UploadChoice::Resume),
            UploadQuestionKind::InvalidResume => {
                ("无法继续此文件", "重新上传", UploadChoice::Restart)
            }
            UploadQuestionKind::Error => ("无法上传此项目", "重试", UploadChoice::Retry),
        };
        window.open_dialog(cx, move |dialog, _, _| {
            let answer_button = |id: &'static str, label: &'static str, choice: UploadChoice| {
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
                                answer: UploadAnswer::new(choice, all),
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
                .title(title)
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
                            "upload-question-cancel",
                            "取消上传",
                            UploadChoice::Cancel,
                        ))
                        .child(answer_button(
                            "upload-question-skip",
                            "跳过",
                            UploadChoice::Skip,
                        ))
                        .when(question.kind() == UploadQuestionKind::Resume, |footer| {
                            footer.child(answer_button(
                                "upload-question-restart",
                                "重新上传",
                                UploadChoice::Restart,
                            ))
                        })
                        .child(
                            DialogAction::new().child(
                                Button::new("upload-question-confirm")
                                    .primary()
                                    .label(primary_label),
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
                                    answer: UploadAnswer::new(primary_choice, all),
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
                                        answer: UploadAnswer::new(UploadChoice::Cancel, false),
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
struct ConflictForm {
    apply_all: bool,
    kind: UploadQuestionKind,
}
impl Render for ConflictForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().when(self.kind == UploadQuestionKind::Conflict, |this| {
            this.child(
                Checkbox::new("upload-apply-all")
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
pub fn confirm_close_upload(
    session: SessionId,
    generation: u64,
    dispatch: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let focus = window.focused(cx);
    window.open_alert_dialog(cx, move |dialog, _, _| {
        dialog
            .title("停止上传并关闭？")
            .description("上传进度会保留，下次选择相同文件和目标目录时可以继续上传。")
            .button_props(
                gpui_kit::component::dialog::DialogButtonProps::default()
                    .ok_text("停止上传并关闭")
                    .cancel_text("继续上传"),
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
                        &ExplorerAction::new(session, ExplorerCommand::CloseConfirmed)
                            .with_generation(generation),
                        window,
                        cx,
                    );
                    true
                }
            })
    });
}
