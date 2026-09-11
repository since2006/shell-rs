use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::COUNT;
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, CursorStyle, Processor, Rgb};

use super::{
    SharedTerminalTransportFactory, TerminalLifecycle, TerminalSize, TerminalStatus,
    TerminalTransportCommand, TerminalTransportEvent,
};

pub type AlacrittyTerm = Term<TerminalEventProxy>;

#[derive(Clone, Debug)]
pub struct TerminalCell {
    pub character: String,
    pub foreground: Color,
    pub background: Color,
    pub flags: Flags,
    pub selected: bool,
}

#[derive(Clone, Debug)]
pub struct TerminalSnapshot {
    pub columns: usize,
    pub rows: usize,
    pub cells: Vec<TerminalCell>,
    pub cursor_row: Option<usize>,
    pub cursor_column: usize,
    pub cursor_shape: CursorShape,
    pub cursor_blinking: bool,
    pub display_offset: usize,
    pub colors: [Option<Rgb>; COUNT],
    pub mode: TermMode,
}

impl TerminalSnapshot {
    pub fn visible_text(&self) -> String {
        self.cells
            .chunks(self.columns)
            .map(|row| {
                row.iter()
                    .filter(|cell| !cell.flags.contains(Flags::WIDE_CHAR_SPACER))
                    .map(|cell| cell.character.as_str())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub struct TerminalEngine {
    factory: SharedTerminalTransportFactory,
    runtime: TerminalRuntime,
    lifecycle: TerminalLifecycle,
    title: Option<String>,
    generation: u64,
    size: TerminalSize,
    event_sender: mpsc::Sender<TerminalUiEvent>,
    event_task: Option<gpui_kit::Task<()>>,
}

impl TerminalEngine {
    pub fn new(factory: SharedTerminalTransportFactory, cx: &mut gpui_kit::Context<Self>) -> Self {
        let size = TerminalSize::DEFAULT;
        let generation = 1;
        // A timer drains this standard channel on the UI thread. Parser
        // threads therefore never wake GPUI's local executor directly, and a
        // burst of terminal bytes results in a single render notification.
        let (event_sender, event_receiver) = mpsc::channel();
        let runtime =
            TerminalRuntime::start(generation, size, factory.create(), event_sender.clone());
        let mut this = Self {
            factory,
            runtime,
            lifecycle: TerminalLifecycle::Starting,
            title: None,
            generation,
            size,
            event_sender,
            event_task: None,
        };
        this.event_task = Some(Self::listen(event_receiver, cx));
        this
    }

    fn listen(
        events: mpsc::Receiver<TerminalUiEvent>,
        cx: &mut gpui_kit::Context<Self>,
    ) -> gpui_kit::Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                let batch: Vec<_> = events.try_iter().collect();
                if !batch.is_empty()
                    && this
                        .update(cx, |this, cx| {
                            for event in batch {
                                this.handle_event(event);
                            }
                            cx.emit(TerminalEngineEvent::Changed);
                            cx.notify();
                        })
                        .is_err()
                {
                    break;
                }
            }
        })
    }

    fn handle_event(&mut self, event: TerminalUiEvent) {
        if event.generation != self.generation {
            return;
        }

        match event.kind {
            TerminalUiEventKind::Started => self.lifecycle = TerminalLifecycle::Running,
            TerminalUiEventKind::Wakeup => {
                self.runtime.wakeup_pending.store(false, Ordering::Release);
            }
            TerminalUiEventKind::Title(title) => self.title = sanitize_title(&title),
            TerminalUiEventKind::ResetTitle => self.title = None,
            TerminalUiEventKind::Exited { code, signal } => {
                self.lifecycle = TerminalLifecycle::Exited { code, signal };
            }
            TerminalUiEventKind::Failed(error) => {
                self.lifecycle = TerminalLifecycle::Failed(error);
            }
            TerminalUiEventKind::ColorRequest(index, formatter) => {
                let color = self.runtime.term.lock().colors()[index]
                    .unwrap_or_else(|| default_query_color(index));
                self.runtime.write(formatter(color).into_bytes());
            }
        }
    }

