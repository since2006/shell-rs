use super::{ExplorerId, ExplorerPanel};
use crate::app::ExplorerDispatch as _;
use crate::{
    app::{ExplorerAction, ExplorerCommand},
    sftp::{TransferAnswer, TransferChoice, TransferDirection, TransferQuestionKind},
};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    dialog::{DialogAction, DialogClose, DialogFooter},
    form::{Field, Form},
    h_flex,
    input::{Input, InputState},
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{
    cell::Cell,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
};

impl ExplorerPanel {
    pub(super) fn open_upload(
        &mut self,
        paths: Vec<PathBuf>,
        target: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty()
            || self.connection_state() != crate::session::ConnectionState::Connected
            || window.has_active_dialog(cx)
        {
            return;
        }
        let target_input = cx.new(|cx| InputState::new(window, cx).default_value(&target));
        let sources = self.transfer_sources(
            false,
            paths.iter().map(|path| path.to_string_lossy().into_owned()),
            cx,
        );
        let queued = self.is_transferring();
        let form = cx.new(|_| UploadForm {
            queued,
            sources,
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
            || self.connection_state() != crate::session::ConnectionState::Connected
            || window.has_active_dialog(cx)
        {
            return;
        }
        let target_input = cx.new(|cx| InputState::new(window, cx).default_value(&target));
        let sources = self.transfer_sources(true, paths.iter().cloned(), cx);
        let queued = self.is_transferring();
        let form = cx.new(|_| DownloadForm {
            queued,
            sources,
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
    /// What a transfer dialog lists for `paths`. Folders are told from
    /// files by the pane that lists them; a local path it does not list,
    /// dropped from elsewhere, is looked at on disk.
    fn transfer_sources(
        &self,
        remote: bool,
        paths: impl Iterator<Item = String>,
        cx: &App,
    ) -> TransferSources {
        let pane = self.pane(remote).read(cx);
        let listed: HashMap<String, bool> = pane
            .entries(cx)
            .iter()
            .filter(|entry| !entry.is_parent())
            .map(|entry| (pane.child_path_of(&entry.name), entry.is_dir()))
            .collect();
        TransferSources::new(
            paths
                .map(|path| {
                    let is_dir = listed
                        .get(&path)
                        .copied()
                        .unwrap_or_else(|| !remote && Path::new(&path).is_dir());
                    SourceItem::new(path, remote, is_dir)
                })
                .collect(),
        )
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
        self.question_shown = true;
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
/// What an upload or download is about: the folder its items are in, once,
/// then each item by name on a line of its own. Whole paths wrapped at a `/`
/// and one item read as two.
struct TransferSources {
    /// Shared by every item, as it is for a pane's selection.
    folder: Option<String>,
    items: Vec<SourceItem>,
}

struct SourceItem {
    path: String,
    folder: String,
    name: String,
    is_dir: bool,
}

impl SourceItem {
    fn new(path: String, remote: bool, is_dir: bool) -> Self {
        let (folder, name) = if remote {
            match path.rsplit_once('/') {
                Some(("", name)) => ("/".to_string(), name.to_string()),
                Some((folder, name)) => (folder.to_string(), name.to_string()),
                None => (String::new(), path.clone()),
            }
        } else {
            let local = Path::new(&path);
            (
                local
                    .parent()
                    .map(|folder| folder.display().to_string())
                    .unwrap_or_default(),
                local
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.clone()),
            )
        };
        Self {
            path,
            folder,
            name,
            is_dir,
        }
    }
}

impl TransferSources {
    fn new(items: Vec<SourceItem>) -> Self {
        let folder = items
            .first()
            .map(|first| first.folder.clone())
            .filter(|folder| items.iter().all(|item| item.folder == *folder));
        Self { folder, items }
    }

    /// `2 个文件`, `1 个文件夹`, or `3 个项目（1 个文件夹、2 个文件）`.
    fn count(&self) -> String {
        let folders = self.items.iter().filter(|item| item.is_dir).count();
        match (folders, self.items.len() - folders) {
            (0, files) => format!("{files} 个文件"),
            (folders, 0) => format!("{folders} 个文件夹"),
            (folders, files) => {
                format!(
                    "{} 个项目（{folders} 个文件夹、{files} 个文件）",
                    folders + files
                )
            }
        }
    }

    fn render(&self, id: &'static str, cx: &App) -> impl IntoElement + use<> {
        let muted = cx.theme().muted_foreground;
        let shared_folder = self.folder.is_some();
        v_flex()
            .gap_1()
            .when_some(self.folder.clone(), |this, folder| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("位于 {folder}")),
                )
            })
            .child(
                v_flex()
                    .id(id)
                    .max_h_32()
                    .overflow_y_scroll()
                    .py_1()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .children(self.items.iter().map(|item| {
                        let path = item.path.clone();
                        h_flex()
                            .id(ElementId::Name(format!("source:{}", item.path).into()))
                            .test_support()
                            .aria_label(item.name.clone())
                            .gap_2()
                            .px_2()
                            .py_0p5()
                            .text_sm()
                            .child(
                                Icon::new(if item.is_dir {
                                    IconName::Folder
                                } else {
                                    IconName::File
                                })
                                .small()
                                .text_color(muted),
                            )
                            .child(div().min_w_0().child(item.name.clone()))
                            // Items from different folders say which.
                            .when(!shared_folder, |this| {
                                this.child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(item.folder.clone()),
                                )
                            })
                            .tooltip(move |window, cx| Tooltip::new(path.clone()).build(window, cx))
                    })),
            )
    }
}

/// Said when another batch is running, which this one will follow.
fn queued_note(cx: &App) -> impl IntoElement {
    div()
        .id("transfer-queued-note")
        .test_support()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child("当前有传输在进行，这一批会排在传输队列里，轮到时自动开始。")
}

/// The dialog's first line: what goes where.
fn summary(text: String) -> impl IntoElement {
    div()
        .id("transfer-summary")
        .test_support()
        .aria_label(text.clone())
        .text_sm()
        .child(text)
}

struct UploadForm {
    /// Another batch runs; this one will wait its turn.
    queued: bool,
    sources: TransferSources,
    target: Entity<InputState>,
    endpoint: String,
    error: Option<String>,
}
impl Render for UploadForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .child(summary(format!(
                "上传 {}到 {}",
                self.sources.count(),
                self.endpoint
            )))
            .child(self.sources.render("upload-source-list", cx))
            .when(self.queued, |this| this.child(queued_note(cx)))
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
    /// Another batch runs; this one will wait its turn.
    queued: bool,
    sources: TransferSources,
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
            .child(summary(format!(
                "从 {} 下载 {}",
                self.endpoint,
                self.sources.count()
            )))
            .child(self.sources.render("download-source-list", cx))
            .when(self.queued, |this| this.child(queued_note(cx)))
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

#[cfg(test)]
mod tests {
    use super::{SourceItem, TransferSources};

    fn sources(items: &[(&str, bool, bool)]) -> TransferSources {
        TransferSources::new(
            items
                .iter()
                .map(|&(path, remote, is_dir)| SourceItem::new(path.into(), remote, is_dir))
                .collect(),
        )
    }

    #[test]
    fn a_selection_is_listed_by_name_under_its_folder_once() {
        let local = sources(&[
            (
                "/Users/me/Downloads/OpenWebStart_macos-aarch64_1_14_0.dmg",
                false,
                false,
            ),
            ("/Users/me/Downloads/db_dump 2.sql", false, false),
        ]);
        assert_eq!(local.folder.as_deref(), Some("/Users/me/Downloads"));
        let names: Vec<&str> = local.items.iter().map(|item| item.name.as_str()).collect();
        assert_eq!(
            names,
            ["OpenWebStart_macos-aarch64_1_14_0.dmg", "db_dump 2.sql"]
        );
        assert_eq!(local.count(), "2 个文件");

        let remote = sources(&[("/srv/app", true, true), ("/etc/hosts", true, false)]);
        assert_eq!(remote.folder, None);
        assert_eq!(remote.items[0].folder, "/srv");
        assert_eq!(remote.count(), "2 个项目（1 个文件夹、1 个文件）");

        let root = sources(&[("/boot", true, true)]);
        assert_eq!(root.folder.as_deref(), Some("/"));
        assert_eq!(root.items[0].name, "boot");
        assert_eq!(root.count(), "1 个文件夹");
    }
}
