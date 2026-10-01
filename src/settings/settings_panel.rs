use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    dock::{BasePanel, Panel, PanelEvent, TabGroup},
    group_box::GroupBoxVariant,
    h_flex,
    link::Link,
    menu::PopupMenu,
    separator::Separator,
    setting::{NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings},
    text::TextView,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{
    CatalogIcon, CenterTab, CheckForUpdates, CloseSettings, CopyAgentSkill, DownloadUpdate,
    InstallAgentSkill, InstallCliCommand, OpenDownloadPage, RefreshCliIntegration,
    RemoveAgentSkill, RemoveCliCommand, ShowUpdate,
};
use crate::cli::{AgentKind, BinaryStatus, CliIntegration, IntegrationStatus, SkillStatus};
use crate::shared::{ClosableTabTitle, close_tab_items};
use crate::terminal::{
    FONT_SIZE_RANGE, LINE_HEIGHT_RANGE, TerminalFontPreview, is_font_installed,
    monospace_font_families,
};
use crate::update::{Phase, Tone, UpdateSnapshot, Updater, build_info, platform};

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
    /// What of the external CLI is installed, for 外部 CLI.
    integration: Entity<CliIntegration>,
    /// Where updating stands, for 关于.
    updater: Entity<Updater>,
    /// The monospace families to choose the terminal font from; empty until
    /// the scan in the background comes back.
    font_families: &'static [SharedString],
    focus_handle: FocusHandle,
    tab_group: Option<WeakEntity<TabGroup>>,
    _subscriptions: [Subscription; 3],
    _font_scan: Task<()>,
}