    pub fn restart(&mut self, cx: &mut gpui_kit::Context<Self>) {
        self.generation = self.generation.saturating_add(1);
        self.runtime.shutdown();
        self.lifecycle = TerminalLifecycle::Starting;
        self.title = None;
        self.runtime = TerminalRuntime::start(
            self.generation,
            self.size,
            self.factory.create(),
            self.event_sender.clone(),
        );
        cx.emit(TerminalEngineEvent::Changed);
        cx.notify();
    }

    pub fn shutdown(&mut self, cx: &mut gpui_kit::Context<Self>) {
        self.generation = self.generation.saturating_add(1);
        self.lifecycle = TerminalLifecycle::Closing;
        self.runtime.shutdown();
        cx.emit(TerminalEngineEvent::Changed);
        cx.notify();
    }

    pub fn stop(&mut self, message: &str, cx: &mut gpui_kit::Context<Self>) {
        self.generation = self.generation.saturating_add(1);
        self.runtime.shutdown();
        append_message(&self.runtime.term, message);
        self.lifecycle = TerminalLifecycle::Exited {
            code: 0,
            signal: None,
        };
        cx.emit(TerminalEngineEvent::Changed);
        cx.notify();
    }

    pub fn append_system_message(&mut self, message: &str, cx: &mut gpui_kit::Context<Self>) {
        append_message(&self.runtime.term, message);
        cx.emit(TerminalEngineEvent::Changed);
        cx.notify();
    }

    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        if self.lifecycle.accepts_input() {
            self.runtime.write(bytes.into());
        }
    }

    /// Send bytes originating from the user. Unlike protocol writes such as
    /// terminal query responses and focus reports, interactive input returns
    /// to the live screen and retires the previous selection first.
    pub fn send_user_input(&self, bytes: impl Into<Vec<u8>>) {
        if !self.lifecycle.accepts_input() {
            return;
        }

        let mut term = self.runtime.term.lock();
        prepare_term_for_user_input(&mut term);
        drop(term);
        self.runtime.write(bytes.into());
    }

    pub fn prepare_for_user_input(&self) {
        if !self.lifecycle.accepts_input() {
            return;
        }

        let mut term = self.runtime.term.lock();
        prepare_term_for_user_input(&mut term);
    }

    pub fn paste(&self, text: &str) {
        if !self.lifecycle.accepts_input() {
            return;
        }
        self.send_user_input(encode_paste(text, self.mode()));
    }

    pub fn resize(&mut self, size: TerminalSize, cx: &mut gpui_kit::Context<Self>) {
        if size == self.size {
            return;
        }
        self.size = size;
        *self
            .runtime
            .size
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = size;
        self.runtime.term.lock().resize(size);
        self.runtime.resize(size);
        cx.emit(TerminalEngineEvent::Changed);
        cx.notify();
    }

    pub fn scroll(&mut self, lines: i32, cx: &mut gpui_kit::Context<Self>) {
        self.runtime
            .term
            .lock()
            .scroll_display(Scroll::Delta(lines));
        cx.notify();
    }

    pub fn start_selection(
        &mut self,
        point: Point,
        side: Side,
        selection_type: SelectionType,
        cx: &mut gpui_kit::Context<Self>,
    ) {
        self.runtime.term.lock().selection = Some(Selection::new(selection_type, point, side));
        cx.notify();
    }

    pub fn update_selection(&mut self, point: Point, side: Side, cx: &mut gpui_kit::Context<Self>) {
        if let Some(selection) = self.runtime.term.lock().selection.as_mut() {
            selection.update(point, side);
        }
        cx.notify();
    }

    pub fn clear_selection(&mut self, cx: &mut gpui_kit::Context<Self>) {
        self.runtime.term.lock().selection = None;
        cx.notify();
    }

    pub fn selection_text(&self) -> Option<String> {
        self.runtime.term.lock().selection_to_string()
    }

    pub fn has_selection(&self) -> bool {
        self.selection_text().is_some_and(|text| !text.is_empty())
    }

    pub fn mode(&self) -> TermMode {
        *self.runtime.term.lock().mode()
    }

    pub fn set_focused(&self, focused: bool) {
        let mut term = self.runtime.term.lock();
        if term.is_focused == focused {
            return;
        }
        term.is_focused = focused;
        if term.mode().contains(TermMode::FOCUS_IN_OUT) {
            drop(term);
            self.runtime
                .write(if focused { b"\x1b[I" } else { b"\x1b[O" }.to_vec());
        }
    }

    pub fn lifecycle(&self) -> &TerminalLifecycle {
        &self.lifecycle
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn status(&self) -> TerminalStatus {
        let term = self.runtime.term.lock();
        let cursor = term.grid().cursor.point;
        TerminalStatus::new(
            self.lifecycle.clone(),
            cursor.line.0.max(0) as usize,
            cursor.column.0,
            self.title.clone(),
        )
    }

    pub fn snapshot(&self) -> TerminalSnapshot {
        let term = self.runtime.term.lock();
        let content = term.renderable_content();
        let selection = content.selection;
        let display_offset = content.display_offset as i32;
        let cursor_row = content.cursor.point.line.0 + display_offset;
        let cursor_point = content.cursor.point;
        let cursor_shape = content.cursor.shape;
        let columns = term.grid().columns();
        let mut cells: Vec<_> = content
            .display_iter
            .map(|indexed| {
                let mut character = indexed.cell.c.to_string();
                if let Some(zerowidth) = indexed.cell.zerowidth() {
                    character.extend(zerowidth);
                }
                TerminalCell {
                    character,
                    foreground: indexed.cell.fg,
                    background: indexed.cell.bg,
                    flags: indexed.cell.flags,
                    selected: selection.is_some_and(|range| {
                        range.contains_cell(&indexed, cursor_point, cursor_shape)
                    }),
                }
            })
            .collect();
        normalize_wide_character_selection(&mut cells, columns);
        TerminalSnapshot {
            columns,
            rows: term.grid().screen_lines(),
            cells,
            cursor_row: (0..term.grid().screen_lines() as i32)
                .contains(&cursor_row)
                .then_some(cursor_row as usize),
            cursor_column: content.cursor.point.column.0,
            cursor_shape: content.cursor.shape,
            cursor_blinking: term.cursor_style().blinking,
            display_offset: content.display_offset,
            colors: std::array::from_fn(|index| content.colors[index]),
            mode: content.mode,
        }
    }
}

