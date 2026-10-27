use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    dock::{BasePanel, Panel, PanelEvent, TabGroup},
    group_box::GroupBoxVariant,
    h_flex,
    label::Label,
    menu::PopupMenu,
    separator::Separator,
    setting::{
        NumberFieldOptions, SelectIndex, SettingField, SettingGroup, SettingItem, SettingPage,
        Settings,
    },
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{
    CatalogIcon, CenterTab, CheckForUpdates, CloseSettings, CopyAgentSkill, DownloadUpdate,
    InstallAgentSkill, InstallCliCommand, OpenDownloadPage, OpenWebsite, RefreshCliIntegration,
    RemoveAgentSkill, RemoveCliCommand, ShowUpdate,
};
use crate::cli::{AgentKind, BinaryStatus, CliIntegration, IntegrationStatus, SkillStatus};
use crate::explorer::{FileIcon, FileIconKind};
use crate::i18n::t;
use crate::sftp::{DEFAULT_PARALLEL_TRANSFERS, PARALLEL_TRANSFERS_RANGE};
use crate::shared::{ClosableTabTitle, close_tab_items};
use crate::sync::SyncService;
use crate::terminal::{
    CHARACTER_DELAY_RANGE, DEFAULT_LOG_TEMPLATE, DEFAULT_LOG_TIMESTAMP_FORMAT, FONT_SIZE_RANGE,
    LINE_DELAY_RANGE, LINE_HEIGHT_RANGE, SendDelay, TerminalColors, TerminalFont,
    TerminalFontPreview, TerminalHighlights, is_font_installed, monospace_font_families,
};
use crate::update::{Phase, Tone, UpdateSnapshot, UpdateStep, Updater, build_info, platform};

use super::highlight_rules::HighlightRulesEditor;
use super::local_shell_editor::{LocalShellEditor, local_shell_field};
use super::session_log_editor::{
    SessionLogEditor, directory_field, template_field, timestamp_format_field,
};
use super::shortcuts_editor::{ShortcutsEditor, shortcut_groups};
use super::sidebar_tools::sidebar_tools_group;
use super::terminal_themes::{app_follows_item, terminal_theme_item};
use super::{
    AppSettings, Choice, INTERFACE_FONT_SIZE_RANGE, NotificationSettings, SettingsStore,
    WindowSettings,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsPanelEvent {
    Activated,
    Closed,
}

/// The 设置 tab in the center. There is at most one: opening settings again
/// brings this tab forward.
///
/// Two columns, from gpui-kit's `Settings`: the categories with a search
/// field on the left, the selected category's settings on the right. The
/// fields read and write the `SettingsStore`; the workspace applies what
/// changes.
pub struct SettingsPanel {
    store: Entity<SettingsStore>,
    /// What of the external CLI is installed, for 外部 CLI.
    integration: Entity<CliIntegration>,
    /// Where updating stands, for 关于.
    updater: Entity<Updater>,
    /// Where sync stands, for 同步.
    sync: Entity<SyncService>,
    /// The rule table of 关键字高亮, whose fields keep their state across
    /// the page's renders.
    highlight_rules: Entity<HighlightRulesEditor>,
    /// 键盘快捷键, which knows which shortcut is taking new keys.
    shortcuts: Entity<ShortcutsEditor>,
    /// 终端 › 本地终端's field, with the shells installed here.
    local_shell: Entity<LocalShellEditor>,
    /// 终端 › 会话日志's text fields.
    session_log: Entity<SessionLogEditor>,
    /// The monospace families to choose the terminal font from; empty until
    /// the scan in the background comes back.
    font_families: &'static [SharedString],
    /// The category last shown, for coming back to it. `Settings` keeps its
    /// choice in element state, which goes while the tab is hidden; its
    /// page header records the category it draws.
    shown_category: Rc<Cell<usize>>,
    focus_handle: FocusHandle,
    tab_group: Option<WeakEntity<TabGroup>>,
    _subscriptions: [Subscription; 7],
    _font_scan: Task<()>,
}

impl SettingsPanel {
    pub fn new(
        store: Entity<SettingsStore>,
        integration: Entity<CliIntegration>,
        updater: Entity<Updater>,
        sync: Entity<SyncService>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let highlight_rules = cx.new(|cx| HighlightRulesEditor::new(store.clone(), window, cx));
        let shortcuts = cx.new(|cx| ShortcutsEditor::new(store.clone(), window, cx));
        let local_shell = cx.new(|cx| LocalShellEditor::new(store.clone(), window, cx));
        let session_log = cx.new(|cx| SessionLogEditor::new(store.clone(), window, cx));
        let subscriptions = [
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.observe(&shortcuts, |_, _, cx| cx.notify()),
            cx.observe(&local_shell, |_, _, cx| cx.notify()),
            cx.observe(&integration, |_, _, cx| cx.notify()),
            cx.observe(&updater, |_, _, cx| cx.notify()),
            cx.observe(&session_log, |_, _, cx| cx.notify()),
            cx.observe(&sync, |_, _, cx| cx.notify()),
        ];
        // Loading every family takes a moment the first time; the process
        // keeps the answer, so later tabs have it at once.
        let text_system = cx.text_system().clone();
        let font_scan = cx.spawn(async move |this, cx| {
            let families = cx
                .background_spawn(async move { monospace_font_families(&text_system) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.font_families = families;
                cx.notify();
            });
        });
        Self {
            store,
            integration,
            updater,
            sync,
            highlight_rules,
            shortcuts,
            local_shell,
            session_log,
            font_families: &[],
            shown_category: Rc::default(),
            focus_handle: cx.focus_handle(),
            tab_group: None,
            _subscriptions: subscriptions,
            _font_scan: font_scan,
        }
    }

    pub fn tab_group(&self) -> Option<WeakEntity<TabGroup>> {
        self.tab_group.clone()
    }

    /// Take the focus, and tell the workspace this is the tab ⌘W closes.
    /// Also called when the tab is opened again while already displayed,
    /// which the tab group does not report.
    pub fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        cx.emit(SettingsPanelEvent::Activated);
    }
}

impl EventEmitter<PanelEvent> for SettingsPanel {}
impl EventEmitter<SettingsPanelEvent> for SettingsPanel {}

impl Focusable for SettingsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl BasePanel for SettingsPanel {
    fn panel_name(&self) -> &'static str {
        "SettingsPanel"
    }

    /// Closing goes through `CloseSettings`; see `ClosableTabTitle`.
    fn closable(&self, _: &App) -> bool {
        false
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if active {
            self.show(window, cx);
        }
    }

    fn on_added_to(&mut self, group: WeakEntity<TabGroup>, _: &mut Window, _: &mut Context<Self>) {
        self.tab_group = Some(group);
    }

    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.tab_group = None;
        cx.emit(SettingsPanelEvent::Closed);
    }
}

