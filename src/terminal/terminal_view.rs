use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use alacritty_terminal::index::{Column, Line, Point as TerminalPoint, Side};
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::COUNT;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::PopupMenu,
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use unicode_width::UnicodeWidthChar as _;

use crate::app::{
    CatalogIcon, ClearTerminal, CopyTerminal, DismissTerminalFind, FindInTerminal,
    FindNextInTerminal, FindPreviousInTerminal, PasteTerminal,
};
use crate::connection::{ConnectionPromptReply, Latency};

use super::mouse::{self, MouseReport, ReportButton, ReportKind};
use super::search::SearchMark;
use super::{
    SearchDirection, SharedTerminalTransportFactory, TerminalEngine, TerminalEvent, TerminalFont,
    TerminalLifecycle, TerminalSize, TerminalSnapshot, TerminalStatus,
};

pub const TERMINAL_KEY_CONTEXT: &str = "Terminal";
/// Key context of a terminal's find bar.
pub const TERMINAL_FIND_KEY_CONTEXT: &str = "TerminalFind";

/// How long typing in the find bar pauses before the scrollback is searched.
/// A full search of a long scrollback takes a frame or more, so it waits for
/// the query to settle rather than running on every keystroke.
const FIND_DEBOUNCE: Duration = Duration::from_millis(80);

/// How long the pointer rests on a link before its address shows.
const LINK_TOOLTIP_DELAY: Duration = Duration::from_millis(500);

/// The key held to open a link with a click, as the tooltip names it.
#[cfg(target_os = "macos")]
const OPEN_LINK_KEY: &str = "⌘";
#[cfg(not(target_os = "macos"))]
const OPEN_LINK_KEY: &str = "Ctrl";

/// Whether `modifiers` hold the key that opens a link: ⌘, or Ctrl where
/// there is no ⌘. A plain click selects text, as it always has.
fn opens_links(modifiers: &Modifiers) -> bool {
    if cfg!(target_os = "macos") {
        modifiers.platform
    } else {
        modifiers.control
    }
}

/// The commands a terminal's owner adds to the bottom of its context menu,
/// built from the terminal's lifecycle when the menu opens.
pub type TerminalMenuItems = Rc<dyn Fn(PopupMenu, &TerminalLifecycle) -> PopupMenu>;

/// The open find bar of one terminal.
struct FindBar {
    input: Entity<InputState>,
    /// The search waiting for typing to pause, if any.
    pending: Option<Task<()>>,
    _subscription: Subscription,
}

gpui_kit::actions!(shellrs_terminal, [SendTab, SendBackTab]);