/// Keep the leading cell and spacer of a full-width glyph visually atomic.
/// Alacritty stores CJK glyphs in one `WIDE_CHAR` cell followed by a
/// `WIDE_CHAR_SPACER`; painting only one half produces a one-column highlight
/// that cannot line up with the two-column glyph.
fn normalize_wide_character_selection(cells: &mut [TerminalCell], columns: usize) {
    if columns == 0 {
        return;
    }

    for row in cells.chunks_mut(columns) {
        for column in 0..row.len().saturating_sub(1) {
            if row[column].flags.contains(Flags::WIDE_CHAR)
                && row[column + 1].flags.contains(Flags::WIDE_CHAR_SPACER)
            {
                let selected = row[column].selected || row[column + 1].selected;
                row[column].selected = selected;
                row[column + 1].selected = selected;
            }
        }
    }
}

impl gpui_kit::EventEmitter<TerminalEngineEvent> for TerminalEngine {}

impl Drop for TerminalEngine {
    fn drop(&mut self) {
        self.runtime.shutdown();
    }
}

#[derive(Clone, Copy, Debug)]
pub enum TerminalEngineEvent {
    Changed,
}

struct TerminalRuntime {
    term: Arc<FairMutex<AlacrittyTerm>>,
    commands: mpsc::Sender<TerminalTransportCommand>,
    size: Arc<Mutex<TerminalSize>>,
    wakeup_pending: Arc<AtomicBool>,
}