impl Panel for SettingsPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (group, panel) = (self.tab_group.clone(), cx.entity_id());
        ClosableTabTitle::new(
            "settings-tab",
            Icon::new(CatalogIcon::Settings).small(),
            t!("settings.tab.title"),
        )
        .closable("close-settings", Box::new(CloseSettings))
        .context_menu(move |menu, _, cx| tab_menu(menu, group.clone(), panel, cx))
    }

    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        tab_menu(menu, self.tab_group.clone(), cx.entity_id(), cx)
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Render for SettingsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("settings")
            .test_support()
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                Settings::new("settings-categories")
                    .small()
                    // Split geometry is an API boundary that takes `Pixels`.
                    .sidebar_width(px(200.))
                    .with_group_variant(GroupBoxVariant::Outline)
                    // Used only when the tab comes back and `Settings`
                    // starts over.
                    .default_selected_index(SelectIndex {
                        page_ix: self.shown_category.get(),
                        group_ix: None,
                    })
                    .pages(
                        CATEGORIES
                            .iter()
                            .enumerate()
                            .map(|(ix, category)| category.page(ix, self, cx)),
                    ),
            )
    }
}

/// One category of the left column.
struct Category {
    /// The key of the title.
    title: &'static str,
    icon: CatalogIcon,
    /// The category's settings. `Settings` leaves out a page without groups,
    /// left column entry and all, so a category needs at least one.
    groups: CategoryGroups,
}

/// Builds a category's setting groups over the store they edit.
type CategoryGroups = fn(&SettingsPanel, &App) -> Vec<SettingGroup>;

/// The categories, in the order the left column lists them.
const CATEGORIES: [Category; 9] = [
    Category {
        title: "settings.category.appearance",
        icon: CatalogIcon::Palette,
        groups: appearance_groups,
    },
    Category {
        title: "settings.category.terminal",
        icon: CatalogIcon::Terminal,
        groups: terminal_groups,
    },
    Category {
        title: "settings.category.sftp",
        icon: CatalogIcon::FolderTree,
        groups: sftp_groups,
    },
    Category {
        title: "settings.category.highlight",
        icon: CatalogIcon::Highlighter,
        groups: highlight_groups,
    },
    Category {
        title: "settings.category.shortcuts",
        icon: CatalogIcon::Keyboard,
        groups: shortcuts_groups,
    },
    Category {
        title: "settings.category.external_cli",
        icon: CatalogIcon::SquareTerminal,
        groups: external_cli_groups,
    },
    Category {
        title: "settings.category.sync",
        icon: CatalogIcon::CloudSync,
        groups: sync_groups,
    },
    Category {
        title: "settings.category.application",
        icon: CatalogIcon::AppWindow,
        groups: application_groups,
    },
    Category {
        title: "settings.category.about",
        icon: CatalogIcon::Info,
        groups: about_groups,
    },
];

impl Category {
    /// The page of the category at `ix` in `CATEGORIES`.
    fn page(&self, ix: usize, panel: &SettingsPanel, cx: &App) -> SettingPage {
        let shown = panel.shown_category.clone();
        SettingPage::new(t!(self.title))
            .icon(Icon::new(self.icon))
            // Drawn only for the page on display: nothing to see, it notes
            // which one that is.
            .title_suffix(move |_, _| {
                shown.set(ix);
                Empty
            })
            .groups((self.groups)(panel, cx))
    }
}

/// 外观.
fn appearance_groups(panel: &SettingsPanel, _: &App) -> Vec<SettingGroup> {
    let store = &panel.store;
    vec![
        SettingGroup::new()
            .title(t!("settings.appearance.interface"))
            .items([
                SettingItem::new(
                    t!("settings.appearance.language"),
                    choice_field(
                        store,
                        |settings| settings.language,
                        |settings, language| settings.language = language,
                    ),
                ),
                SettingItem::new(
                    t!("settings.appearance.mode"),
                    choice_field(
                        store,
                        |settings| settings.appearance,
                        |settings, appearance| settings.appearance = appearance,
                    ),
                ),
                SettingItem::new(
                    t!("settings.appearance.font_size"),
                    font_size_field(
                        store,
                        INTERFACE_FONT_SIZE_RANGE,
                        |settings| settings.interface.font_size,
                        |settings, size| settings.interface.font_size = size,
                    ),
                )
                .description(t!("settings.appearance.font_size_description"))
                .keywords(keywords(t!("settings.appearance.font_size_keywords"))),
            ]),
        SettingGroup::new()
            .title(t!("settings.appearance.theme"))
            .description(t!("settings.appearance.theme_description"))
            .items([app_follows_item(store), terminal_theme_item(store)]),
    ]
}

/// What the preview shows: letters, digits, the characters that are easy to
/// confuse, symbols, Chinese (two cells a character), line drawing, and a
/// command line.
const PREVIEW_LINES: &[&str] = &[
    "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
    "abcdefghijklmnopqrstuvwxyz",
    "0123456789  0O 1lI| ,.;:'\"`",
    "~!@#$%^&*()-_=+[]{}<>/\\?",
    "中文预览：你好，世界！", // i18n: keep (shows how two-cell characters look)
    "┌──┬──┐ ░▒▓█ ←↑→↓ ✓",
    "root@web-01:~$ tail -f /var/log/syslog",
];

/// 终端.
fn terminal_groups(panel: &SettingsPanel, cx: &App) -> Vec<SettingGroup> {
    let store = &panel.store;
    let defaults = AppSettings::default().terminal_font;
    vec![
        SettingGroup::new()
            .title(t!("settings.terminal.font"))
            .items([
                SettingItem::new(t!("settings.terminal.family"), font_family_field(panel, cx))
                    .description(t!("settings.terminal.family_description")),
                SettingItem::new(
                    t!("settings.terminal.size"),
                    font_size_field(
                        store,
                        FONT_SIZE_RANGE,
                        |settings| settings.terminal_font.size,
                        |settings, size| settings.terminal_font.size = size,
                    ),
                )
                .description(t!("settings.terminal.size_description")),
                SettingItem::new(
                    t!("settings.terminal.line_height"),
                    number_field(
                        store,
                        NumberFieldOptions {
                            min: *LINE_HEIGHT_RANGE.start() as f64,
                            max: *LINE_HEIGHT_RANGE.end() as f64,
                            step: 0.1,
                        },
                        |settings| settings.terminal_font.line_height,
                        |settings, line_height| settings.terminal_font.line_height = line_height,
                    )
                    .default_value(defaults.line_height as f64),
                )
                .description(t!("settings.terminal.line_height_description")),
                SettingItem::render(|_, _, cx| {
                    v_flex()
                        .w_full()
                        .gap_2()
                        .child(div().text_sm().child(t!("settings.preview")))
                        .child(
                            div()
                                .w_full()
                                .p_3()
                                .rounded(cx.theme().radius)
                                .border_1()
                                .border_color(cx.theme().border)
                                .bg(TerminalColors::current(cx).background())
                                .overflow_hidden()
                                .child(TerminalFontPreview::new(
                                    "terminal-font-preview",
                                    PREVIEW_LINES,
                                )),
                        )
                })
                .keywords(keywords(t!("settings.terminal.preview_keywords"))),
            ]),
        interaction_group(store),
        sidebar_tools_group(store),
        notification_group(store),
        local_terminal_group(panel),
        send_delay_group(store),
        session_log_group(panel),
    ]
}

