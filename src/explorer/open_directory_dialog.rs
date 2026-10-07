//! 打开目录/书签, after WinSCP's Open directory dialog: a directory to go to,
//! and this host's bookmarks for the pane. Picking a bookmark fills in the
//! directory; the bookmark matching the directory is the one selected. Changes
//! to the bookmarks are saved as they are made, even if the dialog is then
//! cancelled, as in WinSCP.

use super::{ExplorerId, ExplorerPanel, expand_path};
use crate::app::ExplorerDispatch as _;
use crate::{
    app::{CatalogIcon, ExplorerAction, ExplorerCommand},
    host::{BookmarkSide, HostId, HostStore},
    i18n::t,
    shared::{commit_footer, dismiss_form_error, form_error_notification},
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, WindowExt as _,
    button::Button,
    form::{Field, Form},
    h_flex,
    input::{Input, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

struct OpenDirectoryForm {
    remote: bool,
    /// The tab the commands address, and the host whose bookmarks these are.
    explorer: ExplorerId,
    host: HostId,
    dispatch: FocusHandle,
    store: Entity<HostStore>,
    input: Entity<InputState>,
    list_focus: FocusHandle,
    /// The pane's directory and home, which `~` and relative paths use.
    current: String,
    home: String,
    _subscriptions: Vec<Subscription>,
}

impl OpenDirectoryForm {
    fn bookmarks(&self, cx: &App) -> Vec<String> {
        self.store
            .read(cx)
            .bookmarks(self.host, BookmarkSide::from_remote(self.remote))
            .to_vec()
    }
    /// The typed directory, resolved the way the pane resolves it; empty
    /// while the field is blank.
    fn directory(&self, cx: &App) -> String {
        let text = self.input.read(cx).value();
        match text.trim() {
            "" => String::new(),
            text => expand_path(text, &self.current, &self.home, self.remote),
        }
    }
    fn selected(&self, cx: &App) -> Option<usize> {
        let directory = self.directory(cx);
        self.bookmarks(cx)
            .iter()
            .position(|path| *path == directory)
    }
    fn select(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_value(path, window, cx));
        cx.notify();
    }
    fn command(&self, command: ExplorerCommand, window: &mut Window, cx: &mut App) {
        self.dispatch.dispatch_explorer_action(
            &ExplorerAction::new(self.explorer, command),
            window,
            cx,
        );
    }
    /// Go to the directory; false, with the reason shown, when there is none.
    fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let path = self.directory(cx);
        if path.is_empty() {
            window.push_notification(
                form_error_notification(t!("explorer.open_directory.missing")),
                cx,
            );
            return false;
        }
        let remote = self.remote;
        self.command(ExplorerCommand::Navigate { remote, path }, window, cx);
        true
    }
    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.directory(cx);
        if !path.is_empty() && self.selected(cx).is_none() {
            let remote = self.remote;
            self.command(
                ExplorerCommand::AddBookmark {
                    remote,
                    path: Some(path),
                },
                window,
                cx,
            );
        }
    }
    /// Remove the selected bookmark and select the one after it, so Delete
    /// can be pressed again.
    fn remove(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let bookmarks = self.bookmarks(cx);
        let Some(ix) = self.selected(cx) else {
            return;
        };
        let remote = self.remote;
        let path = bookmarks[ix].clone();
        self.command(ExplorerCommand::RemoveBookmark { remote, path }, window, cx);
        let next = bookmarks
            .get(ix + 1)
            .or_else(|| ix.checked_sub(1).and_then(|ix| bookmarks.get(ix)));
        if let Some(next) = next.cloned() {
            self.select(next, window, cx);
        }
    }
    /// 上移 / 下移.
    fn shift(&mut self, offset: isize, window: &mut Window, cx: &mut Context<Self>) {
        let bookmarks = self.bookmarks(cx);
        let Some(ix) = self.selected(cx) else {
            return;
        };
        let Some(to) = ix
            .checked_add_signed(offset)
            .filter(|to| *to < bookmarks.len())
        else {
            return;
        };
        let remote = self.remote;
        let path = bookmarks[ix].clone();
        self.command(
            ExplorerCommand::MoveBookmark { remote, path, to },
            window,
            cx,
        );
    }
    /// ↑ / ↓ in the list.
    fn step(&mut self, offset: isize, window: &mut Window, cx: &mut Context<Self>) {
        let bookmarks = self.bookmarks(cx);
        let Some(last) = bookmarks.len().checked_sub(1) else {
            return;
        };
        let to = match self.selected(cx) {
            Some(ix) => ix.saturating_add_signed(offset).min(last),
            None if offset > 0 => 0,
            None => last,
        };
        self.select(bookmarks[to].clone(), window, cx);
    }
    fn browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let choice = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(t!("explorer.open_directory.choose")),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = choice.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.select(path.to_string_lossy().into_owned(), window, cx)
                });
            }
        })
        .detach();
    }
    fn render_list(
        &self,
        bookmarks: &[String],
        selected: Option<usize>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let focused = self.list_focus.is_focused(window);
        div()
            .id("bookmark-list")
            .test_support()
            .role(Role::ListBox)
            .aria_label(t!("explorer.open_directory.bookmarks"))
            .track_focus(&self.list_focus)
            .flex_1()
            .min_w_0()
            .h(rems(12.))
            .py_1()
            .overflow_y_scroll()
            .border_1()
            .border_color(if focused { theme.ring } else { theme.border })
            .rounded(theme.radius)
            .text_sm()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "up" => this.step(-1, window, cx),
                    "down" => this.step(1, window, cx),
                    "delete" | "backspace" => this.remove(window, cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .when(bookmarks.is_empty(), |this| {
                this.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_color(theme.muted_foreground)
                        .child(t!("explorer.bookmarks.none")),
                )
            })
            .children(bookmarks.iter().enumerate().map(|(ix, path)| {
                let is_selected = selected == Some(ix);
                let clicked = path.clone();
                h_flex()
                    .id(SharedString::from(format!("bookmark:{path}")))
                    .test_support()
                    .role(Role::ListBoxOption)
                    .aria_selected(is_selected)
                    .gap_2()
                    .px_2()
                    .py_1()
                    .map(|this| {
                        if is_selected {
                            this.bg(theme.list_active)
                        } else {
                            this.hover(|style| style.bg(theme.list_hover))
                        }
                    })
                    .child(
                        Icon::new(CatalogIcon::Bookmark)
                            .small()
                            .text_color(theme.muted_foreground),
                    )
                    .child(div().min_w_0().truncate().child(path.clone()))
                    // A click picks the bookmark; a double-click opens it.
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        window.focus(&this.list_focus, cx);
                        this.select(clicked.clone(), window, cx);
                        if event.click_count() == 2 && this.open(window, cx) {
                            window.close_dialog(cx);
                        }
                    }))
            }))
    }
}