impl TerminalRuntime {
    fn start(
        generation: u64,
        size: TerminalSize,
        transport: Box<dyn super::TerminalTransport>,
        ui_events: mpsc::Sender<TerminalUiEvent>,
    ) -> Self {
        let (commands, command_receiver) = mpsc::channel();
        let (transport_events, transport_receiver) = async_channel::unbounded();
        let shared_size = Arc::new(Mutex::new(size));
        let wakeup_pending = Arc::new(AtomicBool::new(false));
        let proxy = TerminalEventProxy {
            generation,
            commands: commands.clone(),
            ui_events: ui_events.clone(),
            size: shared_size.clone(),
            wakeup_pending: wakeup_pending.clone(),
        };
        let parser_proxy = proxy.clone();
        let config = terminal_config();
        let term = Arc::new(FairMutex::new(AlacrittyTerm::new(config, &size, proxy)));

        let parser_term = term.clone();
        let parser_ui_events = ui_events.clone();
        thread::Builder::new()
            .name("shellr-terminal-parser".into())
            .spawn(move || {
                let mut processor = Processor::new();
                while let Ok(event) = transport_receiver.recv_blocking() {
                    match event {
                        TerminalTransportEvent::Started => {
                            send_ui(&parser_ui_events, generation, TerminalUiEventKind::Started);
                        }
                        TerminalTransportEvent::Output(bytes) => {
                            processor.advance(&mut *parser_term.lock(), &bytes);
                            parser_proxy.wakeup();
                        }
                        TerminalTransportEvent::Exited { code, signal } => {
                            let description = signal
                                .as_deref()
                                .map(|signal| format!("进程已退出：{signal}，退出码 {code}"))
                                .unwrap_or_else(|| format!("进程已退出，退出码 {code}"));
                            append_message_with_processor(
                                &parser_term,
                                &mut processor,
                                &description,
                            );
                            send_ui(
                                &parser_ui_events,
                                generation,
                                TerminalUiEventKind::Exited { code, signal },
                            );
                            parser_proxy.wakeup();
                            break;
                        }
                        TerminalTransportEvent::Failed(error) => {
                            append_message_with_processor(
                                &parser_term,
                                &mut processor,
                                &format!("终端错误：{error}"),
                            );
                            send_ui(
                                &parser_ui_events,
                                generation,
                                TerminalUiEventKind::Failed(error),
                            );
                            parser_proxy.wakeup();
                        }
                    }
                }
            })
            .expect("terminal parser thread");

        thread::Builder::new()
            .name("shellr-terminal-transport".into())
            .spawn(move || {
                if let Err(error) = transport.run(size, command_receiver, transport_events.clone())
                {
                    let _ = transport_events
                        .send_blocking(TerminalTransportEvent::Failed(error.to_string()));
                }
            })
            .expect("terminal transport thread");

        Self {
            term,
            commands,
            size: shared_size,
            wakeup_pending,
        }
    }

    fn write(&self, bytes: Vec<u8>) {
        let _ = self.commands.send(TerminalTransportCommand::Write(bytes));
    }

    fn resize(&self, size: TerminalSize) {
        let _ = self.commands.send(TerminalTransportCommand::Resize(size));
    }

    fn shutdown(&self) {
        let _ = self.commands.send(TerminalTransportCommand::Shutdown);
    }
}

fn terminal_config() -> Config {
    Config {
        scrolling_history: 10_000,
        default_cursor_style: CursorStyle {
            shape: CursorShape::Block,
            blinking: true,
        },
        osc52: alacritty_terminal::term::Osc52::Disabled,
        kitty_keyboard: false,
        ..Config::default()
    }
}