/// 终端 → 本地终端: the program a local terminal runs, and whether 项目's
/// terminals keep their output. The shell's row stacks, so a path with its
/// arguments has the page's width.
fn local_terminal_group(panel: &SettingsPanel) -> SettingGroup {
    let (editor, reset) = (panel.local_shell.clone(), panel.local_shell.clone());
    let store = panel.store.clone();
    let (reader, writer) = (panel.store.clone(), panel.store.clone());
    SettingGroup::new()
        .title(t!("settings.local_terminal.title"))
        .items([
            SettingItem::new(
                t!("settings.local_terminal.shell"),
                SettingField::render(move |options, _, cx| local_shell_field(&editor, options, cx))
                    .on_reset(
                        move |cx| store.read(cx).settings().local_terminal.shell.is_some(),
                        move |window, cx| {
                            reset.update(cx, |editor, cx| editor.choose(None, window, cx));
                        },
                    ),
            )
            .description(t!("settings.local_terminal.shell_description"))
            .layout(Axis::Vertical)
            .keywords(keywords(t!("settings.local_terminal.keywords"))),
            SettingItem::new(
                t!("settings.local_terminal.keep_output"),
                SettingField::switch(
                    move |cx| reader.read(cx).settings().local_terminal.keep_output,
                    move |on, cx| {
                        writer.update(cx, |store, cx| {
                            store.update(|settings| settings.local_terminal.keep_output = on, cx)
                        });
                    },
                )
                .default_value(true),
            )
            .description(t!("settings.local_terminal.keep_output_description"))
            .keywords(keywords(t!("settings.local_terminal.keep_output_keywords"))),
        ])
}

/// 终端 → 发送延迟: how long a paste or a script waits between its lines and
/// its characters.
fn send_delay_group(store: &Entity<SettingsStore>) -> SettingGroup {
    let defaults = SendDelay::default();
    SettingGroup::new()
        .title(t!("settings.send_delay.title"))
        .description(t!("settings.send_delay.description"))
        .items([
            SettingItem::new(
                t!("settings.send_delay.line"),
                number_field(
                    store,
                    NumberFieldOptions {
                        min: *LINE_DELAY_RANGE.start() as f64,
                        max: *LINE_DELAY_RANGE.end() as f64,
                        step: 10.0,
                    },
                    |settings| settings.send_delay.line_ms as f32,
                    |settings, line| settings.send_delay.line_ms = line.round() as u32,
                )
                .default_value(defaults.line_ms as f64),
            )
            .description(t!("settings.send_delay.line_description"))
            .keywords(keywords(t!("settings.send_delay.line_keywords"))),
            SettingItem::new(
                t!("settings.send_delay.character"),
                number_field(
                    store,
                    NumberFieldOptions {
                        min: *CHARACTER_DELAY_RANGE.start() as f64,
                        max: *CHARACTER_DELAY_RANGE.end() as f64,
                        step: 5.0,
                    },
                    |settings| settings.send_delay.character_ms as f32,
                    |settings, character| {
                        settings.send_delay.character_ms = character.round() as u32
                    },
                )
                .default_value(defaults.character_ms as f64),
            )
            .description(t!("settings.send_delay.character_description"))
            .keywords(keywords(t!("settings.send_delay.character_keywords"))),
        ])
}

/// 终端 → 会话日志: the switch, the folder and the name, and the time each
/// line starts with.
fn session_log_group(panel: &SettingsPanel) -> SettingGroup {
    let store = panel.store.clone();
    let (directory_store, template_store, format_store) =
        (store.clone(), store.clone(), store.clone());
    let (directory_editor, directory_reset) =
        (panel.session_log.clone(), panel.session_log.clone());
    let (template_editor, template_reset) = (panel.session_log.clone(), panel.session_log.clone());
    let (format_editor, format_reset) = (panel.session_log.clone(), panel.session_log.clone());
    let (reader, writer) = (store.clone(), store.clone());
    let (timestamp_reader, timestamp_writer) = (store.clone(), store.clone());
    SettingGroup::new()
        .title(t!("settings.session_log.title"))
        .description(t!("settings.session_log.description"))
        .items([
            SettingItem::new(
                t!("settings.session_log.enabled"),
                SettingField::switch(
                    move |cx| reader.read(cx).settings().session_log.enabled,
                    move |on, cx| {
                        writer.update(cx, |store, cx| {
                            store.update(|settings| settings.session_log.enabled = on, cx)
                        });
                    },
                )
                .default_value(false),
            )
            .description(t!("settings.session_log.enabled_description"))
            .keywords(keywords(t!("settings.session_log.enabled_keywords"))),
            SettingItem::new(
                t!("settings.session_log.directory"),
                SettingField::render(move |options, _, cx| {
                    directory_field(&directory_editor, options, cx)
                })
                .on_reset(
                    move |cx| {
                        directory_store
                            .read(cx)
                            .settings()
                            .session_log
                            .directory
                            .is_some()
                    },
                    move |window, cx| {
                        directory_reset
                            .update(cx, |editor, cx| editor.set_directory(None, window, cx));
                    },
                ),
            )
            .description(t!("settings.session_log.directory_description"))
            .layout(Axis::Vertical)
            .keywords(keywords(t!("settings.session_log.directory_keywords"))),
            SettingItem::new(
                t!("settings.session_log.template"),
                SettingField::render(move |options, _, cx| {
                    template_field(&template_editor, options, cx)
                })
                .on_reset(
                    move |cx| {
                        template_store.read(cx).settings().session_log.template
                            != DEFAULT_LOG_TEMPLATE
                    },
                    move |window, cx| {
                        template_reset.update(cx, |editor, cx| {
                            editor.set_template(DEFAULT_LOG_TEMPLATE, window, cx)
                        });
                    },
                ),
            )
            .description(t!("settings.session_log.template_description"))
            .layout(Axis::Vertical)
            .keywords(keywords(t!("settings.session_log.template_keywords"))),
            SettingItem::new(
                t!("settings.session_log.timestamp"),
                SettingField::switch(
                    move |cx| timestamp_reader.read(cx).settings().session_log.timestamp,
                    move |on, cx| {
                        timestamp_writer.update(cx, |store, cx| {
                            store.update(|settings| settings.session_log.timestamp = on, cx)
                        });
                    },
                )
                .default_value(true),
            )
            .description(t!("settings.session_log.timestamp_description"))
            .keywords(keywords(t!("settings.session_log.timestamp_keywords"))),
            SettingItem::new(
                t!("settings.session_log.timestamp_format"),
                SettingField::render(move |options, _, cx| {
                    timestamp_format_field(&format_editor, options, cx)
                })
                .on_reset(
                    move |cx| {
                        format_store
                            .read(cx)
                            .settings()
                            .session_log
                            .timestamp_format
                            != DEFAULT_LOG_TIMESTAMP_FORMAT
                    },
                    move |window, cx| {
                        format_reset.update(cx, |editor, cx| {
                            editor.set_timestamp_format(DEFAULT_LOG_TIMESTAMP_FORMAT, window, cx)
                        });
                    },
                ),
            )
            .description(t!("settings.session_log.timestamp_format_description"))
            .layout(Axis::Vertical)
            .keywords(keywords(t!(
                "settings.session_log.timestamp_format_keywords"
            ))),
        ])
}