pub(crate) fn terminal_key_bindings() -> [KeyBinding; 2] {
    [
        KeyBinding::new("tab", SendTab, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("shift-tab", SendBackTab, Some(TERMINAL_KEY_CONTEXT)),
    ]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScrollTarget {
    History,
    AlternateScreen,
    /// The program asked for the mouse: the wheel is reported to it.
    Report,
}

#[derive(Clone, Copy, Debug, Default)]
struct ScrollAccumulator {
    fractional_lines: f32,
    target: Option<ScrollTarget>,
}

impl ScrollAccumulator {
    fn reset(&mut self) {
        self.fractional_lines = 0.;
        self.target = None;
    }

    fn consume(&mut self, delta: ScrollDelta, line_height: Pixels, target: ScrollTarget) -> i32 {
        if self.target != Some(target) {
            self.fractional_lines = 0.;
            self.target = Some(target);
        }

        match delta {
            ScrollDelta::Lines(delta) => {
                self.fractional_lines = 0.;
                delta.y.round() as i32
            }
            ScrollDelta::Pixels(delta) => {
                self.fractional_lines += delta.y.as_f32() / line_height.as_f32().max(1.);
                let complete_lines = self.fractional_lines.trunc() as i32;
                self.fractional_lines -= complete_lines as f32;
                complete_lines.clamp(-12, 12)
            }
        }
    }
}

/// Where the grid sits on screen and how large one cell of it is.
#[derive(Clone, Copy, Debug)]
struct TerminalGeometry {
    bounds: Bounds<Pixels>,
    cell_width: Pixels,
    line_height: Pixels,
}

impl TerminalGeometry {
    fn cell_bounds(&self, column: usize, row: usize, width: usize) -> Bounds<Pixels> {
        Bounds::new(
            point(
                self.bounds.left() + self.cell_width * column as f32,
                self.bounds.top() + self.line_height * row as f32,
            ),
            size(self.cell_width * width as f32, self.line_height),
        )
    }
}

#[derive(Clone, Copy, Debug)]
struct TerminalViewport {
    geometry: TerminalGeometry,
    columns: usize,
    rows: usize,
    display_offset: usize,
    cursor_bounds: Bounds<Pixels>,
}

impl TerminalViewport {
    /// The cell on screen under `position`, if it is over the grid.
    fn cell_at(&self, position: gpui_kit::Point<Pixels>) -> Option<(usize, usize)> {
        screen_cell(&self.geometry, self.columns, self.rows, position)
    }

    /// The cell on screen nearest `position`, which may be outside: where a
    /// drag that left the terminal is reported.
    fn nearest_cell(&self, position: gpui_kit::Point<Pixels>) -> (usize, usize) {
        let (point, _) = self.grid_point(position);
        (
            point.column.0,
            (point.line.0 + self.display_offset as i32).max(0) as usize,
        )
    }

    fn grid_point(&self, position: gpui_kit::Point<Pixels>) -> (TerminalPoint, Side) {
        grid_point(
            position,
            self.geometry.bounds,
            self.geometry.cell_width,
            self.geometry.line_height,
            self.display_offset,
            self.columns,
            self.rows,
        )
    }
}

/// The link under the pointer, and where the pointer is.
#[derive(Clone, Debug, PartialEq)]
struct HoveredLink {
    /// Which link of the snapshot, as the cells number them.
    index: usize,
    uri: SharedString,
    position: gpui_kit::Point<Pixels>,
}

pub struct TerminalView {
    engine: Entity<TerminalEngine>,
    element_id: ElementId,
    aria_label: SharedString,
    focus_handle: FocusHandle,
    marked_text: String,
    marked_selection: Range<usize>,
    viewport: Option<TerminalViewport>,
    ime_cursor_bounds: Option<Bounds<Pixels>>,
    requested_size: TerminalSize,
    /// A selection drag is under way.
    selecting: bool,
    scroll_accumulator: ScrollAccumulator,
    context_menu: Option<(Entity<PopupMenu>, gpui_kit::Point<Pixels>)>,
    context_menu_subscription: Option<Subscription>,
    menu_items: Option<TerminalMenuItems>,
    find: Option<FindBar>,
    cursor_visible: bool,
    focused: bool,
    hovered_link: Option<HoveredLink>,
    /// The hovered link's address shows, the pointer having rested on it.
    link_tooltip: bool,
    link_tooltip_task: Option<Task<()>>,
    /// The button the program was told went down, until it hears it go up.
    reported_button: Option<ReportButton>,
    /// The cell the program last heard of: motion is reported once a cell.
    reported_cell: Option<(usize, usize)>,
    _subscriptions: Vec<Subscription>,
    _blink_task: Task<()>,
}

impl TerminalView {
    pub fn new(
        element_id: impl Into<ElementId>,
        aria_label: impl Into<SharedString>,
        factory: SharedTerminalTransportFactory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let engine = cx.new(|cx| TerminalEngine::new(factory, cx));
        let focus_handle = cx.focus_handle();
        let subscriptions = vec![
            cx.observe(&engine, |_, _, cx| cx.notify()),
            // The panel hears the engine's events through the view.
            cx.subscribe(&engine, |_, _, event: &TerminalEvent, cx| {
                cx.emit(event.clone())
            }),
            cx.on_focus(&focus_handle, window, |this, _, cx| {
                this.focused = true;
                this.cursor_visible = true;
                this.engine.read(cx).set_focused(true);
                cx.notify();
            }),
            cx.on_blur(&focus_handle, window, |this, _, cx| {
                this.focused = false;
                this.cursor_visible = false;
                this.selecting = false;
                this.engine.read(cx).set_focused(false);
                cx.notify();
            }),
        ];
        let blink_task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                if this
                    .update(cx, |this, cx| {
                        if this.focused && this.snapshot(cx).cursor_blinking {
                            this.cursor_visible = !this.cursor_visible;
                            cx.notify();
                        } else if this.focused && !this.cursor_visible {
                            this.cursor_visible = true;
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        Self {
            engine,
            element_id: element_id.into(),
            aria_label: aria_label.into(),
            focus_handle,
            marked_text: String::new(),
            marked_selection: 0..0,
            viewport: None,
            ime_cursor_bounds: None,
            requested_size: TerminalSize::DEFAULT,
            selecting: false,
            scroll_accumulator: ScrollAccumulator::default(),
            context_menu: None,
            context_menu_subscription: None,
            menu_items: None,
            find: None,
            cursor_visible: true,
            focused: false,
            hovered_link: None,
            link_tooltip: false,
            link_tooltip_task: None,
            reported_button: None,
            reported_cell: None,
            _subscriptions: subscriptions,
            _blink_task: blink_task,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus_handle, cx);
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    pub fn restart(&mut self, cx: &mut Context<Self>) {
        self.reset_interaction();
        self.engine.update(cx, |engine, cx| engine.restart(cx));
    }

    pub fn restart_with_factory(
        &mut self,
        factory: SharedTerminalTransportFactory,
        cx: &mut Context<Self>,
    ) {
        self.reset_interaction();
        self.engine
            .update(cx, |engine, cx| engine.restart_with_factory(factory, cx));
    }

    pub fn reply_to_prompt(&self, request_id: u64, reply: ConnectionPromptReply, cx: &App) {
        self.engine.read(cx).reply_to_prompt(request_id, reply);
    }

    /// Forget everything in progress on the screen that is going away: IME
    /// composition, a selection drag, the context menu and the find bar.
    fn reset_interaction(&mut self) {
        self.marked_text.clear();
        self.marked_selection = 0..0;
        self.ime_cursor_bounds = None;
        self.selecting = false;
        self.scroll_accumulator.reset();
        self.context_menu = None;
        self.context_menu_subscription = None;
        self.find = None;
        self.cursor_visible = true;
        self.hovered_link = None;
        self.link_tooltip = false;
        self.link_tooltip_task = None;
        self.reported_button = None;
        self.reported_cell = None;
    }

    pub fn shutdown(&mut self, cx: &mut Context<Self>) {
        self.reset_interaction();
        self.engine.update(cx, |engine, cx| engine.shutdown(cx));
    }

    pub fn stop(&mut self, message: &str, cx: &mut Context<Self>) {
        self.engine
            .update(cx, |engine, cx| engine.stop(message, cx));
    }

    pub fn status(&self, cx: &App) -> TerminalStatus {
        self.engine.read(cx).status()
    }

    pub fn lifecycle(&self, cx: &App) -> TerminalLifecycle {
        self.engine.read(cx).lifecycle().clone()
    }

    pub fn title(&self, cx: &App) -> Option<String> {
        self.engine.read(cx).title().map(str::to_owned)
    }

    pub fn latency(&self, cx: &App) -> Option<Latency> {
        self.engine.read(cx).latency()
    }

    /// Which connection the terminal is on: counts up with every reconnect.
    pub fn generation(&self, cx: &App) -> u64 {
        self.engine.read(cx).generation()
    }

    /// Run `command` beside the shell, on this terminal's own connection;
    /// see [`super::ExecRequest`]. `None` while the shell is not running.
    pub fn exec(
        &self,
        command: String,
        cx: &App,
    ) -> Option<std::sync::mpsc::Receiver<super::ExecResult>> {
        self.engine.read(cx).exec(command)
    }

    pub fn screen_text(&self, cx: &App) -> String {
        self.engine.read(cx).snapshot().visible_text()
    }

    pub fn copy_selection(&self, cx: &mut App) {
        if let Some(text) = self
            .engine
            .read(cx)
            .selection_text()
            .filter(|text| !text.is_empty())
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    pub fn paste_clipboard(&mut self, cx: &mut Context<Self>) {
        if !self.engine.read(cx).lifecycle().accepts_input() {
            return;
        }
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        self.engine.read(cx).paste(&text);
        self.selecting = false;
        self.scroll_accumulator.reset();
        self.cursor_visible = true;
        cx.notify();
    }

    /// Put `command` on the shell's input line, with `replace` in place of
    /// what is typed there, and with `run` run it; see
    /// [`TerminalEngine::enter_command`].
    pub fn enter_command(
        &mut self,
        command: &str,
        run: bool,
        replace: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        self.engine.read(cx).enter_command(command, run, replace)?;
        self.selecting = false;
        self.scroll_accumulator.reset();
        self.cursor_visible = true;
        cx.notify();
        Ok(())
    }

    /// Add the owner's commands below the terminal's own in the context menu.
    pub fn set_menu_items(&mut self, items: TerminalMenuItems) {
        self.menu_items = Some(items);
    }

    /// Clear the screen and the scrollback, keeping the prompt line.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.selecting = false;
        self.scroll_accumulator.reset();
        self.engine
            .update(cx, |engine, cx| engine.clear_keeping_prompt(cx));
    }

    /// Open the find bar and put the cursor in it. An open bar selects its
    /// query again. A selection, when there is one, becomes the query.
    pub fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selection = self
            .engine
            .read(cx)
            .selection_text()
            .filter(|text| !text.is_empty() && !text.contains('\n'));
        let input = match &self.find {
            Some(find) => find.input.clone(),
            None => {
                let input = cx.new(|cx| InputState::new(window, cx).placeholder("查找"));
                let subscription = cx.subscribe_in(
                    &input,
                    window,
                    |this, _, event: &InputEvent, _, cx| match event {
                        InputEvent::Change => this.schedule_find(cx),
                        InputEvent::PressEnter { shift, .. } => {
                            let direction = if *shift {
                                SearchDirection::Up
                            } else {
                                SearchDirection::Down
                            };
                            this.step_find(direction, cx);
                        }
                        _ => {}
                    },
                );
                self.find = Some(FindBar {
                    input: input.clone(),
                    pending: None,
                    _subscription: subscription,
                });
                input
            }
        };
        if let Some(query) = selection {
            input.update(cx, |input, cx| input.set_value(query, window, cx));
            self.run_find(cx);
        }
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        cx.notify();
    }

    /// Focus the next match up or down. A query still being typed is searched
    /// first, and that search picks the match to focus.
    pub fn step_find(&mut self, direction: SearchDirection, cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else {
            return;
        };
        if find.pending.take().is_some() {
            self.run_find(cx);
            return;
        }
        self.engine
            .update(cx, |engine, cx| engine.step_search(direction, cx));
    }

    /// Close the find bar, drop its highlights and hand the keyboard back to
    /// the terminal.
    pub fn dismiss_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.take().is_none() {
            return;
        }
        self.engine.update(cx, |engine, cx| engine.clear_search(cx));
        self.focus(window, cx);
        cx.notify();
    }

    fn schedule_find(&mut self, cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else {
            return;
        };
        find.pending = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FIND_DEBOUNCE).await;
            _ = this.update(cx, |this, cx| this.run_find(cx));
        }));
    }

    fn run_find(&mut self, cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else {
            return;
        };
        find.pending = None;
        let query = find.input.read(cx).value();
        self.engine
            .update(cx, |engine, cx| engine.set_search_query(&query, cx));
    }

    fn snapshot(&self, cx: &App) -> TerminalSnapshot {
        self.engine.read(cx).snapshot()
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let mode = self.engine.read(cx).mode();
        if let Some(bytes) = encode_key(&event.keystroke, mode) {
            self.send_user_input(bytes, cx);
            cx.stop_propagation();
        }
    }

    fn resize(&mut self, size: TerminalSize, cx: &mut Context<Self>) {
        if size == self.requested_size {
            return;
        }
        self.requested_size = size;
        self.scroll_accumulator.reset();
        self.engine.update(cx, |engine, cx| engine.resize(size, cx));
    }

    fn scroll(
        &mut self,
        delta: ScrollDelta,
        line_height: Pixels,
        cell: (usize, usize),
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) {
        let mode = self.engine.read(cx).mode();
        // Shift scrolls the history even while the program has the mouse.
        let target = if mode.intersects(TermMode::MOUSE_MODE) && !modifiers.shift {
            ScrollTarget::Report
        } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
            ScrollTarget::AlternateScreen
        } else {
            ScrollTarget::History
        };
        let lines = self.scroll_accumulator.consume(delta, line_height, target);
        if lines == 0 {
            return;
        }
        // The text under the pointer moves on.
        self.hover_link(None, cx);
        if target == ScrollTarget::Report {
            let button = if lines > 0 {
                ReportButton::WheelUp
            } else {
                ReportButton::WheelDown
            };
            for _ in 0..lines.unsigned_abs().min(12) {
                self.report_mouse(button, ReportKind::Press, cell, modifiers, cx);
            }
        } else if target == ScrollTarget::AlternateScreen {
            let sequence = if lines > 0 { b"\x1bOA" } else { b"\x1bOB" };
            for _ in 0..lines.unsigned_abs().min(12) {
                self.engine.read(cx).write(sequence.to_vec());
            }
        } else {
            self.engine
                .update(cx, |engine, cx| engine.scroll(lines, cx));
        }
    }

    fn start_selection(
        &mut self,
        point: TerminalPoint,
        side: Side,
        selection_type: SelectionType,
        cx: &mut Context<Self>,
    ) {
        self.selecting = true;
        self.engine.update(cx, |engine, cx| {
            engine.start_selection(point, side, selection_type, cx)
        });
    }

    fn update_selection(&mut self, point: TerminalPoint, side: Side, cx: &mut Context<Self>) {
        self.engine
            .update(cx, |engine, cx| engine.update_selection(point, side, cx));
    }

    /// Tell the program of the mouse, if it asked to hear of it. Shift keeps
    /// the mouse for selecting text, unless a reported button is down.
    /// Whether the program was told.
    fn report_mouse(
        &mut self,
        button: ReportButton,
        kind: ReportKind,
        cell: (usize, usize),
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) -> bool {
        let mode = self.engine.read(cx).mode();
        let held = self.reported_button.is_some();
        if (modifiers.shift && !held) || !mouse::wants(mode, kind, held) {
            return false;
        }
        let button = match kind {
            ReportKind::Motion if self.reported_cell == Some(cell) => return true,
            ReportKind::Motion => self.reported_button.unwrap_or(ReportButton::None),
            _ => button,
        };
        let report = MouseReport {
            button,
            kind,
            column: cell.0,
            row: cell.1,
            alt: modifiers.alt,
            control: modifiers.control,
        };
        if let Some(bytes) = mouse::encode(report, mode) {
            self.engine.read(cx).write(bytes);
        }
        self.reported_cell = Some(cell);
        if kind == ReportKind::Press && matches!(button, ReportButton::Left | ReportButton::Middle)
        {
            self.reported_button = Some(button);
        }
        true
    }

    /// A button went up: the program hears of it if it heard it go down.
    fn release_mouse(
        &mut self,
        button: ReportButton,
        cell: (usize, usize),
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) {
        if self.reported_button == Some(button) {
            self.report_mouse(button, ReportKind::Release, cell, modifiers, cx);
            self.reported_button = None;
        }
    }

    /// The pointer moved onto a link or off one. Its address shows once the
    /// pointer rests there.
    fn hover_link(&mut self, link: Option<HoveredLink>, cx: &mut Context<Self>) {
        let same = match (&self.hovered_link, &link) {
            (Some(hovered), Some(link)) => hovered.index == link.index && hovered.uri == link.uri,
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.link_tooltip = false;
        self.link_tooltip_task = link.as_ref().map(|_| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(LINK_TOOLTIP_DELAY).await;
                let _ = this.update(cx, |this, cx| {
                    this.link_tooltip = true;
                    cx.notify();
                });
            })
        });
        self.hovered_link = link;
        cx.notify();
    }

    fn send_user_input(&mut self, bytes: impl Into<Vec<u8>>, cx: &mut Context<Self>) {
        self.hover_link(None, cx);
        self.selecting = false;
        self.scroll_accumulator.reset();
        self.cursor_visible = true;
        self.engine.read(cx).send_user_input(bytes);
        cx.notify();
    }

    fn prepare_for_user_input(&mut self, cx: &mut Context<Self>) {
        self.selecting = false;
        self.scroll_accumulator.reset();
        self.cursor_visible = true;
        self.engine.read(cx).prepare_for_user_input();
    }

    fn open_context_menu(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus(window, cx);
        let engine = self.engine.read(cx);
        let has_selection = engine.has_selection();
        let lifecycle = engine.lifecycle().clone();
        let can_clear = engine.accepts_clear();
        let can_paste = lifecycle.accepts_input()
            && cx
                .read_from_clipboard()
                .and_then(|item| item.text())
                .is_some();
        let action_context = self.focus_handle.clone();
        let owner_items = self.menu_items.clone();
        let menu = PopupMenu::build(window, cx, move |menu, _, _| {
            let menu = menu
                .action_context(action_context)
                .menu_with_icon_and_disabled(
                    "复制",
                    Icon::new(IconName::Copy),
                    Box::new(CopyTerminal),
                    !has_selection,
                )
                .menu_with_icon_and_disabled(
                    "粘贴",
                    Icon::new(CatalogIcon::ClipboardPaste),
                    Box::new(PasteTerminal),
                    !can_paste,
                )
                .separator()
                .menu_with_icon(
                    "查找…",
                    Icon::new(IconName::Search),
                    Box::new(FindInTerminal),
                )
                .menu_with_icon_and_disabled(
                    "清屏",
                    Icon::new(CatalogIcon::Eraser),
                    Box::new(ClearTerminal),
                    !can_clear,
                );
            match &owner_items {
                Some(items) => items(menu.separator(), &lifecycle),
                None => menu,
            }
        });
        let terminal = cx.weak_entity();
        let subscription = window.subscribe(&menu, cx, move |_, _: &DismissEvent, _, cx| {
            _ = terminal.update(cx, |terminal, cx| {
                terminal.context_menu = None;
                terminal.context_menu_subscription = None;
                cx.notify();
            });
        });
        menu.focus_handle(cx).focus(window, cx);
        self.context_menu = Some((menu, event.position));
        self.context_menu_subscription = Some(subscription);
        cx.notify();
    }

    fn set_viewport(&mut self, viewport: TerminalViewport, ime_cursor_bounds: Bounds<Pixels>) {
        self.viewport = Some(viewport);
        self.ime_cursor_bounds = Some(ime_cursor_bounds);
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let length = self.marked_text.encode_utf16().count();
        adjusted_range.replace(0..length);
        Some(self.marked_text.clone())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.marked_selection.clone(),
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        (!self.marked_text.is_empty()).then(|| 0..self.marked_text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked_text.clear();
        self.marked_selection = 0..0;
        cx.notify();
    }

    fn paste(&mut self, item: ClipboardItem, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = item.text() {
            self.engine.read(cx).paste(&text);
            self.selecting = false;
            self.scroll_accumulator.reset();
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text.clear();
        self.marked_selection = 0..0;
        self.send_user_input(text.as_bytes().to_vec(), cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.prepare_for_user_input(cx);
        self.marked_text = new_text.to_owned();
        let length = new_text.encode_utf16().count();
        self.marked_selection = new_selected_range.unwrap_or(length..length);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.ime_cursor_bounds
            .or_else(|| self.viewport.map(|viewport| viewport.cursor_bounds))
    }

    fn character_index_for_point(
        &mut self,
        _: gpui_kit::Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.marked_selection.end)
    }

    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.marked_text.encode_utf16().count())
    }

    fn accepts_text_input(&self, _: &mut Window, cx: &mut Context<Self>) -> bool {
        self.engine.read(cx).lifecycle().accepts_input()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = self.engine.read(cx).snapshot();
        let view = cx.entity();
        let context_menu = self.context_menu.clone();
        let element = TerminalElement {
            view: view.clone(),
            snapshot,
            focus_handle: self.focus_handle.clone(),
            cursor_visible: self.cursor_visible,
            marked_text: self.marked_text.clone(),
            marked_selection: self.marked_selection.clone(),
            requested_size: self.requested_size,
        };

        let font = TerminalFont::current(cx);
        let terminal = div()
            .id(self.element_id.clone())
            .test_support()
            .key_context(TERMINAL_KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .font_family(font.family(cx))
            .text_size(font.size)
            .aria_label(self.aria_label.clone())
            .on_action(cx.listener(|this, _: &SendTab, _, cx| {
                this.send_user_input(vec![b'\t'], cx);
            }))
            .on_action(cx.listener(|this, _: &SendBackTab, _, cx| {
                this.send_user_input(b"\x1b[Z".to_vec(), cx);
            }))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::open_context_menu))
            .child(element)
            .when_some(context_menu, |terminal, (menu, position)| {
                terminal.child(
                    deferred(
                        anchored()
                            .position(position)
                            .snap_to_window_with_margin(px(8.))
                            .child(menu),
                    )
                    .with_priority(gpui_kit::base::POPUP_PRIORITY),
                )
            })
            // Below the pointer, so the pointer stays on the link.
            .when_some(
                self.hovered_link.clone().filter(|_| self.link_tooltip),
                |terminal, link| {
                    terminal.child(
                        deferred(
                            anchored()
                                .position(link.position + point(px(0.), px(20.)))
                                .snap_to_window_with_margin(px(8.))
                                .child(link_tooltip(link.uri, window, cx)),
                        )
                        .with_priority(1),
                    )
                },
            );

        // The find bar is the terminal's sibling, not its child: keys typed
        // into it must not bubble to the terminal's key handler, which would
        // send them to the shell (and Tab would hit `SendTab`).
        div()
            .relative()
            .size_full()
            .p_2()
            .child(terminal)
            .when_some(self.find.as_ref(), |container, find| {
                container.child(self.render_find_bar(find, cx))
            })
    }
}