impl Drop for TerminalRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Clone)]
pub struct TerminalEventProxy {
    generation: u64,
    commands: mpsc::Sender<TerminalTransportCommand>,
    ui_events: mpsc::Sender<TerminalUiEvent>,
    size: Arc<Mutex<TerminalSize>>,
    wakeup_pending: Arc<AtomicBool>,
}

impl EventListener for TerminalEventProxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::Title(title) => {
                send_ui(
                    &self.ui_events,
                    self.generation,
                    TerminalUiEventKind::Title(title),
                );
            }
            Event::ResetTitle => {
                send_ui(
                    &self.ui_events,
                    self.generation,
                    TerminalUiEventKind::ResetTitle,
                );
            }
            Event::ColorRequest(index, formatter) => {
                send_ui(
                    &self.ui_events,
                    self.generation,
                    TerminalUiEventKind::ColorRequest(index, formatter),
                );
            }
            Event::PtyWrite(text) => {
                let _ = self
                    .commands
                    .send(TerminalTransportCommand::Write(text.into_bytes()));
            }
            Event::TextAreaSizeRequest(formatter) => {
                let size = *self.size.lock().unwrap_or_else(|error| error.into_inner());
                let response = formatter(WindowSize {
                    num_lines: size.rows().min(u16::MAX as usize) as u16,
                    num_cols: size.columns().min(u16::MAX as usize) as u16,
                    cell_width: size.cell_width(),
                    cell_height: size.cell_height(),
                });
                let _ = self
                    .commands
                    .send(TerminalTransportCommand::Write(response.into_bytes()));
            }
            Event::Wakeup | Event::MouseCursorDirty | Event::CursorBlinkingChange | Event::Bell => {
                self.wakeup()
            }
            Event::ClipboardStore(_, _) | Event::ClipboardLoad(_, _) => {}
            Event::Exit | Event::ChildExit(_) => self.wakeup(),
        }
    }
}

impl TerminalEventProxy {
    fn wakeup(&self) {
        if !self.wakeup_pending.swap(true, Ordering::AcqRel) {
            send_ui(
                &self.ui_events,
                self.generation,
                TerminalUiEventKind::Wakeup,
            );
        }
    }
}

struct TerminalUiEvent {
    generation: u64,
    kind: TerminalUiEventKind,
}

enum TerminalUiEventKind {
    Started,
    Wakeup,
    Title(String),
    ResetTitle,
    Exited { code: u32, signal: Option<String> },
    Failed(String),
    ColorRequest(usize, Arc<dyn Fn(Rgb) -> String + Send + Sync>),
}

fn send_ui(events: &mpsc::Sender<TerminalUiEvent>, generation: u64, kind: TerminalUiEventKind) {
    let _ = events.send(TerminalUiEvent { generation, kind });
}

fn append_message(term: &Arc<FairMutex<AlacrittyTerm>>, message: &str) {
    let mut processor = Processor::new();
    append_message_with_processor(term, &mut processor, message);
}

fn append_message_with_processor(
    term: &Arc<FairMutex<AlacrittyTerm>>,
    processor: &mut Processor,
    message: &str,
) {
    let line = format!("\r\n\x1b[2m[{message}]\x1b[0m\r\n");
    processor.advance(&mut *term.lock(), line.as_bytes());
}

fn sanitize_title(title: &str) -> Option<String> {
    let title: String = title
        .chars()
        .filter(|character| !character.is_control())
        .take(80)
        .collect();
    (!title.trim().is_empty()).then_some(title)
}

fn default_query_color(index: usize) -> Rgb {
    if index == alacritty_terminal::vte::ansi::NamedColor::Background as usize {
        Rgb {
            r: 30,
            g: 30,
            b: 30,
        }
    } else {
        Rgb {
            r: 224,
            g: 224,
            b: 224,
        }
    }
}