/// 终端 → 终端交互: what the mouse does besides selecting.
fn interaction_group(store: &Entity<SettingsStore>) -> SettingGroup {
    let (reader, writer) = (store.clone(), store.clone());
    SettingGroup::new()
        .title(t!("settings.interaction.title"))
        .items([
            SettingItem::new(
                t!("settings.interaction.copy_on_select"),
                SettingField::switch(
                    move |cx| {
                        reader
                            .read(cx)
                            .settings()
                            .terminal_interaction
                            .copy_on_select
                    },
                    move |on, cx| {
                        writer.update(cx, |store, cx| {
                            store.update(
                                |settings| settings.terminal_interaction.copy_on_select = on,
                                cx,
                            )
                        });
                    },
                )
                .default_value(false),
            )
            .description(t!("settings.interaction.copy_on_select_description")),
            SettingItem::new(
                t!("settings.interaction.right_click"),
                choice_field(
                    store,
                    |settings| settings.terminal_interaction.right_click,
                    |settings, action| settings.terminal_interaction.right_click = action,
                ),
            )
            .description(t!("settings.interaction.right_click_description")),
        ])
}

/// 终端 → 通知: when the window is not in front, the system's notification;
/// when another tab is, one in the window.
fn notification_group(store: &Entity<SettingsStore>) -> SettingGroup {
    let switch = |read: fn(&NotificationSettings) -> bool,
                  write: fn(&mut NotificationSettings, bool)| {
        let (reader, writer) = (store.clone(), store.clone());
        SettingField::switch(
            move |cx| read(&reader.read(cx).settings().notifications),
            move |on, cx| {
                writer.update(cx, |store, cx| {
                    store.update(|settings| write(&mut settings.notifications, on), cx)
                });
            },
        )
        .default_value(true)
    };
    SettingGroup::new()
        .title(t!("settings.notifications.title"))
        .description(t!("settings.notifications.description"))
        .items([
            SettingItem::new(
                t!("settings.notifications.programs"),
                switch(
                    |settings| settings.programs,
                    |settings, on| settings.programs = on,
                ),
            )
            .description(t!("settings.notifications.programs_description")),
            SettingItem::new(
                t!("settings.notifications.bell"),
                switch(|settings| settings.bell, |settings, on| settings.bell = on),
            )
            .description(t!("settings.notifications.bell_description")),
        ])
}

/// What the SFTP preview lists after a plain folder and a plain file: what a
/// server's home folder and a project deployed on it hold, each with a color
/// icon of its own, dot files lighter as when the lists show them.
const FILE_ICON_PREVIEW: &[(&str, FileIconKind)] = &[
    (".config", FileIconKind::Folder),
    ("logs", FileIconKind::Folder),
    ("scripts", FileIconKind::Folder),
    (".bashrc", FileIconKind::File),
    ("deploy.sh", FileIconKind::File),
    ("docker-compose.yml", FileIconKind::File),
    ("nginx.conf", FileIconKind::File),
    ("backup.tar.gz", FileIconKind::File),
];

/// SFTP: how many files move at once, the right sidebar's 文件, and the
/// file lists' icons and how they look.
fn sftp_groups(panel: &SettingsPanel, _: &App) -> Vec<SettingGroup> {
    let (follow_reader, follow_writer) = (panel.store.clone(), panel.store.clone());
    vec![
        SettingGroup::new()
            .title(t!("settings.sftp.transfers"))
            .items([SettingItem::new(
                t!("settings.sftp.parallel"),
                parallel_transfers_field(&panel.store),
            )
            .description(t!("settings.sftp.parallel_description"))
            .keywords(keywords(t!("settings.sftp.parallel_keywords")))]),
        SettingGroup::new()
            .title(t!("settings.sftp.file_sidebar"))
            .items([SettingItem::new(
                t!("settings.sftp.detect_follow"),
                SettingField::switch(
                    move |cx| {
                        follow_reader
                            .read(cx)
                            .settings()
                            .file_sidebar
                            .detect_directory_follow
                    },
                    move |on, cx| {
                        follow_writer.update(cx, |store, cx| {
                            store.update(
                                |settings| settings.file_sidebar.detect_directory_follow = on,
                                cx,
                            )
                        });
                    },
                )
                .default_value(true),
            )
            .description(t!("settings.sftp.detect_follow_description"))
            .keywords(keywords(t!("settings.sftp.follow_keywords")))]),
        SettingGroup::new()
            .title(t!("settings.sftp.file_list"))
            .items([
                SettingItem::new(
                    t!("settings.sftp.icon_style"),
                    choice_field(
                        &panel.store,
                        |settings| settings.file_icon_style,
                        |settings, style| settings.file_icon_style = style,
                    ),
                )
                .description(t!("settings.sftp.icon_style_description")),
                SettingItem::render(|_, _, cx| {
                    let row = |id: &str, label: SharedString, icon: FileIcon, hidden: bool| {
                        h_flex()
                            .id(ElementId::Name(format!("preview:{id}").into()))
                            .min_w_0()
                            .gap_2()
                            .when(hidden, |row| row.text_color(cx.theme().muted_foreground))
                            .child(icon.dimmed(hidden))
                            .child(div().min_w_0().truncate().child(label))
                            .test_support()
                    };
                    // The plain ones first, side by side, called what they
                    // are. Named nothing, they get the plain icons whatever
                    // the label says.
                    let plain = [
                        (
                            "folder",
                            t!("settings.sftp.preview_folder"),
                            FileIconKind::Folder,
                        ),
                        ("file", t!("settings.sftp.preview_file"), FileIconKind::File),
                    ]
                    .into_iter()
                    .map(|(id, label, kind)| row(id, label, FileIcon::new("", kind), false));
                    let named = FILE_ICON_PREVIEW.iter().map(|&(name, kind)| {
                        row(
                            name,
                            name.into(),
                            FileIcon::new(name, kind),
                            name.starts_with('.'),
                        )
                    });
                    v_flex()
                        .w_full()
                        .gap_2()
                        .child(div().text_sm().child(t!("settings.preview")))
                        .child(
                            div()
                                .id("file-icon-preview")
                                .test_support()
                                .w_full()
                                .grid()
                                .grid_cols(2)
                                .gap_x_4()
                                .gap_y_1()
                                .p_3()
                                .rounded(cx.theme().radius)
                                .border_1()
                                .border_color(cx.theme().border)
                                .bg(cx.theme().tokens.table)
                                .text_sm()
                                .children(plain.chain(named)),
                        )
                })
                .keywords(keywords(t!("settings.sftp.keywords"))),
            ]),
    ]
}