/// Where a link leads and how to open it: the address may not be its text.
fn link_tooltip(uri: SharedString, window: &mut Window, cx: &mut App) -> AnyView {
    Tooltip::element(move |_, cx| {
        v_flex()
            .id("terminal-link-tooltip")
            .test_support()
            .aria_label(uri.clone())
            .max_w(rems(32.))
            .child(uri.clone())
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{OPEN_LINK_KEY} 单击打开")),
            )
    })
    .build(window, cx)
}

impl TerminalView {
    fn render_find_bar(&self, find: &FindBar, cx: &App) -> impl IntoElement {
        let has_query = !find.input.read(cx).value().is_empty();
        let position = self
            .engine
            .read(cx)
            .search_position()
            .filter(|_| has_query && find.pending.is_none());
        let navigate = position.is_some_and(|position| position.total() > 0);
        h_flex()
            .key_context(TERMINAL_FIND_KEY_CONTEXT)
            // Keep the terminal underneath from taking clicks on the bar,
            // which would move focus and start a selection there.
            .occlude()
            .absolute()
            .top_2()
            .right_4()
            .w_80()
            .gap_1()
            .p_1()
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius)
            .shadow_md()
            .child(
                Input::new(&find.input)
                    .id("terminal-find")
                    .small()
                    .appearance(false)
                    .prefix(Icon::new(IconName::Search).small()),
            )
            .when_some(position, |bar, position| {
                let label = position.label();
                bar.child(
                    div()
                        .id("terminal-find-count")
                        .test_support()
                        .aria_label(label.clone())
                        .flex_none()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(label),
                )
            })
            .child(
                Button::new("terminal-find-previous")
                    .ghost()
                    .xsmall()
                    .icon(IconName::ChevronUp)
                    .tooltip("上一项（⇧↩）")
                    .disabled(!navigate)
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(FindPreviousInTerminal), cx)
                    }),
            )
            .child(
                Button::new("terminal-find-next")
                    .ghost()
                    .xsmall()
                    .icon(IconName::ChevronDown)
                    .tooltip("下一项（↩）")
                    .disabled(!navigate)
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(FindNextInTerminal), cx)
                    }),
            )
            .child(
                Button::new("terminal-find-close")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Close)
                    .tooltip("关闭（Esc）")
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(DismissTerminalFind), cx)
                    }),
            )
    }
}

