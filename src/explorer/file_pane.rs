use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    table::{DataTable, TableEvent, TableState},
    v_flex,
};
use gpui_kit::*;

use crate::app::CatalogIcon;
use crate::session::SessionId;

use super::{FileEntry, FileListing, Location};

/// Which side of the explorer a pane is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneSide {
    Local,
    Remote,
}

impl PaneSide {
    pub fn label(self) -> &'static str {
        match self {
            PaneSide::Local => "本地",
            PaneSide::Remote => "远程",
        }
    }

    fn pane_id(self) -> &'static str {
        match self {
            PaneSide::Local => "local-pane",
            PaneSide::Remote => "remote-pane",
        }
    }

    fn path_id(self) -> &'static str {
        match self {
            PaneSide::Local => "local-path",
            PaneSide::Remote => "remote-path",
        }
    }
}

/// One side of the explorer: address row, file table, summary footer.
pub struct FilePane {
    side: PaneSide,
    session_id: SessionId,
    location: Location,
    table: Entity<TableState<FileListing>>,
    path_input: Entity<InputState>,
    selected: Option<usize>,
    _subscriptions: Vec<Subscription>,
}

impl FilePane {
    pub fn new(
        side: PaneSide,
        session_id: SessionId,
        location: Location,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let rows = location.rows();
        let table = cx.new(|cx| {
            TableState::new(FileListing::new(side, rows), window, cx)
                .row_selectable(true)
                .col_selectable(false)
                .cell_selectable(false)
        });
        let path_input = cx.new(|cx| InputState::new(window, cx).default_value(location.display()));
        let subscriptions =
            vec![
                cx.subscribe_in(&table, window, |this, _, event: &TableEvent, window, cx| {
                    match event {
                        TableEvent::SelectRow(ix) => {
                            this.selected = Some(*ix);
                            cx.notify();
                        }
                        TableEvent::DoubleClickedRow(ix) => this.open_row(*ix, window, cx),
                        TableEvent::ClearSelection => {
                            this.selected = None;
                            cx.notify();
                        }
                        _ => {}
                    }
                }),
                cx.subscribe_in(
                    &path_input,
                    window,
                    |this, state, event: &InputEvent, window, cx| {
                        if let InputEvent::PressEnter { .. } = event {
                            let path = state.read(cx).value().to_string();
                            this.navigate_to(&path, window, cx);
                        }
                    },
                ),
            ];

        Self {
            side,
            session_id,
            location,
            table,
            path_input,
            selected: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn side(&self) -> PaneSide {
        self.side
    }

    /// Absolute path of the directory shown.
    pub fn path(&self) -> String {
        self.location.path()
    }

    pub fn selected_entry(&self, cx: &App) -> Option<FileEntry> {
        self.selected
            .and_then(|ix| self.table.read(cx).delegate().entry(ix).cloned())
    }

    fn open_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.table.read(cx).delegate().entry(ix).cloned() else {
            return;
        };
        if !entry.is_dir() {
            return;
        }
        let moved = if entry.is_parent() {
            self.location.up()
        } else {
            self.location.enter(&entry.name)
        };
        if moved {
            self.reload(window, cx);
        }
    }

    fn go_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.location.up() {
            self.reload(window, cx);
        }
    }

    fn navigate_to(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.location.set_path(path) {
            self.reload(window, cx);
        } else {
            window.push_notification(format!("路径不存在：{path}"), cx);
        }
    }

    /// Re-read the current directory into the table and address field.
    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.location.rows();
        self.table.update(cx, |table, cx| {
            table.delegate_mut().set_rows(rows);
            table.clear_selection(cx);
            table.refresh(cx);
        });
        let display = self.location.display();
        self.path_input
            .update(cx, |input, cx| input.set_value(display, window, cx));
        self.selected = None;
        cx.notify();
    }

    /// Mock transfer: the real product would enqueue an upload/download.
    fn transfer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(entry) = self.selected_entry(cx) {
            window.push_notification(format!("已加入传输队列：{}", entry.name), cx);
        }
    }
}

impl Render for FilePane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sid = self.session_id.0;
        let selected = self.selected_entry(cx);
        let transferable = selected.as_ref().is_some_and(|entry| !entry.is_parent());
        let count = self
            .table
            .read(cx)
            .delegate()
            .rows()
            .iter()
            .filter(|entry| !entry.is_parent())
            .count();
        let (transfer_id, transfer_icon, transfer_label) = match self.side {
            PaneSide::Local => ("upload", CatalogIcon::Upload, "上传"),
            PaneSide::Remote => ("download", CatalogIcon::Download, "下载"),
        };

        v_flex()
            .id((self.side.pane_id(), sid))
            .test_support()
            .size_full()
            .child(
                h_flex()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .w_10()
                            .flex_shrink_0()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(self.side.label()),
                    )
                    .child(
                        Input::new(&self.path_input)
                            .id(self.side.path_id())
                            .small()
                            .flex_1(),
                    )
                    .child(
                        Button::new("up")
                            .ghost()
                            .xsmall()
                            .icon(IconName::ArrowUp)
                            .tooltip("上级目录")
                            .disabled(self.location.is_root())
                            .on_click(cx.listener(|this, _, window, cx| this.go_up(window, cx))),
                    )
                    .child(
                        Button::new("refresh")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(CatalogIcon::RefreshCw))
                            .tooltip("刷新")
                            .on_click(cx.listener(|this, _, window, cx| this.reload(window, cx))),
                    )
                    .child(
                        Button::new(transfer_id)
                            .outline()
                            .xsmall()
                            .icon(Icon::new(transfer_icon))
                            .label(transfer_label)
                            .disabled(!transferable)
                            .on_click(cx.listener(|this, _, window, cx| this.transfer(window, cx))),
                    ),
            )
            .child(
                div().flex_1().min_h_0().child(
                    DataTable::new(&self.table)
                        .stripe(false)
                        .bordered(false)
                        .small(),
                ),
            )
            .child(
                h_flex()
                    .justify_between()
                    .px_2()
                    .py_1()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "共 {count} 项，已选 {} 项",
                        usize::from(transferable)
                    )),
            )
    }
}