/// What the 关键字高亮 preview shows: a service's log followed on a server,
/// with what the example rules color (levels and addresses) and lines they
/// leave alone.
const HIGHLIGHT_PREVIEW_LINES: &[&str] = &[
    "root@web-01:~# tail -f /var/log/orders/app.log",
    "09:12:03 INFO  orders listening on 0.0.0.0:8080",
    "09:12:41 WARN  slow query: 2.31s on table order_items",
    "09:13:07 ERROR upstream 10.20.3.15:5432 refused connection",
    "09:13:08 INFO  retrying in 5s (attempt 2/5)",
];

/// 关键字高亮: the switch, a preview of the rules in the terminal's font and
/// colors, and the rules.
fn highlight_groups(panel: &SettingsPanel, cx: &App) -> Vec<SettingGroup> {
    let (reader, writer) = (panel.store.clone(), panel.store.clone());
    let count = panel
        .store
        .read(cx)
        .settings()
        .terminal_highlight
        .rules
        .len();
    let editor = panel.highlight_rules.clone();
    vec![
        SettingGroup::new()
            .title(t!("settings.highlight.options"))
            .items([SettingItem::new(
                t!("settings.highlight.enabled"),
                SettingField::switch(
                    move |cx| reader.read(cx).settings().terminal_highlight.enabled,
                    move |enabled, cx| {
                        writer.update(cx, |store, cx| {
                            store.update(
                                |settings| settings.terminal_highlight.enabled = enabled,
                                cx,
                            )
                        });
                    },
                )
                .default_value(true),
            )
            .description(t!("settings.highlight.enabled_description"))]),
        SettingGroup::new()
            .title(t!("settings.preview"))
            .description(t!("settings.highlight.preview_description"))
            .items([highlight_preview_item()]),
        SettingGroup::new()
            .title(t!("settings.highlight.rules", count = count))
            .description(t!("settings.highlight.rules_description"))
            .items([SettingItem::render(move |_, _, _| editor.clone())
                .keywords(keywords(t!("settings.highlight.rules_keywords")))]),
    ]
}

/// The preview: sample output in the terminal's font and theme, colored by
/// the rules in effect, as a terminal would show it. The group's outline is
/// its frame; the theme's background is set in from it.
fn highlight_preview_item() -> SettingItem {
    SettingItem::render(|_, window, cx| {
        let rules = TerminalHighlights::current(cx);
        let font = TerminalFont::current(cx);
        let colors = TerminalColors::current(cx);
        div()
            .id("highlight-preview")
            .test_support()
            .w_full()
            .p_3()
            .rounded(cx.theme().radius)
            .bg(colors.background())
            .text_color(colors.foreground())
            .font_family(font.family(cx))
            .text_size(font.size)
            .line_height(font.row_height(window))
            .overflow_hidden()
            .children(HIGHLIGHT_PREVIEW_LINES.iter().map(|line| {
                let highlights = rules.spans(line).into_iter().map(|(range, color)| {
                    (
                        range,
                        HighlightStyle {
                            color: Some(color.hsla()),
                            ..HighlightStyle::default()
                        },
                    )
                });
                div()
                    .whitespace_nowrap()
                    .child(StyledText::new(*line).with_highlights(highlights))
            }))
    })
    .keywords(keywords(t!("settings.highlight.preview_keywords")))
}

/// 键盘快捷键.
fn shortcuts_groups(panel: &SettingsPanel, _: &App) -> Vec<SettingGroup> {
    shortcut_groups(&panel.shortcuts)
}

/// 同步.
fn sync_groups(panel: &SettingsPanel, cx: &App) -> Vec<SettingGroup> {
    super::sync_page::sync_groups(&panel.sync, cx)
}

/// 应用 → 窗口: what the next launch restores of the main window.
fn application_groups(panel: &SettingsPanel, _: &App) -> Vec<SettingGroup> {
    let store = &panel.store;
    let switch = |read: fn(&WindowSettings) -> bool, write: fn(&mut WindowSettings, bool)| {
        let (reader, writer) = (store.clone(), store.clone());
        SettingField::switch(
            move |cx| read(&reader.read(cx).settings().window),
            move |on, cx| {
                writer.update(cx, |store, cx| {
                    store.update(|settings| write(&mut settings.window, on), cx)
                });
            },
        )
        .default_value(true)
    };
    vec![
        SettingGroup::new()
            .title(t!("settings.window.title"))
            .items([
                SettingItem::new(
                    t!("settings.window.remember_size"),
                    switch(
                        |window| window.remember_size,
                        |window, on| window.remember_size = on,
                    ),
                )
                .description(t!("settings.window.remember_size_description")),
                SettingItem::new(
                    t!("settings.window.remember_position"),
                    switch(
                        |window| window.remember_position,
                        |window, on| window.remember_position = on,
                    ),
                )
                .description(t!("settings.window.remember_position_description")),
            ]),
    ]
}

/// 外部 CLI: whether the `shellrs` command may use the saved hosts, the
/// command itself, and the skill that teaches agents to use it.
fn external_cli_groups(panel: &SettingsPanel, cx: &App) -> Vec<SettingGroup> {
    let (reader, writer) = (panel.store.clone(), panel.store.clone());
    let integration = panel.integration.read(cx);
    let installable = integration.paths().is_some();
    let status = integration.status().cloned();
    let paths = integration.paths().cloned();
    let (bin_link, skill_files) = match &paths {
        Some(paths) => (
            Some(paths.bin_link.display().to_string()),
            Some(AgentKind::ALL.map(|agent| paths.skill_file(agent).display().to_string())),
        ),
        None => (None, None),
    };
    vec![
        SettingGroup::new()
            .title(t!("settings.cli.access"))
            .items([SettingItem::new(
                t!("settings.cli.enabled"),
                SettingField::switch(
                    move |cx| reader.read(cx).settings().external_cli.enabled,
                    move |enabled, cx| {
                        writer.update(cx, |store, cx| {
                            store.update(|settings| settings.external_cli.enabled = enabled, cx)
                        });
                    },
                )
                .default_value(false),
            )
            .description(t!("settings.cli.enabled_description"))]),
        SettingGroup::new()
            .title(t!("settings.cli.binary"))
            .description(if crate::app::portable_root().is_some() {
                t!("settings.cli.binary_description_portable")
            } else if cfg!(windows) {
                t!("settings.cli.binary_description_windows")
            } else {
                t!("settings.cli.binary_description")
            })
            .items([binary_item(bin_link, status.clone())]),
        SettingGroup::new()
            .title("Agent Skills")
            .description(t!("settings.cli.skills_description"))
            .items([
                skill_actions_item(installable),
                skills_item(skill_files, status),
            ]),
    ]
}