struct TerminalElement {
    view: Entity<TerminalView>,
    snapshot: TerminalSnapshot,
    focus_handle: FocusHandle,
    cursor_visible: bool,
    marked_text: String,
    marked_selection: Range<usize>,
    requested_size: TerminalSize,
}

struct TerminalPaintState {
    hitbox: Hitbox,
    viewport: TerminalViewport,
    lines: Vec<ShapedLine>,
    backgrounds: Vec<PaintQuad>,
    preedit: Vec<PreeditPaintCell>,
    cursor: Option<PaintQuad>,
    ime_cursor_bounds: Bounds<Pixels>,
}

struct PreeditPaintCell {
    bounds: Bounds<Pixels>,
    line: ShapedLine,
    background: Option<PaintQuad>,
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = TerminalPaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let bounds = window.pixel_snap_bounds(bounds);
        let text_style = window.text_style();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let measure_run = TextRun {
            len: 1,
            font: text_style.font(),
            color: text_style.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let cell_width = window
            .text_system()
            .shape_line("M".into(), font_size, &[measure_run], None)
            .width()
            .max(px(1.));
        let line_height = TerminalFont::current(cx).row_height(window);
        let columns = ((bounds.size.width / cell_width).floor() as usize).max(2);
        let rows = ((bounds.size.height / line_height).floor() as usize).max(1);
        let requested_size = TerminalSize::new(
            columns,
            rows,
            cell_width.as_f32().round().clamp(1., u16::MAX as f32) as u16,
            line_height.as_f32().round().clamp(1., u16::MAX as f32) as u16,
        );
        if requested_size != self.requested_size {
            let terminal = self.view.clone();
            cx.defer(move |cx| {
                terminal.update(cx, |terminal, cx| terminal.resize(requested_size, cx));
            });
        }

        let palette = TerminalPalette::new(cx);
        let geometry = TerminalGeometry {
            bounds,
            cell_width,
            line_height,
        };
        // The link under the pointer, lit a little brighter.
        let hovered_link = screen_cell(
            &geometry,
            self.snapshot.columns,
            self.snapshot.rows,
            window.mouse_position(),
        )
        .and_then(|(column, row)| {
            self.snapshot
                .cells
                .get(row * self.snapshot.columns + column)?
                .link
        });
        let mut lines = Vec::with_capacity(self.snapshot.rows);
        let mut backgrounds = Vec::new();
        for (row_index, row) in self
            .snapshot
            .cells
            .chunks(self.snapshot.columns)
            .enumerate()
        {
            let mut text = String::new();
            let mut runs = Vec::new();
            let mut background_start = 0;
            let mut background_color = None;

            for (column, cell) in row.iter().enumerate() {
                let (foreground, background) = palette.cell_colors(
                    cell.foreground,
                    cell.background,
                    cell.flags,
                    &self.snapshot.colors,
                );
                let cell_background = if cell.selected {
                    cx.theme().selection
                } else {
                    match cell.search {
                        SearchMark::Focused => cx.theme().warning,
                        SearchMark::Match => cx.theme().warning.opacity(0.35),
                        SearchMark::None => background,
                    }
                };
                if column == 0 {
                    background_color = Some(cell_background);
                } else if background_color != Some(cell_background) {
                    push_background(
                        &mut backgrounds,
                        geometry,
                        background_start..column,
                        row_index,
                        background_color.unwrap_or(palette.background),
                    );
                    background_start = column;
                    background_color = Some(cell_background);
                }

                let character = painted_cell_text(&cell.character, cell.flags);
                let font = terminal_font(text_style.font(), cell.flags);
                let len = character.len();
                text.push_str(character);
                // Links always show as links, blue whatever color the
                // program gave them. Not the theme's `link`: that is the
                // color of text.
                let link = cell.link.map(|link| {
                    if hovered_link == Some(link) {
                        cx.theme().blue_light
                    } else {
                        cx.theme().blue
                    }
                });
                runs.push(TextRun {
                    len,
                    font,
                    color: if cell.selected {
                        palette.foreground
                    } else if cell.search == SearchMark::Focused {
                        cx.theme().warning_foreground
                    } else {
                        link.unwrap_or(foreground)
                    },
                    background_color: None,
                    underline: match link {
                        Some(color) => Some(UnderlineStyle {
                            thickness: px(1.),
                            color: Some(color),
                            wavy: false,
                        }),
                        None => {
                            cell.flags
                                .intersects(Flags::ALL_UNDERLINES)
                                .then_some(UnderlineStyle {
                                    thickness: px(1.),
                                    color: Some(foreground),
                                    wavy: cell.flags.contains(Flags::UNDERCURL),
                                })
                        }
                    },
                    strikethrough: cell.flags.contains(Flags::STRIKEOUT).then_some(
                        StrikethroughStyle {
                            thickness: px(1.),
                            color: Some(foreground),
                        },
                    ),
                });
            }
            push_background(
                &mut backgrounds,
                geometry,
                background_start..row.len(),
                row_index,
                background_color.unwrap_or(palette.background),
            );
            let line = window.text_system().shape_line(
                text.into(),
                font_size,
                &runs,
                // `force_width` is the width of one glyph cell, not the maximum
                // width of the complete line. Passing the row width here sends
                // every character after the first one towards the right edge.
                Some(cell_width),
            );
            lines.push(line);
        }

        let cursor_bounds = geometry.cell_bounds(
            self.snapshot
                .cursor_column
                .min(self.snapshot.columns.saturating_sub(1)),
            self.snapshot
                .cursor_row
                .unwrap_or_default()
                .min(self.snapshot.rows.saturating_sub(1)),
            1,
        );
        let viewport = TerminalViewport {
            geometry,
            columns: self.snapshot.columns,
            rows: self.snapshot.rows,
            display_offset: self.snapshot.display_offset,
            cursor_bounds,
        };
        let (preedit, ime_cursor_bounds) = build_preedit(
            &self.marked_text,
            self.marked_selection.clone(),
            self.snapshot.cursor_column,
            self.snapshot.cursor_row,
            viewport,
            text_style.font(),
            font_size,
            &palette,
            cx.theme().selection,
            window,
        );
        let cursor = self
            .snapshot
            .cursor_row
            .filter(|_| {
                self.marked_text.is_empty()
                    && self.cursor_visible
                    && self.focus_handle.is_focused(window)
            })
            .and_then(|_| cursor_quad(cursor_bounds, self.snapshot.cursor_shape, &palette));

        TerminalPaintState {
            hitbox: window.insert_hitbox(bounds, HitboxBehavior::Normal),
            viewport,
            lines,
            backgrounds,
            preedit,
            cursor,
            ime_cursor_bounds,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        state: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.handle_input(
            &self.focus_handle,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );
        let view = self.view.clone();
        let viewport = state.viewport;
        let ime_cursor_bounds = state.ime_cursor_bounds;
        view.update(cx, |view, _| view.set_viewport(viewport, ime_cursor_bounds));

        let mask = ContentMask { bounds };
        window.with_content_mask(Some(mask), |window| {
            for background in state.backgrounds.drain(..) {
                window.paint_quad(background);
            }
            for (row, line) in state.lines.iter().enumerate() {
                let _ = line.paint(
                    point(
                        bounds.left(),
                        bounds.top() + state.viewport.geometry.line_height * row as f32,
                    ),
                    state.viewport.geometry.line_height,
                    TextAlign::Left,
                    Some(bounds.size.width),
                    window,
                    cx,
                );
            }
            for cell in state.preedit.iter_mut() {
                if let Some(background) = cell.background.take() {
                    window.paint_quad(background);
                }
                let _ = cell.line.paint(
                    cell.bounds.origin,
                    cell.bounds.size.height,
                    TextAlign::Left,
                    Some(cell.bounds.size.width),
                    window,
                    cx,
                );
            }
            if let Some(cursor) = state.cursor.take() {
                window.paint_quad(cursor);
            }
        });

        let hitbox = state.hitbox.clone();
        let view = self.view.clone();
        let viewport = state.viewport;
        let links = Rc::new(ScreenLinks::new(&self.snapshot));

        // A hand over a link while the key that opens it is held; pressing
        // or letting go of the key changes it at once.
        let pointer_on_link = hitbox.is_hovered(window)
            && viewport
                .cell_at(window.mouse_position())
                .and_then(|cell| links.at(cell))
                .is_some();
        if pointer_on_link {
            if opens_links(&window.modifiers()) {
                window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
            }
            let view = view.clone();
            window.on_modifiers_changed(move |_, _, cx| view.update(cx, |_, cx| cx.notify()));
        }

        // A click opens a link with ⌘ held; otherwise it goes to the program
        // that asked for the mouse, or selects text.
        window.on_mouse_event({
            let (hitbox, view, links) = (hitbox.clone(), view.clone(), links.clone());
            move |event: &MouseDownEvent, phase, window, cx| {
                if !phase.bubble() || !hitbox.is_hovered(window) {
                    return;
                }
                let Some(cell) = viewport.cell_at(event.position) else {
                    return;
                };
                let button = match event.button {
                    MouseButton::Left => ReportButton::Left,
                    MouseButton::Middle => ReportButton::Middle,
                    _ => return,
                };
                if button == ReportButton::Left
                    && opens_links(&event.modifiers)
                    && let Some((_, uri)) = links.at(cell)
                {
                    cx.open_url(uri);
                    view.update(cx, |view, cx| view.hover_link(None, cx));
                    cx.stop_propagation();
                    return;
                }
                let reported = view.update(cx, |view, cx| {
                    view.focus(window, cx);
                    view.report_mouse(button, ReportKind::Press, cell, event.modifiers, cx)
                });
                if reported {
                    cx.stop_propagation();
                } else if button == ReportButton::Left {
                    let (point, side) = viewport.grid_point(event.position);
                    let selection_type = match event.click_count {
                        2 => SelectionType::Semantic,
                        count if count >= 3 => SelectionType::Lines,
                        _ => SelectionType::Simple,
                    };
                    view.update(cx, |view, cx| {
                        view.start_selection(point, side, selection_type, cx);
                    });
                    cx.stop_propagation();
                }
            }
        });

        // The link under the pointer, for its tooltip; and motion, for a
        // program that asked for it. A drag that leaves the terminal is
        // reported at its edge.
        window.on_mouse_event({
            let (hitbox, view, links) = (hitbox.clone(), view.clone(), links.clone());
            move |event: &MouseMoveEvent, phase, window, cx| {
                if !phase.bubble() {
                    return;
                }
                let cell = hitbox
                    .is_hovered(window)
                    .then(|| viewport.cell_at(event.position))
                    .flatten();
                let link = cell
                    .and_then(|cell| links.at(cell))
                    .map(|(index, uri)| HoveredLink {
                        index,
                        uri: uri.clone(),
                        position: event.position,
                    });
                view.update(cx, |view, cx| {
                    view.hover_link(link, cx);
                    let cell = match (cell, view.reported_button) {
                        (_, Some(_)) => Some(viewport.nearest_cell(event.position)),
                        (cell, None) => cell,
                    };
                    if let Some(cell) = cell {
                        view.report_mouse(
                            ReportButton::None,
                            ReportKind::Motion,
                            cell,
                            event.modifiers,
                            cx,
                        );
                    }
                });
            }
        });

        // A reported button going up is reported wherever it is let go.
        window.on_mouse_event({
            let view = view.clone();
            move |event: &MouseUpEvent, phase, _, cx| {
                if !phase.capture() {
                    return;
                }
                let button = match event.button {
                    MouseButton::Left => ReportButton::Left,
                    MouseButton::Middle => ReportButton::Middle,
                    _ => return,
                };
                let cell = viewport.nearest_cell(event.position);
                view.update(cx, |view, cx| {
                    view.release_mouse(button, cell, event.modifiers, cx)
                });
            }
        });

        let view = self.view.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
            if !phase.capture() || !view.read(cx).selecting {
                return;
            }
            if event.dragging() {
                let (point, side) = viewport.grid_point(event.position);
                view.update(cx, |view, cx| view.update_selection(point, side, cx));
            } else {
                view.update(cx, |view, _| view.selecting = false);
            }
        });