impl SettingsPanel {
    pub fn new(
        store: Entity<SettingsStore>,
        integration: Entity<CliIntegration>,
        updater: Entity<Updater>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = [
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.observe(&integration, |_, _, cx| cx.notify()),
            cx.observe(&updater, |_, _, cx| cx.notify()),
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
            font_families: &[],
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
const CATEGORIES: [Category; 4] = [
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
    Category {
        title: "外部 CLI",
        icon: CatalogIcon::SquareTerminal,
        groups: external_cli_groups,
    },
    Category {
        title: "关于",
        icon: CatalogIcon::Info,
        groups: about_groups,
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

/// 外部 CLI: whether the `shellrs` command may use the saved sessions, the
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
            .title("访问控制")
            .items([SettingItem::new(
                "启用外部 CLI",
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
            .description(
                "开启后，本机当前用户下的程序（如 AI Agent）可以通过 shellrs 命令，\
             在已保存的主机上执行命令、传输文件，无需知道密码。",
            )]),
        SettingGroup::new()
            .title("CLI 二进制")
            .description(if cfg!(windows) {
                "把 shellrs 命令加入当前用户的 PATH，方便在终端和 Agent 中直接调用。\
                 重新打开的终端和 Agent 才能找到它。"
            } else {
                "把 shellrs 命令加入 PATH，方便在终端和 Agent 中直接调用。"
            })
            .items([binary_item(bin_link, status.clone())]),
        SettingGroup::new()
            .title("Agent Skills")
            .description(
                "安装 shellrs skill，让外部 Agent 在连接服务器、执行远程命令和传输文件时\
                 使用 shellrs CLI。可安装到通用目录或指定 Agent 的专用目录。",
            )
            .items([
                skill_actions_item(installable),
                skills_item(skill_files, status),
            ]),
    ]
}

/// 关于: which ShellRS this is, and updating it.
fn about_groups(panel: &SettingsPanel, cx: &App) -> Vec<SettingGroup> {
    let snapshot = panel.updater.read(cx).snapshot();
    let (reader, writer) = (panel.store.clone(), panel.store.clone());
    let notes = snapshot
        .release
        .as_ref()
        .filter(|release| !release.notes.trim().is_empty())
        .map(|release| (release.version.to_string(), release.notes.clone()));
    let mut groups = vec![
        SettingGroup::new().title("ShellRS").items([version_item()]),
        SettingGroup::new().title("更新").items([
            update_status_item(snapshot),
            SettingItem::new(
                "自动检查并下载更新",
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
            .description(
                "在后台检查新版本并下载，下载完成后在标题栏右上角提示，退出或重启时安装。\
                 检查时只发送 ShellRS 的版本号、系统和架构。",
            ),
        ]),
    ];
    if let Some((version, notes)) = notes {
        groups.push(
            SettingGroup::new()
                .title(format!("{version} 更新内容"))
                .items([SettingItem::render(move |_, _, _| {
                    div().w_full().child(
                        TextView::markdown(
                            SharedString::from(format!("about-notes-{version}")),
                            notes.clone(),
                        )
                        .selectable(true),
                    )
                })]),
        );
    }
    groups
}

/// 版本, 构建 and 平台, with where to read more.
fn version_item() -> SettingItem {
    SettingItem::render(|_, _, cx| {
        let channel = match crate::update::Channel::of_this_build() {
            Some(channel) => channel.key().to_string(),
            None => "开发构建".into(),
        };
        let version = format!("{}（{channel}）", build_info::VERSION);
        let build = match (build_info::COMMIT, build_info::BUILD_DATE) {
            (Some(commit), Some(date)) => {
                format!("{} · {date}", commit.get(..7).unwrap_or(commit))
            }
            (Some(commit), None) => commit.get(..7).unwrap_or(commit).to_string(),
            _ => "本机构建".into(),
        };
        let row = |label: &'static str, value: String, id: &'static str| {
            h_flex()
                .gap_3()
                .text_sm()
                .child(
                    div()
                        .w(rems(4.))
                        .flex_shrink_0()
                        .text_color(cx.theme().muted_foreground)
                        .child(label),
                )
                .child(
                    div()
                        .id(id)
                        .test_support()
                        .aria_label(value.clone())
                        .child(value),
                )
        };
        v_flex()
            .w_full()
            .gap_2()
            .child(row("版本", version, "about-version"))
            .child(row("构建", build, "about-build"))
            .child(row(
                "平台",
                platform::platform_label().to_string(),
                "about-platform",
            ))
            .child(
                h_flex()
                    .gap_4()
                    .text_sm()
                    .child(
                        Link::new("about-website")
                            .href("https://shellrs.com")
                            .child("官网"),
                    )
                    .child(
                        Link::new("about-changelog")
                            .href("https://shellrs.com/changelog")
                            .child("更新日志"),
                    ),
            )
    })
    .keywords(["版本", "关于", "ShellRS"])
}

/// Where updating stands, with 检查更新 and the step that applies.
fn update_status_item(snapshot: UpdateSnapshot) -> SettingItem {
    SettingItem::render(move |_, _, cx| {
        let (text, tone) = snapshot.status_line();
        let color = match tone {
            Tone::Plain => cx.theme().muted_foreground,
            Tone::Success => cx.theme().success,
            Tone::Warning => cx.theme().warning,
            Tone::Danger => cx.theme().danger,
        };
        let step: Option<(&'static str, &'static str, Box<dyn Action>)> =
            if snapshot.phase == Phase::Ready {
                Some(("show-update", "重启更新…", Box::new(ShowUpdate)))
            } else if snapshot.needs_download_page() {
                Some((
                    "open-download-page",
                    "前往下载页",
                    Box::new(OpenDownloadPage),
                ))
            } else if snapshot.can_download() {
                let label = if matches!(snapshot.phase, Phase::Failed { .. }) {
                    "重试下载"
                } else {
                    "下载"
                };
                Some(("download-update", label, Box::new(DownloadUpdate)))
            } else {
                None
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
                    .child(div().text_sm().child("状态："))
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
                    .gap_2()
                    .flex_shrink_0()
                    .when(snapshot.phase != Phase::Off, |buttons| {
                        buttons.child(
                            Button::new("check-for-updates")
                                .outline()
                                .small()
                                .icon(Icon::new(CatalogIcon::RefreshCw))
                                .label("检查更新")
                                .loading(snapshot.phase == Phase::Checking)
                                .disabled(!snapshot.can_check())
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(Box::new(CheckForUpdates), cx)
                                }),
                        )
                    })
                    .children(step.map(|(id, label, action)| {
                        Button::new(id).primary().small().label(label).on_click(
                            move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx),
                        )
                    })),
            )
    })
    .keywords(["更新", "升级", "版本", "下载"])
}

/// Where the `shellrs` command stands, with the one thing to do about it.
fn binary_item(bin_link: Option<String>, status: Option<IntegrationStatus>) -> SettingItem {
    SettingItem::render(move |_, _, cx| {
        let binary = status.as_ref().map(|status| &status.binary);
        let (line, action) = match (&bin_link, binary) {
            (None, _) => (
                RowStatus::Plain("此系统暂不支持安装 shellrs 命令".into()),
                None,
            ),
            (Some(_), None) => (RowStatus::Plain("正在检查…".into()), None),
            (Some(path), Some(BinaryStatus::Installed)) => {
                (RowStatus::Installed(path.clone()), Some(("移除", false)))
            }
            (Some(path), Some(BinaryStatus::Missing)) => (
                RowStatus::Plain(format!("未安装（{path}）")),
                Some(("安装", true)),
            ),
            (Some(path), Some(BinaryStatus::Stale { target })) => (
                RowStatus::Warning(format!(
                    "{path} 指向 {}，已失效或不是这个 ShellRS",
                    target.display()
                )),
                Some(("重新安装", true)),
            ),
            (Some(path), Some(BinaryStatus::Occupied { target })) => (
                RowStatus::Warning(match target {
                    Some(target) => {
                        format!("{path} 已被其他程序占用（指向 {}）", target.display())
                    }
                    None => format!("{path} 已被其他程序占用"),
                }),
                None,
            ),
            (Some(path), Some(BinaryStatus::Outdated)) => (
                RowStatus::Warning(format!("已安装旧版本（{path}）")),
                Some(("更新", true)),
            ),
            (Some(path), Some(BinaryStatus::NotOnPath)) => (
                RowStatus::Warning(format!("{path} 所在的文件夹不在 PATH 中")),
                Some(("安装", true)),
            ),
            (Some(_), Some(BinaryStatus::Unavailable)) => (
                RowStatus::Warning(
                    "找不到 shellrs-cli.exe：它应在 ShellRS 程序旁边（开发时请先 cargo build）"
                        .into(),
                ),
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
                    .child(div().text_sm().child("状态："))
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
    .keywords(["CLI", "PATH", "shellrs", "命令", "安装"])
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
                    .label("检查状态")
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
                    .label("复制 skills")
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(CopyAgentSkill), cx)),
            )
    })
    .keywords(["skill", "Agent", "检查", "复制"])
}

/// One row per agent, each with where its skill goes and whether it is
/// there.
fn skills_item(files: Option<[String; 4]>, status: Option<IntegrationStatus>) -> SettingItem {
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
    .keywords(["skill", "Agent", "Codex", "Claude", "OpenCode"])
}

fn skill_row(
    agent: AgentKind,
    file: Option<String>,
    status: Option<SkillStatus>,
    cx: &App,
) -> impl IntoElement {
    let (line, action) = match (&file, status) {
        (None, _) => (RowStatus::Plain("此系统暂不支持安装".into()), None),
        (Some(_), None) => (RowStatus::Plain("正在检查…".into()), None),
        (Some(file), Some(SkillStatus::Installed)) => {
            (RowStatus::Installed(file.clone()), Some(("移除", false)))
        }
        (Some(file), Some(SkillStatus::Missing)) => (
            RowStatus::Plain(format!("未安装（{file}）")),
            Some(("安装", true)),
        ),
        (Some(file), Some(SkillStatus::Outdated)) => (
            RowStatus::Warning(format!("已安装旧版本（{file}）")),
            Some(("更新", true)),
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
fn row_button(id: impl Into<ElementId>, label: &'static str, install: bool) -> Button {
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
                .aria_label(format!("已安装于 {path}"))
                .text_color(cx.theme().success)
                .child(
                    h_flex()
                        .gap_1()
                        .child(Icon::new(IconName::Check).small())
                        .child("已安装于")
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