fn encode_paste(text: &str, mode: TermMode) -> Vec<u8> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if mode.contains(TermMode::BRACKETED_PASTE) {
        format!("\x1b[200~{}\x1b[201~", normalized.replace('\x1b', "")).into_bytes()
    } else {
        normalized.into_bytes()
    }
}

fn prepare_term_for_user_input(term: &mut AlacrittyTerm) {
    term.scroll_display(Scroll::Bottom);
    term.selection = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::index::{Column, Line};
    use alacritty_terminal::vte::ansi::NamedColor;

    fn test_term(columns: usize, rows: usize) -> AlacrittyTerm {
        let size = TerminalSize::new(columns, rows, 8, 16);
        let (commands, _command_receiver) = mpsc::channel();
        let (ui_events, _ui_receiver) = mpsc::channel();
        let config = terminal_config();
        AlacrittyTerm::new(
            config,
            &size,
            TerminalEventProxy {
                generation: 1,
                commands,
                ui_events,
                size: Arc::new(Mutex::new(size)),
                wakeup_pending: Arc::new(AtomicBool::new(false)),
            },
        )
    }

    fn feed(term: &mut AlacrittyTerm, bytes: &[u8]) {
        Processor::<alacritty_terminal::vte::ansi::StdSyncHandler>::new().advance(term, bytes);
    }

    #[test]
    fn sanitizes_terminal_titles() {
        assert_eq!(sanitize_title("hello\nworld"), Some("helloworld".into()));
        assert_eq!(sanitize_title("\n\t"), None);
    }

    #[test]
    fn terminal_size_implements_dimensions() {
        let size = TerminalSize::new(120, 40, 9, 18);
        assert_eq!(size.columns(), 120);
        assert_eq!(size.screen_lines(), 40);
    }

    #[test]
    fn default_cursor_is_a_blinking_block() {
        let term = test_term(8, 2);
        assert_eq!(term.cursor_style().shape, CursorShape::Block);
        assert!(term.cursor_style().blinking);
    }

    #[test]
    fn parses_ansi_color_wide_characters_and_alternate_screen() {
        let mut term = test_term(12, 3);
        feed(&mut term, b"\x1b[31mA\x1b[0m");
        assert_eq!(
            term.grid()[Line(0)][Column(0)].fg,
            Color::Named(NamedColor::Red)
        );

        feed(&mut term, "你".as_bytes());
        assert!(
            term.grid()[Line(0)][Column(1)]
                .flags
                .contains(Flags::WIDE_CHAR)
        );
        assert!(
            term.grid()[Line(0)][Column(2)]
                .flags
                .contains(Flags::WIDE_CHAR_SPACER)
        );

        feed(&mut term, b"\x1b[?1049hB");
        assert!(term.mode().contains(TermMode::ALT_SCREEN));
        assert!((0..12).any(|column| term.grid()[Line(0)][Column(column)].c == 'B'));
        feed(&mut term, b"\x1b[?1049l");
        assert!(!term.mode().contains(TermMode::ALT_SCREEN));
        assert_eq!(term.grid()[Line(0)][Column(0)].c, 'A');
    }

    #[test]
    fn caps_scrollback_at_ten_thousand_lines() {
        let mut term = test_term(4, 2);
        feed(&mut term, "x\r\n".repeat(10_050).as_bytes());
        assert_eq!(
            term.grid().total_lines() - term.grid().screen_lines(),
            10_000
        );
    }

    #[test]
    fn returns_selected_text() {
        let mut term = test_term(8, 2);
        feed(&mut term, b"hello");
        term.selection = Some(Selection::new(
            SelectionType::Simple,
            Point::new(Line(0), Column(1)),
            Side::Left,
        ));
        term.selection
            .as_mut()
            .unwrap()
            .update(Point::new(Line(0), Column(3)), Side::Right);
        assert_eq!(term.selection_to_string().as_deref(), Some("ell"));
    }

    #[test]
    fn supports_reverse_word_line_and_empty_selections() {
        let mut term = test_term(20, 2);
        feed(&mut term, b"alpha.txt beta");

        term.selection = Some(Selection::new(
            SelectionType::Simple,
            Point::new(Line(0), Column(4)),
            Side::Right,
        ));
        term.selection
            .as_mut()
            .unwrap()
            .update(Point::new(Line(0), Column(1)), Side::Left);
        assert_eq!(term.selection_to_string().as_deref(), Some("lpha"));

        term.selection = Some(Selection::new(
            SelectionType::Semantic,
            Point::new(Line(0), Column(2)),
            Side::Left,
        ));
        assert_eq!(term.selection_to_string().as_deref(), Some("alpha.txt"));

        term.selection = Some(Selection::new(
            SelectionType::Lines,
            Point::new(Line(0), Column(7)),
            Side::Left,
        ));
        assert_eq!(
            term.selection_to_string().as_deref(),
            Some("alpha.txt beta\n")
        );

        term.selection = Some(Selection::new(
            SelectionType::Simple,
            Point::new(Line(0), Column(0)),
            Side::Left,
        ));
        assert_eq!(term.selection_to_string(), None);
    }

    #[test]
    fn selected_text_preserves_cjk_and_joins_soft_wrapped_lines() {
        let mut term = test_term(4, 2);
        feed(&mut term, "ab你cd".as_bytes());
        term.selection = Some(Selection::new(
            SelectionType::Simple,
            Point::new(Line(0), Column(1)),
            Side::Left,
        ));
        term.selection
            .as_mut()
            .unwrap()
            .update(Point::new(Line(1), Column(1)), Side::Right);
        assert_eq!(term.selection_to_string().as_deref(), Some("b你cd"));
    }

    #[test]
    fn wide_character_selection_highlights_both_grid_cells() {
        let cell = |flags, selected| TerminalCell {
            character: " ".into(),
            foreground: Color::Named(NamedColor::Foreground),
            background: Color::Named(NamedColor::Background),
            flags,
            selected,
        };
        let mut cells = vec![
            cell(Flags::WIDE_CHAR, true),
            cell(Flags::WIDE_CHAR_SPACER, false),
            cell(Flags::empty(), false),
            cell(Flags::WIDE_CHAR, false),
            cell(Flags::WIDE_CHAR_SPACER, true),
            cell(Flags::empty(), false),
        ];

        normalize_wide_character_selection(&mut cells, 3);

        assert!(cells[0].selected);
        assert!(cells[1].selected);
        assert!(!cells[2].selected);
        assert!(cells[3].selected);
        assert!(cells[4].selected);
        assert!(!cells[5].selected);
    }

    #[test]
    fn user_input_returns_to_bottom_and_clears_selection() {
        let mut term = test_term(8, 2);
        feed(&mut term, "one\r\ntwo\r\nthree\r\n".as_bytes());
        term.scroll_display(Scroll::Delta(2));
        term.selection = Some(Selection::new(
            SelectionType::Semantic,
            Point::new(Line(-1), Column(1)),
            Side::Left,
        ));
        assert!(term.grid().display_offset() > 0);
        assert!(term.selection.is_some());

        prepare_term_for_user_input(&mut term);

        assert_eq!(term.grid().display_offset(), 0);
        assert!(term.selection.is_none());
    }

    #[test]
    fn bracketed_paste_filters_escape_and_normalizes_newlines() {
        assert_eq!(
            encode_paste("a\r\nb\x1bc", TermMode::BRACKETED_PASTE),
            b"\x1b[200~a\nbc\x1b[201~"
        );
        assert_eq!(encode_paste("a\rb", TermMode::default()), b"a\nb");
    }
}