        let view = self.view.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
            if event.button != MouseButton::Left || !phase.capture() || !view.read(cx).selecting {
                return;
            }
            let (point, side) = viewport.grid_point(event.position);
            view.update(cx, |view, cx| {
                view.update_selection(point, side, cx);
                view.selecting = false;
            });
        });

        let hitbox = state.hitbox.clone();
        let view = self.view.clone();
        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
            if !phase.bubble() || !hitbox.should_handle_scroll(window) {
                return;
            }
            let cell = viewport.nearest_cell(event.position);
            view.update(cx, |view, cx| {
                view.scroll(
                    event.delta,
                    viewport.geometry.line_height,
                    cell,
                    event.modifiers,
                    cx,
                )
            });
            cx.stop_propagation();
        });
    }
}

fn painted_cell_text(character: &str, flags: Flags) -> &str {
    if flags.intersects(Flags::HIDDEN | Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
        // Keep one shaped glyph for every emulator grid cell. In particular, a
        // wide-character spacer must remain present so the following glyph does
        // not move one column to the left.
        " "
    } else {
        character
    }
}

fn terminal_font(mut font: Font, flags: Flags) -> Font {
    // BOLD_ITALIC and DIM_BOLD are aliases composed from the basic flag bits.
    // Testing them with `intersects` would make every bold glyph italic and
    // every dim glyph bold, which is both semantically wrong and visibly soft.
    if flags.contains(Flags::BOLD) {
        font = font.bold();
    }
    if flags.contains(Flags::ITALIC) {
        font = font.italic();
    }
    font
}

