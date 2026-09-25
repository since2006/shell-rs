use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _,
    dock::{BasePanel, Panel, PanelEvent, TabGroup},
    group_box::GroupBoxVariant,
    menu::PopupMenu,
    setting::{NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings},
    v_flex,
};
use gpui_kit::*;

use crate::app::{CatalogIcon, CenterTab, CloseSettings};
use crate::shared::{ClosableTabTitle, close_tab_items};
use crate::terminal::{
    FONT_SIZE_RANGE, LINE_HEIGHT_RANGE, TerminalFontPreview, is_font_installed,
    monospace_font_families,
};

use super::{AppSettings, Choice, SettingsStore};

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
    /// The monospace families to choose the terminal font from; empty until
    /// the scan in the background comes back.
    font_families: &'static [SharedString],
    focus_handle: FocusHandle,
    tab_group: Option<WeakEntity<TabGroup>>,
    _subscription: Subscription,
    _font_scan: Task<()>,
}

impl SettingsPanel {
    pub fn new(store: Entity<SettingsStore>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&store, |_, _, cx| cx.notify());
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
            font_families: &[],
            focus_handle: cx.focus_handle(),
            tab_group: None,
            _subscription: subscription,
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
            "设置",
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
                    .pages(CATEGORIES.iter().map(|category| category.page(self, cx))),
            )
    }
}

/// One category of the left column.
struct Category {
    title: &'static str,
    icon: CatalogIcon,
    /// The category's settings. `Settings` leaves out a page without groups,
    /// left column entry and all, so a category needs at least one.
    groups: CategoryGroups,
}

/// Builds a category's setting groups over the store they edit.
type CategoryGroups = fn(&SettingsPanel, &App) -> Vec<SettingGroup>;

/// The categories, in the order the left column lists them.
const CATEGORIES: [Category; 2] = [
    Category {
        title: "外观",
        icon: CatalogIcon::Palette,
        groups: appearance_groups,
    },
    Category {
        title: "终端",
        icon: CatalogIcon::Terminal,
        groups: terminal_groups,
    },
];

impl Category {
    fn page(&self, panel: &SettingsPanel, cx: &App) -> SettingPage {
        SettingPage::new(self.title)
            .icon(Icon::new(self.icon))
            .groups((self.groups)(panel, cx))
    }
}

/// 外观.
fn appearance_groups(panel: &SettingsPanel, _: &App) -> Vec<SettingGroup> {
    let store = &panel.store;
    vec![
        SettingGroup::new().title("常规").items([
            SettingItem::new(
                "界面语言",
                choice_field(
                    store,
                    |settings| settings.language,
                    |settings, language| settings.language = language,
                ),
            )
            .description("英文界面尚在翻译中，目前只有部分文字会切换。"),
            SettingItem::new(
                "应用外观",
                choice_field(
                    store,
                    |settings| settings.appearance,
                    |settings, appearance| settings.appearance = appearance,
                ),
            ),
        ]),
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
    "中文预览：你好，世界！",
    "┌──┬──┐ ░▒▓█ ←↑→↓ ✓",
    "root@web-01:~$ tail -f /var/log/syslog",
];

/// 终端.
fn terminal_groups(panel: &SettingsPanel, cx: &App) -> Vec<SettingGroup> {
    let store = &panel.store;
    let defaults = AppSettings::default().terminal_font;
    vec![
        SettingGroup::new().title("字体配置").items([
            SettingItem::new("字体", font_family_field(panel, cx)).description("只列出等宽字体。"),
            SettingItem::new("字号", font_size_field(store)).description("以像素计。"),
            SettingItem::new(
                "行高",
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
            .description("字号的倍数。"),
            SettingItem::render(|_, _, cx| {
                v_flex()
                    .w_full()
                    .gap_2()
                    .child(div().text_sm().child("预览"))
                    .child(
                        div()
                            .w_full()
                            .p_3()
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().background)
                            .overflow_hidden()
                            .child(TerminalFontPreview::new(
                                "terminal-font-preview",
                                PREVIEW_LINES,
                            )),
                    )
            })
            .keywords(["预览", "字体", "字号", "行高"]),
        ]),
    ]
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
                format!("{family}（默认）").into()
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

/// The terminal font size, a dropdown of whole pixels. Not a number input:
/// that clamps to its minimum as each digit is typed, so 「14」 would turn
/// into 8 at its 「1」.
fn font_size_field(store: &Entity<SettingsStore>) -> SettingField<SharedString> {
    let default = AppSettings::default().terminal_font.size;
    let (start, end) = (
        *FONT_SIZE_RANGE.start() as u32,
        *FONT_SIZE_RANGE.end() as u32,
    );
    let options = (start..=end)
        .map(|size| {
            let key = SharedString::from(size.to_string());
            let label = if size as f32 == default {
                format!("{size}（默认）").into()
            } else {
                key.clone()
            };
            (key, label)
        })
        .collect();
    let (reader, writer) = (store.clone(), store.clone());
    SettingField::scrollable_dropdown(
        options,
        move |cx| {
            reader
                .read(cx)
                .settings()
                .terminal_font
                .size
                .to_string()
                .into()
        },
        move |size: SharedString, cx| {
            if let Ok(size) = size.parse::<f32>() {
                writer.update(cx, |store, cx| {
                    store.update(|settings| settings.terminal_font.size = size, cx)
                });
            }
        },
    )
    .default_value(default.to_string())
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

/// A dropdown over one `Choice` of the settings. Its reset goes back to the
/// default, which is what the page's 「重置全部」 uses.
fn choice_field<T: Choice>(
    store: &Entity<SettingsStore>,
    read: fn(&AppSettings) -> T,
    write: fn(&mut AppSettings, T),
) -> SettingField<SharedString> {
    let options = T::ALL
        .iter()
        .map(|choice| (choice.key().into(), choice.label().into()))
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