/// 关于: 应用更新, as three rows: the version with where updating stands,
/// the channel, and 自动升级; then 隐私, whether usage statistics are sent.
fn about_groups(panel: &SettingsPanel, cx: &App) -> Vec<SettingGroup> {
    let snapshot = panel.updater.read(cx).snapshot();
    let (reader, writer) = (panel.store.clone(), panel.store.clone());
    let (analytics_reader, analytics_writer) = (panel.store.clone(), panel.store.clone());
    vec![
        SettingGroup::new()
            .title(t!("settings.about.updates"))
            .items([
                current_version_item(snapshot),
                SettingItem::new(
                    t!("settings.about.channel"),
                    choice_field(
                        &panel.store,
                        |settings| settings.update.channel,
                        |settings, channel| settings.update.channel = channel,
                    ),
                )
                .description(t!("settings.about.channel_description")),
                SettingItem::new(
                    t!("settings.about.automatic"),
                    SettingField::switch(
                        move |cx| reader.read(cx).settings().update.automatic,
                        move |automatic, cx| {
                            writer.update(cx, |store, cx| {
                                store.update(|settings| settings.update.automatic = automatic, cx)
                            });
                        },
                    )
                    .default_value(true),
                )
                .description(t!("settings.about.automatic_description")),
            ]),
        SettingGroup::new()
            .title(t!("settings.about.privacy"))
            .items([SettingItem::new(
                t!("settings.about.analytics"),
                SettingField::switch(
                    move |cx| analytics_reader.read(cx).settings().analytics.enabled,
                    move |enabled, cx| {
                        analytics_writer.update(cx, |store, cx| {
                            store.update(|settings| settings.analytics.enabled = enabled, cx)
                        });
                    },
                )
                .default_value(true),
            )
            .description(t!("settings.about.analytics_description"))]),
        SettingGroup::new()
            .title("ShellRS")
            .items([SettingItem::new(
                t!("settings.about.website"),
                SettingField::render(|options, _, _| {
                    Button::new("open-website")
                        .outline()
                        .with_size(options.size())
                        .icon(Icon::new(CatalogIcon::ExternalLink))
                        .label("shellrs.com")
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenWebsite), cx))
                }),
            )
            .description(t!("settings.about.website_description"))]),
    ]
}

/// 当前版本: where updating stands under the title, and the version with the
/// one button that applies: 检查更新, or the step the version on offer
/// needs. Laid out like the page's other rows.
fn current_version_item(snapshot: UpdateSnapshot) -> SettingItem {
    SettingItem::render(move |options, _, cx| {
        let (text, tone) = snapshot.status_line();
        let color = match tone {
            Tone::Plain => cx.theme().muted_foreground,
            Tone::Warning => cx.theme().warning,
            Tone::Danger => cx.theme().danger,
        };
        let version = SharedString::from(format!("v{}", build_info::VERSION));
        let build = SharedString::from(build_description());
        let step = snapshot.step().map(|step| -> (_, _, Box<dyn Action>) {
            match step {
                UpdateStep::Restart => (
                    "show-update",
                    t!("settings.about.restart"),
                    Box::new(ShowUpdate),
                ),
                UpdateStep::DownloadPage => (
                    "open-download-page",
                    t!("settings.about.download_page"),
                    Box::new(OpenDownloadPage),
                ),
                UpdateStep::Download { retry } => (
                    "download-update",
                    if retry {
                        t!("settings.about.retry_download")
                    } else {
                        t!("settings.about.download")
                    },
                    Box::new(DownloadUpdate),
                ),
            }
        });
        let button = match step {
            Some((id, label, action)) => Some(
                Button::new(id)
                    .primary()
                    .with_size(options.size())
                    .label(label)
                    .on_click(move |_, window, cx| {
                        window.dispatch_action(action.boxed_clone(), cx)
                    }),
            ),
            None => (snapshot.phase != Phase::Off).then(|| {
                Button::new("check-for-updates")
                    .outline()
                    .with_size(options.size())
                    .icon(Icon::new(CatalogIcon::RefreshCw))
                    .label(t!("settings.about.check"))
                    .loading(snapshot.phase == Phase::Checking)
                    .disabled(!snapshot.can_check())
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(CheckForUpdates), cx))
            }),
        };
        // A narrow page stacks each row, the controls under the text.
        let stacked = options.layout() == Axis::Vertical;
        let row = if stacked {
            v_flex()
        } else {
            h_flex().justify_between().items_center()
        };
        row.w_full()
            .gap_3()
            .child(
                v_flex()
                    .map(|text| {
                        if stacked {
                            text.w_full()
                        } else {
                            text.flex_1().max_w_3_5()
                        }
                    })
                    .child(Label::new(t!("settings.about.version")).text_sm())
                    .child(
                        div()
                            .id("update-status")
                            .test_support()
                            .aria_label(text.clone())
                            .text_sm()
                            .text_color(color)
                            .child(text),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .flex_shrink_0()
                    .child(
                        div()
                            .id("about-version")
                            .test_support()
                            .aria_label(version.clone())
                            .text_sm()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_color(cx.theme().muted_foreground)
                            .tooltip(move |window, cx| {
                                Tooltip::new(build.clone()).build(window, cx)
                            })
                            .child(version),
                    )
                    .children(button),
            )
    })
    .keywords(keywords(t!("settings.about.version_keywords")))
}

/// The version's tooltip: which build this is, and for which system.
fn build_description() -> String {
    let build = match (build_info::COMMIT, build_info::BUILD_DATE) {
        (Some(commit), Some(date)) => format!("{} · {date}", commit.get(..7).unwrap_or(commit)),
        (Some(commit), None) => commit.get(..7).unwrap_or(commit).to_string(),
        _ => t!("settings.about.local_build").to_string(),
    };
    let mut platform = platform::platform_label(crate::app::portable_root().is_some()).to_string();
    if crate::app::software_rendering() {
        platform = format!("{platform} · {}", t!("settings.about.software_rendering"));
    }
    t!("settings.about.build", build = build, platform = platform).to_string()
}