#[derive(Debug, PartialEq, Eq)]
struct PreeditCellLayout {
    text: String,
    width: usize,
    utf16_range: Range<usize>,
}

fn preedit_cell_layouts(text: &str) -> Vec<PreeditCellLayout> {
    let mut layouts: Vec<PreeditCellLayout> = Vec::new();
    let mut utf16_offset = 0;

    for character in text.chars() {
        let utf16_len = character.len_utf16();
        let width = character.width().unwrap_or(1);
        if width == 0
            && let Some(previous) = layouts.last_mut()
        {
            previous.text.push(character);
            previous.utf16_range.end += utf16_len;
        } else {
            layouts.push(PreeditCellLayout {
                text: character.to_string(),
                width: width.max(1),
                utf16_range: utf16_offset..utf16_offset + utf16_len,
            });
        }
        utf16_offset += utf16_len;
    }

    layouts
}

fn advance_grid_position(column: &mut usize, row: &mut usize, width: usize, columns: usize) {
    if *column + width > columns {
        *column = 0;
        *row = row.saturating_add(1);
    }
    *column += width;
    if *column >= columns {
        *column = 0;
        *row = row.saturating_add(1);
    }
}

fn preedit_caret_position(
    layouts: &[PreeditCellLayout],
    utf16_index: usize,
    cursor_column: usize,
    cursor_row: usize,
    columns: usize,
) -> (usize, usize) {
    let mut column = cursor_column.min(columns.saturating_sub(1));
    let mut row = cursor_row;
    if utf16_index == 0 {
        return (column, row);
    }

    for layout in layouts {
        let width = layout.width.min(columns).max(1);
        if column + width > columns {
            column = 0;
            row = row.saturating_add(1);
        }
        if utf16_index <= layout.utf16_range.start {
            return (column, row);
        }
        advance_grid_position(&mut column, &mut row, width, columns);
        if utf16_index <= layout.utf16_range.end {
            return (column, row);
        }
    }

    (column, row)
}

#[allow(clippy::too_many_arguments)]
fn build_preedit(
    text: &str,
    selection: Range<usize>,
    cursor_column: usize,
    cursor_row: Option<usize>,
    viewport: TerminalViewport,
    font: Font,
    font_size: Pixels,
    palette: &TerminalPalette,
    selection_color: Hsla,
    window: &mut Window,
) -> (Vec<PreeditPaintCell>, Bounds<Pixels>) {
    let Some(cursor_row) = cursor_row else {
        return (Vec::new(), viewport.cursor_bounds);
    };
    if text.is_empty() {
        return (Vec::new(), viewport.cursor_bounds);
    }

    let layouts = preedit_cell_layouts(text);
    let text_utf16_len = text.encode_utf16().count();
    let caret_index = selection.end.min(text_utf16_len);
    let selection = selection.start.min(text_utf16_len)..selection.end.min(text_utf16_len);
    let mut column = cursor_column.min(viewport.columns.saturating_sub(1));
    let mut row = cursor_row.min(viewport.rows.saturating_sub(1));
    let caret = preedit_caret_position(&layouts, caret_index, column, row, viewport.columns);
    let mut painted = Vec::with_capacity(layouts.len());

    for layout in layouts {
        let width = layout.width.min(viewport.columns).max(1);
        if column + width > viewport.columns {
            column = 0;
            row = row.saturating_add(1);
        }
        let cell_bounds = viewport.geometry.cell_bounds(column, row, width);
        if row < viewport.rows {
            let selected = !selection.is_empty()
                && selection.start < layout.utf16_range.end
                && selection.end > layout.utf16_range.start;
            let run = TextRun {
                len: layout.text.len(),
                font: font.clone(),
                color: palette.foreground,
                background_color: None,
                underline: Some(UnderlineStyle {
                    thickness: px(1.),
                    color: Some(palette.cursor),
                    wavy: false,
                }),
                strikethrough: None,
            };
            let line = window.text_system().shape_line(
                layout.text.into(),
                font_size,
                &[run],
                Some(viewport.geometry.cell_width * width as f32),
            );
            painted.push(PreeditPaintCell {
                bounds: cell_bounds,
                line,
                background: selected.then(|| fill(cell_bounds, selection_color)),
            });
        }

        advance_grid_position(&mut column, &mut row, width, viewport.columns);
    }

    let caret_column = caret.0.min(viewport.columns.saturating_sub(1));
    let caret_row = caret.1.min(viewport.rows.saturating_sub(1));
    (
        painted,
        viewport.geometry.cell_bounds(caret_column, caret_row, 1),
    )
}