impl Render for OpenDirectoryForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bookmarks = self.bookmarks(cx);
        let selected = self.selected(cx);
        let typed = !self.directory(cx).is_empty();
        let button = |id: &'static str, label: SharedString| Button::new(id).small().label(label);
        v_flex().gap_3().child(
            Form::new()
                .child(
                    Field::new()
                        .label(t!("explorer.open_directory.folder"))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Input::new(&self.input)
                                        .id("open-directory-path")
                                        .small()
                                        .flex_1(),
                                )
                                .when(!self.remote, |this| {
                                    this.child(
                                        button("open-directory-browse", t!("explorer.browse"))
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.browse(window, cx)
                                            })),
                                    )
                                }),
                        ),
                )
                .child(
                    Field::new()
                        .label(t!("explorer.open_directory.bookmarks"))
                        .child(
                            h_flex()
                                .gap_2()
                                .items_start()
                                .child(self.render_list(&bookmarks, selected, window, cx))
                                .child(
                                    v_flex()
                                        .gap_1()
                                        .w(rems(5.))
                                        .flex_shrink_0()
                                        .child(
                                            button(
                                                "bookmark-add",
                                                t!("explorer.open_directory.add"),
                                            )
                                            .w_full()
                                            .disabled(!typed || selected.is_some())
                                            .on_click(
                                                cx.listener(|this, _, window, cx| {
                                                    this.add(window, cx)
                                                }),
                                            ),
                                        )
                                        .child(
                                            button(
                                                "bookmark-remove",
                                                t!("explorer.open_directory.remove"),
                                            )
                                            .w_full()
                                            .disabled(selected.is_none())
                                            .on_click(
                                                cx.listener(|this, _, window, cx| {
                                                    this.remove(window, cx)
                                                }),
                                            ),
                                        )
                                        .child(div().h_4())
                                        .child(
                                            button("bookmark-up", t!("explorer.open_directory.up"))
                                                .w_full()
                                                .disabled(selected.is_none_or(|ix| ix == 0))
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.shift(-1, window, cx)
                                                })),
                                        )
                                        .child(
                                            button(
                                                "bookmark-down",
                                                t!("explorer.open_directory.down"),
                                            )
                                            .w_full()
                                            .disabled(
                                                selected.is_none_or(|ix| ix + 1 >= bookmarks.len()),
                                            )
                                            .on_click(
                                                cx.listener(|this, _, window, cx| {
                                                    this.shift(1, window, cx)
                                                }),
                                            ),
                                        ),
                                ),
                        ),
                ),
        )
    }
}

impl ExplorerPanel {
    /// WinSCP's 打开目录/书签 for one pane.
    pub(super) fn open_directory(
        &mut self,
        remote: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pane = self.pane(remote).clone();
        if !pane.read(cx).is_connected() || window.has_active_dialog(cx) {
            return;
        }
        let (current, home, pane_focus) = {
            let pane = pane.read(cx);
            (pane.path(), pane.home(), pane.focus_handle(cx))
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(&current));
        let form = cx.new(|cx| OpenDirectoryForm {
            remote,
            explorer: self.id(),
            host: self.host_id(),
            dispatch: self.dispatch.clone(),
            store: self.store.clone(),
            input: input.clone(),
            list_focus: cx.focus_handle().tab_stop(true),
            current,
            home,
            _subscriptions: vec![cx.observe(&self.store, |_, _, cx| cx.notify())],
        });
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(t!("explorer.open_directory.title"))
                // Closed by its buttons or Escape, not by a click beside it.
                .overlay_closable(false)
                .child(form.clone())
                .footer(commit_footer(
                    "open-directory-confirm",
                    t!("explorer.open_directory.open"),
                ))
                .on_ok({
                    let form = form.clone();
                    move |_, window, cx| form.update(cx, |form, cx| form.open(window, cx))
                })
                // Back to the pane the directory was opened in.
                .on_close({
                    let pane_focus = pane_focus.clone();
                    move |_, window, cx| {
                        dismiss_form_error(window, cx);
                        window.focus(&pane_focus, cx)
                    }
                })
        });
        // As in WinSCP the directory is ready to be typed over.
        input.update(cx, |input, cx| {
            input.select_all(window, cx);
            input.focus(window, cx);
        });
    }
}