/// Where the `shellrs` command stands, with the one thing to do about it.
fn binary_item(bin_link: Option<String>, status: Option<IntegrationStatus>) -> SettingItem {
    SettingItem::render(move |_, _, cx| {
        let binary = status.as_ref().map(|status| &status.binary);
        let (line, action) = match (&bin_link, binary) {
            (None, _) => (
                RowStatus::Plain(t!("settings.cli.binary_unsupported").into()),
                None,
            ),
            (Some(_), None) => (RowStatus::Plain(t!("settings.cli.checking").into()), None),
            (Some(path), Some(BinaryStatus::Installed)) => (
                RowStatus::Installed(path.clone()),
                Some((t!("settings.cli.remove"), false)),
            ),
            (Some(path), Some(BinaryStatus::Missing)) => (
                RowStatus::Plain(t!("settings.cli.missing", path = path).into()),
                Some((t!("settings.cli.install"), true)),
            ),
            (Some(path), Some(BinaryStatus::Stale { target })) => (
                RowStatus::Warning(
                    t!(
                        "settings.cli.binary_stale",
                        path = path,
                        target = target.display()
                    )
                    .into(),
                ),
                Some((t!("settings.cli.reinstall"), true)),
            ),
            (Some(path), Some(BinaryStatus::Occupied { target })) => (
                RowStatus::Warning(match target {
                    Some(target) => t!(
                        "settings.cli.binary_occupied_by",
                        path = path,
                        target = target.display()
                    )
                    .into(),
                    None => t!("settings.cli.binary_occupied", path = path).into(),
                }),
                None,
            ),
            (Some(path), Some(BinaryStatus::Outdated)) => (
                RowStatus::Warning(t!("settings.cli.outdated", path = path).into()),
                Some((t!("settings.cli.update"), true)),
            ),
            (Some(path), Some(BinaryStatus::NotOnPath)) => (
                RowStatus::Warning(t!("settings.cli.binary_not_on_path", path = path).into()),
                Some((t!("settings.cli.install"), true)),
            ),
            (Some(_), Some(BinaryStatus::Unavailable)) => (
                RowStatus::Warning(t!("settings.cli.binary_unavailable").into()),
                None,
            ),
        };
        h_flex()
            .w_full()
            .justify_between()
            .items_center()
            .gap_3()
            .child(
                v_flex()
                    .gap_1()
                    .min_w_0()
                    .child(div().text_sm().child(t!("settings.cli.status")))
                    .child(line.render("cli-binary-status", cx)),
            )
            .children(action.map(|(label, install)| {
                row_button("cli-binary", label, install).on_click(move |_, window, cx| {
                    if install {
                        window.dispatch_action(Box::new(InstallCliCommand), cx);
                    } else {
                        window.dispatch_action(Box::new(RemoveCliCommand), cx);
                    }
                })
            }))
    })
    .keywords(keywords(t!("settings.cli.binary_keywords")))
}

/// 检查状态 and 复制 skills, above the agents.
fn skill_actions_item(installable: bool) -> SettingItem {
    SettingItem::render(move |_, _, _| {
        h_flex()
            .gap_2()
            .child(
                Button::new("check-cli-integration")
                    .outline()
                    .small()
                    .icon(Icon::new(CatalogIcon::RefreshCw))
                    .label(t!("settings.cli.check"))
                    .disabled(!installable)
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(RefreshCliIntegration), cx)
                    }),
            )
            .child(
                Button::new("copy-agent-skill")
                    .outline()
                    .small()
                    .icon(Icon::new(IconName::Copy))
                    .label(t!("settings.cli.copy_skills"))
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(CopyAgentSkill), cx)),
            )
    })
    .keywords(keywords(t!("settings.cli.skills_keywords")))
}

/// One row per agent, each with where its skill goes and whether it is
/// there.
fn skills_item(
    files: Option<[String; AgentKind::ALL.len()]>,
    status: Option<IntegrationStatus>,
) -> SettingItem {
    SettingItem::render(move |_, _, cx| {
        v_flex()
            .w_full()
            .gap_3()
            .children(AgentKind::ALL.iter().enumerate().map(|(ix, agent)| {
                let file = files.as_ref().map(|files| files[ix].clone());
                let skill = status.as_ref().map(|status| status.skill(*agent));
                v_flex()
                    .gap_3()
                    .when(ix > 0, |row| row.child(Separator::horizontal()))
                    .child(skill_row(*agent, file, skill, cx))
            }))
    })
    .keywords(["skill", "Agent", "Codex", "Claude", "OpenCode", "WorkBuddy"])
}

fn skill_row(
    agent: AgentKind,
    file: Option<String>,
    status: Option<SkillStatus>,
    cx: &App,
) -> impl IntoElement {
    let (line, action) = match (&file, status) {
        (None, _) => (
            RowStatus::Plain(t!("settings.cli.skill_unsupported").into()),
            None,
        ),
        (Some(_), None) => (RowStatus::Plain(t!("settings.cli.checking").into()), None),
        (Some(file), Some(SkillStatus::Installed)) => (
            RowStatus::Installed(file.clone()),
            Some((t!("settings.cli.remove"), false)),
        ),
        (Some(file), Some(SkillStatus::Missing)) => (
            RowStatus::Plain(t!("settings.cli.missing", path = file).into()),
            Some((t!("settings.cli.install"), true)),
        ),
        (Some(file), Some(SkillStatus::Outdated)) => (
            RowStatus::Warning(t!("settings.cli.outdated", path = file).into()),
            Some((t!("settings.cli.update"), true)),
        ),
    };
    h_flex()
        .w_full()
        .justify_between()
        .items_center()
        .gap_3()
        .child(
            v_flex()
                .gap_1()
                .min_w_0()
                .child(div().text_sm().child(agent.label()))
                .children(agent.description().map(|description| {
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(description)
                }))
                .child(line.render(
                    SharedString::from(format!("skill-status-{}", agent.key())),
                    cx,
                )),
        )
        .children(action.map(|(label, install)| {
            row_button(
                SharedString::from(format!("skill-{}", agent.key())),
                label,
                install,
            )
            .on_click(move |_, window, cx| {
                if install {
                    window.dispatch_action(Box::new(InstallAgentSkill(agent)), cx);
                } else {
                    window.dispatch_action(Box::new(RemoveAgentSkill(agent)), cx);
                }
            })
        }))
}

/// Installing is the step the row asks for, so it is the primary button;
/// removing is not.
fn row_button(id: impl Into<ElementId>, label: SharedString, install: bool) -> Button {
    let button = Button::new(id).small().label(label);
    if install {
        button.primary()
    } else {
        button.outline()
    }
}

/// The line under a row's name that says where things stand.
enum RowStatus {
    /// ✓ 已安装于 the path, in the success colour.
    Installed(String),
    Plain(String),
    Warning(String),
}