fn push_background(
    quads: &mut Vec<PaintQuad>,
    geometry: TerminalGeometry,
    columns: Range<usize>,
    row: usize,
    color: Hsla,
) {
    if columns.is_empty() {
        return;
    }
    quads.push(fill(
        geometry.cell_bounds(columns.start, row, columns.len()),
        color,
    ));
}

fn cursor_quad(
    bounds: Bounds<Pixels>,
    shape: CursorShape,
    palette: &TerminalPalette,
) -> Option<PaintQuad> {
    let cursor = match shape {
        CursorShape::Hidden => return None,
        CursorShape::Block => bounds,
        CursorShape::Underline => Bounds::new(
            point(bounds.left(), bounds.bottom() - px(2.)),
            size(bounds.size.width, px(2.)),
        ),
        CursorShape::Beam => Bounds::new(bounds.origin, size(px(2.), bounds.size.height)),
        CursorShape::HollowBlock => {
            return Some(outline(bounds, palette.cursor, BorderStyle::Solid));
        }
    };
    Some(fill(cursor, palette.cursor.opacity(0.75)))
}

/// The cell on screen under `position`, if it is over the grid.
fn screen_cell(
    geometry: &TerminalGeometry,
    columns: usize,
    rows: usize,
    position: gpui_kit::Point<Pixels>,
) -> Option<(usize, usize)> {
    let local = position - geometry.bounds.origin;
    if local.x < px(0.) || local.y < px(0.) || columns == 0 || rows == 0 {
        return None;
    }
    let column = (local.x / geometry.cell_width).floor() as usize;
    let row = (local.y / geometry.line_height).floor() as usize;
    (column < columns && row < rows).then_some((column, row))
}

/// Which link each cell on screen belongs to, for the pointer's handlers.
struct ScreenLinks {
    columns: usize,
    cells: Vec<Option<usize>>,
    links: Vec<SharedString>,
}

impl ScreenLinks {
    fn new(snapshot: &TerminalSnapshot) -> Self {
        Self {
            columns: snapshot.columns,
            cells: snapshot.cells.iter().map(|cell| cell.link).collect(),
            links: snapshot.links.clone(),
        }
    }

    fn at(&self, (column, row): (usize, usize)) -> Option<(usize, &SharedString)> {
        let link = (*self.cells.get(row * self.columns + column)?)?;
        Some((link, self.links.get(link)?))
    }
}

fn grid_point(
    position: gpui_kit::Point<Pixels>,
    bounds: Bounds<Pixels>,
    cell_width: Pixels,
    line_height: Pixels,
    display_offset: usize,
    columns: usize,
    rows: usize,
) -> (TerminalPoint, Side) {
    let grid_width = cell_width.as_f32() * columns as f32;
    let local_x = (position.x - bounds.left()).as_f32();
    let local_y = (position.y - bounds.top()).as_f32();
    let clamped_x = local_x.clamp(0., grid_width.max(0.));
    let clamped_y = local_y.clamp(0., line_height.as_f32() * rows as f32);
    let column =
        ((clamped_x / cell_width.as_f32()).floor() as usize).min(columns.saturating_sub(1));
    let row =
        ((clamped_y / line_height.as_f32()).floor() as usize).min(rows.saturating_sub(1)) as i32;
    let side = if local_x <= 0. {
        Side::Left
    } else if local_x >= grid_width {
        Side::Right
    } else if (clamped_x % cell_width.as_f32()) / cell_width.as_f32() < 0.5 {
        Side::Left
    } else {
        Side::Right
    };
    (
        TerminalPoint::new(Line(row - display_offset as i32), Column(column)),
        side,
    )
}

struct TerminalPalette {
    foreground: Hsla,
    background: Hsla,
    cursor: Hsla,
}

impl TerminalPalette {
    fn new(cx: &App) -> Self {
        Self {
            foreground: cx.theme().foreground,
            background: cx.theme().background,
            cursor: cx.theme().primary,
        }
    }

    fn cell_colors(
        &self,
        foreground: Color,
        background: Color,
        flags: Flags,
        overrides: &[Option<Rgb>; COUNT],
    ) -> (Hsla, Hsla) {
        let mut foreground = self.resolve(foreground, overrides);
        let mut background = self.resolve(background, overrides);
        if flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut foreground, &mut background);
        }
        if flags.contains(Flags::DIM) {
            foreground = foreground.opacity(0.66);
        }
        (foreground, background)
    }

    fn resolve(&self, color: Color, overrides: &[Option<Rgb>; COUNT]) -> Hsla {
        let index = match color {
            Color::Spec(color) => return rgb_to_hsla(color),
            Color::Indexed(index) => index as usize,
            Color::Named(color) => color as usize,
        };
        if let Some(color) = overrides.get(index).copied().flatten() {
            return rgb_to_hsla(color);
        }
        match color {
            Color::Named(NamedColor::Foreground | NamedColor::BrightForeground) => self.foreground,
            Color::Named(NamedColor::DimForeground) => self.foreground.opacity(0.66),
            Color::Named(NamedColor::Background) => self.background,
            Color::Named(NamedColor::Cursor) => self.cursor,
            Color::Named(NamedColor::DimBlack) => indexed_color(0).opacity(0.66),
            Color::Named(NamedColor::DimRed) => indexed_color(1).opacity(0.66),
            Color::Named(NamedColor::DimGreen) => indexed_color(2).opacity(0.66),
            Color::Named(NamedColor::DimYellow) => indexed_color(3).opacity(0.66),
            Color::Named(NamedColor::DimBlue) => indexed_color(4).opacity(0.66),
            Color::Named(NamedColor::DimMagenta) => indexed_color(5).opacity(0.66),
            Color::Named(NamedColor::DimCyan) => indexed_color(6).opacity(0.66),
            Color::Named(NamedColor::DimWhite) => indexed_color(7).opacity(0.66),
            Color::Named(named) => indexed_color(named as u8),
            Color::Indexed(index) => indexed_color(index),
            Color::Spec(_) => unreachable!(),
        }
    }
}

fn rgb_to_hsla(color: Rgb) -> Hsla {
    rgb(((color.r as u32) << 16) | ((color.g as u32) << 8) | color.b as u32).into()
}

fn indexed_color(index: u8) -> Hsla {
    const ANSI: [u32; 16] = [
        0x000000, 0xcd0000, 0x00cd00, 0xcdcd00, 0x0000ee, 0xcd00cd, 0x00cdcd, 0xe5e5e5, 0x7f7f7f,
        0xff0000, 0x00ff00, 0xffff00, 0x5c5cff, 0xff00ff, 0x00ffff, 0xffffff,
    ];
    let value = match index {
        0..=15 => ANSI[index as usize],
        16..=231 => {
            let index = index - 16;
            let component = |value: u8| {
                if value == 0 {
                    0
                } else {
                    55 + value as u32 * 40
                }
            };
            (component(index / 36) << 16)
                | (component((index % 36) / 6) << 8)
                | component(index % 6)
        }
        232..=255 => {
            let value = 8 + (index as u32 - 232) * 10;
            (value << 16) | (value << 8) | value
        }
    };
    rgb(value).into()
}

