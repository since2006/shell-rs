//! The dialog of a 关键字高亮 rule: a new one, or one to change.

use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, WindowExt as _,
    checkbox::Checkbox,
    form::{Field, Form},
    input::{Input, InputEvent, InputState},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::shared::{
    Segment, SegmentedControl, commit_footer, dismiss_form_error, form_error_notification,
};
use crate::terminal::{HighlightColor, HighlightRule, PatternKind};

use super::SettingsStore;

/// How wide the dialog is: six colors side by side, and a pattern of a fair
/// length on one line.
const DIALOG_WIDTH: f32 = 32.;

/// The ways a pattern reads, in the order the dialog offers them.
const KINDS: [(PatternKind, &str); 2] = [
    (PatternKind::Keyword, "关键字"),
    (PatternKind::Regex, "正则表达式"),
];

/// The body of the dialog. Checks the pattern on commit; the settings are
/// only written when it compiles.
pub struct HighlightForm {
    store: Entity<SettingsStore>,
    /// The position of the rule being changed, and the rule as it was.
    editing: Option<(usize, HighlightRule)>,
    pattern: Entity<InputState>,
    kind: PatternKind,
    color: HighlightColor,
    bold: bool,
    notify: bool,
    _pattern_changes: Subscription,
}

impl HighlightForm {
    fn new(
        editing: Option<usize>,
        store: Entity<SettingsStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editing = editing.and_then(|ix| {
            let rule = store
                .read(cx)
                .settings()
                .terminal_highlight
                .rules
                .get(ix)?
                .clone();
            Some((ix, rule))
        });
        let rule = editing
            .as_ref()
            .map(|(_, rule)| rule.clone())
            .unwrap_or_default();
        let pattern = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("例如 ERROR")
                .default_value(rule.pattern.clone())
        });
        // The preview shows what is typed.
        let pattern_changes = cx.subscribe(&pattern, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        Self {
            store,
            editing,
            pattern,
            kind: rule.kind,
            color: rule.color,
            bold: rule.bold,
            notify: rule.notify,
            _pattern_changes: pattern_changes,
        }
    }

    fn rule(&self, cx: &App) -> HighlightRule {
        HighlightRule {
            pattern: self.pattern.read(cx).value().to_string(),
            kind: self.kind,
            color: self.color,
            bold: self.bold,
            notify: self.notify,
        }
    }

    /// Check the pattern and write the rule. Returns whether the dialog may
    /// close.
    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let rule = self.rule(cx);
        if let Err(error) = rule.compile() {
            window.push_notification(form_error_notification(error), cx);
            return false;
        }
        let editing = self.editing.clone();
        self.store.update(cx, |store, cx| {
            store.update(
                |settings| {
                    let rules = &mut settings.terminal_highlight.rules;
                    match editing {
                        // Only over the rule the dialog opened on.
                        Some((ix, original)) if rules.get(ix) == Some(&original) => {
                            rules[ix] = rule;
                        }
                        _ => rules.push(rule),
                    }
                },
                cx,
            )
        });
        true
    }
}

impl Render for HighlightForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let description = match self.kind {
            PatternKind::Keyword => "按字面匹配。含大写字母时区分大小写，全小写时不区分。",
            PatternKind::Regex => "区分大小写，开头加 (?i) 不区分。",
        };
        let pattern = self.pattern.read(cx).value();
        let sample = if pattern.is_empty() {
            "示例文字".into()
        } else {
            pattern
        };
        Form::new()
            .child(
                Field::new().label("匹配方式").child(
                    SegmentedControl::new("highlight-kind")
                        .selected_index(KINDS.iter().position(|(kind, _)| *kind == self.kind))
                        .on_change(cx.listener(|this, ix: &usize, _, cx| {
                            if let Some((kind, _)) = KINDS.get(*ix) {
                                this.kind = *kind;
                                cx.notify();
                            }
                        }))
                        .segments(KINDS.map(|(_, label)| Segment::new(label))),
                ),
            )
            .child(
                Field::new()
                    .label("内容")
                    .required(true)
                    .description(description)
                    .child(
                        Input::new(&self.pattern)
                            .id("highlight-pattern")
                            .small()
                            .font_family(cx.theme().mono_font_family.clone()),
                    ),
            )
            .child(
                Field::new().label("颜色").child(
                    SegmentedControl::new("highlight-color")
                        .selected_index(
                            HighlightColor::ALL
                                .iter()
                                .position(|color| *color == self.color),
                        )
                        .on_change(cx.listener(|this, ix: &usize, _, cx| {
                            if let Some(color) = HighlightColor::ALL.get(*ix) {
                                this.color = *color;
                                cx.notify();
                            }
                        }))
                        .segments(
                            HighlightColor::ALL
                                .map(|color| Segment::new(color.label()).swatch(color.hsla(cx))),
                        ),
                ),
            )
            .child(
                Field::new().child(
                    Checkbox::new("highlight-bold")
                        .label("加粗")
                        .checked(self.bold)
                        .small()
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.bold = *checked;
                            cx.notify();
                        })),
                ),
            )
            .child(
                Field::new().label("预览").child(
                    // On the terminal's background, in its font.
                    div()
                        .id("highlight-preview")
                        .test_support()
                        .w_full()
                        .px_2()
                        .py_1()
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().background)
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_sm()
                        .truncate()
                        .text_color(self.color.hsla(cx))
                        .when(self.bold, |preview| preview.font_weight(FontWeight::BOLD))
                        .child(sample),
                ),
            )
            .child(
                Field::new().child(
                    Checkbox::new("highlight-notify")
                        .label("匹配时通知")
                        .checked(self.notify)
                        .small()
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.notify = *checked;
                            cx.notify();
                        }))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                    "看不到这个终端时提醒：ShellRS 在后台，或终端在别的标签。\
                                     只检查换了行的新输出，全屏程序里不提醒；\
                                     同一终端 10 秒内只提醒一次。",
                                ),
                        ),
                ),
            )
    }
}

/// Open the dialog for a new rule, or for the rule at `editing`.
pub fn open_highlight_dialog(
    editing: Option<usize>,
    store: Entity<SettingsStore>,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| HighlightForm::new(editing, store, window, cx));
    let (title, commit): (SharedString, SharedString) = if form.read(cx).editing.is_some() {
        ("编辑高亮规则".into(), "保存".into())
    } else {
        ("添加高亮规则".into(), "添加".into())
    };
    window.open_dialog(cx, {
        let form = form.clone();
        move |dialog, window, _| {
            dialog
                .title(title.clone())
                // Dialog geometry is an API boundary that takes `Pixels`; the
                // width follows the interface zoom through the rem.
                .w(rems(DIALOG_WIDTH).to_pixels(window.rem_size()))
                // Closed by its buttons or Escape, not by a click beside it.
                .overlay_closable(false)
                .child(form.clone())
                .footer(commit_footer("commit", commit.clone()))
                .on_ok({
                    let form = form.clone();
                    move |_, window, cx| form.update(cx, |form, cx| form.commit(window, cx))
                })
                .on_close(|_, window, cx| dismiss_form_error(window, cx))
        }
    });
    // Straight to typing: in the same update as the opening, or the
    // dialog's own focus wins.
    let pattern = form.read(cx).pattern.clone();
    pattern.update(cx, |pattern, cx| pattern.focus(window, cx));
}