impl RowStatus {
    /// With `id` and what it says as its label, for tests.
    fn render(self, id: impl Into<ElementId>, cx: &App) -> AnyElement {
        let line = div().id(id).test_support().text_sm();
        match self {
            RowStatus::Installed(path) => line
                .aria_label(t!("settings.cli.installed_at_path", path = path))
                .text_color(cx.theme().success)
                .child(
                    h_flex()
                        .gap_1()
                        .child(Icon::new(IconName::Check).small())
                        .child(t!("settings.cli.installed_at"))
                        .child(
                            div()
                                .font_family(cx.theme().mono_font_family.clone())
                                .child(path),
                        ),
                ),
            RowStatus::Plain(text) => line
                .aria_label(text.clone())
                .text_color(cx.theme().muted_foreground)
                .child(text),
            RowStatus::Warning(text) => line
                .aria_label(text.clone())
                .text_color(cx.theme().warning)
                .child(text),
        }
        .into_any_element()
    }
}

/// The terminal font family: the monospace families found so far, with the
/// theme's own marked as the default. Choosing the default stores nothing,
/// so the file keeps following the platform's default.
fn font_family_field(panel: &SettingsPanel, cx: &App) -> SettingField<SharedString> {
    let default_family = cx.theme().mono_font_family.clone();
    let mut families = panel.font_families.to_vec();
    if !families.contains(&default_family) {
        families.insert(0, default_family.clone());
    }
    let options = families
        .into_iter()
        .map(|family| {
            let label = if family == default_family {
                t!("settings.default_option", value = family)
            } else {
                family.clone()
            };
            (family, label)
        })
        .collect();

    let (reader, writer) = (panel.store.clone(), panel.store.clone());
    let unset = default_family.clone();
    SettingField::scrollable_dropdown(
        options,
        move |cx| {
            // A family no longer installed shows as what the terminal uses.
            reader
                .read(cx)
                .settings()
                .terminal_font
                .family
                .filter(|family| is_font_installed(family, cx))
                .map(SharedString::from)
                .unwrap_or_else(|| cx.theme().mono_font_family.clone())
        },
        move |family: SharedString, cx| {
            let family = (family != unset).then(|| family.to_string());
            writer.update(cx, |store, cx| {
                store.update(|settings| settings.terminal_font.family = family, cx)
            });
        },
    )
    .default_value(default_family)
}

/// A font size, a dropdown of the whole pixels in `range`. Not a number
/// input: that clamps to its minimum as each digit is typed, so 「14」 would
/// turn into 10 at its 「1」.
fn font_size_field(
    store: &Entity<SettingsStore>,
    range: std::ops::RangeInclusive<f32>,
    read: fn(&AppSettings) -> f32,
    write: fn(&mut AppSettings, f32),
) -> SettingField<SharedString> {
    let default = read(&AppSettings::default());
    let (start, end) = (*range.start() as u32, *range.end() as u32);
    let options = (start..=end)
        .map(|size| {
            let key = SharedString::from(size.to_string());
            let label = if size as f32 == default {
                t!("settings.default_option", value = size)
            } else {
                key.clone()
            };
            (key, label)
        })
        .collect();
    let (reader, writer) = (store.clone(), store.clone());
    SettingField::scrollable_dropdown(
        options,
        move |cx| read(&reader.read(cx).settings()).to_string().into(),
        move |size: SharedString, cx| {
            if let Ok(size) = size.parse::<f32>() {
                writer.update(cx, |store, cx| {
                    store.update(|settings| write(settings, size), cx)
                });
            }
        },
    )
    .default_value(default.to_string())
}

/// 最大同时传输数量, a dropdown of whole numbers as the font size's is.
fn parallel_transfers_field(store: &Entity<SettingsStore>) -> SettingField<SharedString> {
    let options = PARALLEL_TRANSFERS_RANGE
        .map(|count| {
            let key = SharedString::from(count.to_string());
            let label = if count == DEFAULT_PARALLEL_TRANSFERS {
                t!("settings.default_option", value = count)
            } else {
                key.clone()
            };
            (key, label)
        })
        .collect();
    let (reader, writer) = (store.clone(), store.clone());
    SettingField::dropdown(
        options,
        move |cx| {
            reader
                .read(cx)
                .settings()
                .transfers
                .parallel
                .to_string()
                .into()
        },
        move |count: SharedString, cx| {
            if let Ok(count) = count.parse::<usize>() {
                writer.update(cx, |store, cx| {
                    store.update(|settings| settings.transfers.parallel = count, cx)
                });
            }
        },
    )
    .default_value(DEFAULT_PARALLEL_TRANSFERS.to_string())
}

/// A number input over one number of the settings. The store rounds what
/// is typed or stepped, and the input shows the rounded value back.
fn number_field(
    store: &Entity<SettingsStore>,
    options: NumberFieldOptions,
    read: fn(&AppSettings) -> f32,
    write: fn(&mut AppSettings, f32),
) -> SettingField<f64> {
    let (reader, writer) = (store.clone(), store.clone());
    SettingField::number_input(
        options,
        // Through the decimal text, so 1.54 reads as 1.54 and not as the
        // double nearest to the float nearest to it.
        move |cx| {
            read(&reader.read(cx).settings())
                .to_string()
                .parse()
                .unwrap_or_default()
        },
        move |value, cx| {
            writer.update(cx, |store, cx| {
                store.update(|settings| write(settings, value as f32), cx)
            });
        },
    )
}

/// A row's search words, from a translation that lists them separated by
/// commas: they are in the interface language like everything the search
/// matches.
pub(super) fn keywords(words: SharedString) -> Vec<SharedString> {
    words
        .split(',')
        .map(|word| SharedString::from(word.trim().to_string()))
        .collect()
}

/// A dropdown over one `Choice` of the settings. Its reset goes back to the
/// default, which is what the page's 「重置全部」 uses.
fn choice_field<T: Choice>(
    store: &Entity<SettingsStore>,
    read: fn(&AppSettings) -> T,
    write: fn(&mut AppSettings, T),
) -> SettingField<SharedString> {
    let options = T::ALL
        .iter()
        .map(|choice| (choice.key().into(), choice.label()))
        .collect();
    let (reader, writer) = (store.clone(), store.clone());
    SettingField::dropdown(
        options,
        move |cx| read(&reader.read(cx).settings()).key().into(),
        move |key: SharedString, cx| {
            if let Some(choice) = T::from_key(&key) {
                writer.update(cx, |store, cx| {
                    store.update(|settings| write(settings, choice), cx)
                });
            }
        },
    )
    .default_value(read(&AppSettings::default()).key())
}

/// The settings tab's commands, shared by its context menu and the tab bar's
/// 「…」 menu: only the close family.
fn tab_menu(
    menu: PopupMenu,
    group: Option<WeakEntity<TabGroup>>,
    panel: EntityId,
    cx: &App,
) -> PopupMenu {
    close_tab_items(menu, CenterTab::Settings, group, panel, cx)
}