pub(crate) fn encode_key(keystroke: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    if keystroke.modifiers.platform {
        return None;
    }
    let modifiers = keystroke.modifiers;
    let key = keystroke.key.as_str();
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let modifier = 1
        + u8::from(modifiers.shift)
        + u8::from(modifiers.alt) * 2
        + u8::from(modifiers.control) * 4;
    let modified_csi = |final_byte: char| format!("\x1b[1;{modifier}{final_byte}").into_bytes();
    let mut sequence = match key {
        "enter" => vec![b'\r'],
        "tab" if modifiers.shift => b"\x1b[Z".to_vec(),
        "tab" => vec![b'\t'],
        "backspace" => vec![0x7f],
        "escape" => vec![0x1b],
        "up" if modifier > 1 => modified_csi('A'),
        "down" if modifier > 1 => modified_csi('B'),
        "right" if modifier > 1 => modified_csi('C'),
        "left" if modifier > 1 => modified_csi('D'),
        "up" => if app_cursor { b"\x1bOA" } else { b"\x1b[A" }.to_vec(),
        "down" => if app_cursor { b"\x1bOB" } else { b"\x1b[B" }.to_vec(),
        "right" => if app_cursor { b"\x1bOC" } else { b"\x1b[C" }.to_vec(),
        "left" => if app_cursor { b"\x1bOD" } else { b"\x1b[D" }.to_vec(),
        "home" if modifier > 1 => modified_csi('H'),
        "end" if modifier > 1 => modified_csi('F'),
        "home" => b"\x1b[H".to_vec(),
        "end" => b"\x1b[F".to_vec(),
        "insert" => b"\x1b[2~".to_vec(),
        "delete" => b"\x1b[3~".to_vec(),
        "pageup" => b"\x1b[5~".to_vec(),
        "pagedown" => b"\x1b[6~".to_vec(),
        "f1" => b"\x1bOP".to_vec(),
        "f2" => b"\x1bOQ".to_vec(),
        "f3" => b"\x1bOR".to_vec(),
        "f4" => b"\x1bOS".to_vec(),
        "f5" => b"\x1b[15~".to_vec(),
        "f6" => b"\x1b[17~".to_vec(),
        "f7" => b"\x1b[18~".to_vec(),
        "f8" => b"\x1b[19~".to_vec(),
        "f9" => b"\x1b[20~".to_vec(),
        "f10" => b"\x1b[21~".to_vec(),
        "f11" => b"\x1b[23~".to_vec(),
        "f12" => b"\x1b[24~".to_vec(),
        _ if modifiers.control => {
            let character = keystroke.key_char.as_deref().unwrap_or(key);
            let byte = character.as_bytes().first().copied()?;
            match byte.to_ascii_lowercase() {
                b'a'..=b'z' => vec![byte.to_ascii_lowercase() - b'a' + 1],
                b' ' | b'@' => vec![0],
                b'[' => vec![0x1b],
                b'\\' => vec![0x1c],
                b']' => vec![0x1d],
                b'^' => vec![0x1e],
                b'_' => vec![0x1f],
                b'?' => vec![0x7f],
                _ => return None,
            }
        }
        _ if modifiers.alt => keystroke.key_char.as_ref()?.as_bytes().to_vec(),
        _ => return None,
    };
    if modifiers.alt && !matches!(key, "up" | "down" | "right" | "left" | "home" | "end") {
        sequence.insert(0, 0x1b);
    }
    Some(sequence)
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::term::TermMode;
    use alacritty_terminal::term::cell::Flags;
    use gpui_kit::{Bounds, FontStyle, FontWeight, Keystroke, ScrollDelta, font, point, px, size};

    use super::{
        ScrollAccumulator, ScrollTarget, encode_key, grid_point, painted_cell_text,
        preedit_caret_position, preedit_cell_layouts, terminal_font,
    };

    fn key(source: &str) -> Keystroke {
        Keystroke::parse(source).unwrap()
    }

    #[test]
    fn encodes_control_and_cursor_keys() {
        assert_eq!(
            encode_key(&key("ctrl-c"), TermMode::default()),
            Some(vec![3])
        );
        assert_eq!(
            encode_key(&key("up"), TermMode::APP_CURSOR),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            encode_key(&key("shift-up"), TermMode::default()),
            Some(b"\x1b[1;2A".to_vec())
        );
    }

    #[test]
    fn leaves_platform_shortcuts_for_actions() {
        assert_eq!(encode_key(&key("cmd-t"), TermMode::default()), None);
    }

    #[test]
    fn wide_character_spacers_keep_their_grid_column() {
        assert_eq!(painted_cell_text("你", Flags::WIDE_CHAR), "你");
        assert_eq!(painted_cell_text("", Flags::WIDE_CHAR_SPACER), " ");
        assert_eq!(painted_cell_text("", Flags::LEADING_WIDE_CHAR_SPACER), " ");
        assert_eq!(painted_cell_text("secret", Flags::HIDDEN), " ");
    }

    #[test]
    fn ansi_bold_does_not_accidentally_make_text_italic() {
        let bold = terminal_font(font("Menlo"), Flags::BOLD);
        assert_eq!(bold.weight, FontWeight::BOLD);
        assert_eq!(bold.style, FontStyle::Normal);

        let italic = terminal_font(font("Menlo"), Flags::ITALIC);
        assert_eq!(italic.weight, FontWeight::NORMAL);
        assert_eq!(italic.style, FontStyle::Italic);
    }

    #[test]
    fn grid_hit_testing_clamps_to_viewport_and_applies_scrollback_offset() {
        let bounds = Bounds::new(point(px(10.), px(20.)), size(px(80.), px(40.)));
        let (top_left, left_side) =
            grid_point(point(px(0.), px(0.)), bounds, px(10.), px(20.), 3, 8, 2);
        assert_eq!(top_left.line.0, -3);
        assert_eq!(top_left.column.0, 0);
        assert_eq!(left_side, alacritty_terminal::index::Side::Left);

        let (bottom_right, right_side) =
            grid_point(point(px(200.), px(200.)), bounds, px(10.), px(20.), 3, 8, 2);
        assert_eq!(bottom_right.line.0, -2);
        assert_eq!(bottom_right.column.0, 7);
        assert_eq!(right_side, alacritty_terminal::index::Side::Right);
    }

    #[test]
    fn pixel_scrolling_accumulates_full_lines_and_resets_between_modes() {
        let mut accumulator = ScrollAccumulator::default();
        assert_eq!(
            accumulator.consume(
                ScrollDelta::Pixels(point(px(0.), px(4.))),
                px(10.),
                ScrollTarget::History,
            ),
            0
        );
        assert_eq!(
            accumulator.consume(
                ScrollDelta::Pixels(point(px(0.), px(7.))),
                px(10.),
                ScrollTarget::History,
            ),
            1
        );
        assert_eq!(
            accumulator.consume(
                ScrollDelta::Pixels(point(px(0.), px(-12.))),
                px(10.),
                ScrollTarget::History,
            ),
            -1
        );

        assert_eq!(
            accumulator.consume(
                ScrollDelta::Pixels(point(px(0.), px(9.))),
                px(10.),
                ScrollTarget::AlternateScreen,
            ),
            0
        );
        assert_eq!(
            accumulator.consume(
                ScrollDelta::Pixels(point(px(0.), px(2.))),
                px(10.),
                ScrollTarget::AlternateScreen,
            ),
            1
        );
    }

    #[test]
    fn preedit_layout_uses_utf16_ranges_and_terminal_cell_widths() {
        let layouts = preedit_cell_layouts("a你e\u{301}😀");
        assert_eq!(layouts.len(), 4);
        assert_eq!(layouts[0].width, 1);
        assert_eq!(layouts[1].width, 2);
        assert_eq!(layouts[1].utf16_range, 1..2);
        assert_eq!(layouts[2].text, "e\u{301}");
        assert_eq!(layouts[2].utf16_range, 2..4);
        assert_eq!(layouts[3].width, 2);
        assert_eq!(layouts[3].utf16_range, 4..6);

        assert_eq!(preedit_caret_position(&layouts, 2, 2, 3, 5), (0, 4));
        assert_eq!(preedit_caret_position(&layouts, 6, 2, 3, 5), (3, 4));
    }
}
