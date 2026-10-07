//! 设置 › 关键字高亮 › 规则: the rules as a table edited in place. Every
//! change goes to the settings at once, so the preview above and the
//! terminals follow as the user types.

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState},
    h_flex,
    input::{Input, InputEvent, InputState},
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::CatalogIcon;
use crate::i18n::{UiLocale, t};
use crate::terminal::{HighlightColor, HighlightRule, PatternError};

use super::SettingsStore;

/// The colors the picker offers first, the examples' among them.
///
/// None may be a color of the picker's own palette (shadcn's stone, red,
/// orange, yellow, green, cyan, blue, purple and pink): gpui-kit keys each
/// swatch by its hex, so one color in both rows is one accessibility node
/// twice, which a debug build stops on while assistive technology (or a
/// window manager using it) is connected. Its default first row, the
/// theme's colors, is just such a duplicate.
const FEATURED_COLORS: [HighlightColor; 10] = [
    HighlightColor::RED,
    HighlightColor::from_rgb(0xf76b15),
    HighlightColor::AMBER,
    HighlightColor::from_rgb(0x2fa86b),
    HighlightColor::from_rgb(0x0fa3a3),
    HighlightColor::BLUE,
    HighlightColor::from_rgb(0x5b5bd6),
    HighlightColor::from_rgb(0xa855d6),
    HighlightColor::from_rgb(0xe0489c),
    HighlightColor::from_rgb(0x8a8f98),
];

/// The rule table. The rows' text fields and color pickers live here, so
/// they keep the caret and an open picker while the settings change under
/// them; the settings hold the rules themselves.
pub struct HighlightRulesEditor {
    store: Entity<SettingsStore>,
    rows: Vec<RuleRow>,
    /// The next row's id: a row keeps it, whatever its place, so a dragged
    /// row keeps its fields.
    next_id: usize,
    _store_changes: Subscription,
    _locale: Subscription,
}

struct RuleRow {
    id: usize,
    rule: HighlightRule,
    /// Why the pattern matches nothing, when it does not compile.
    error: Option<PatternError>,
    pattern: Entity<InputState>,
    note: Entity<InputState>,
    hex: Entity<InputState>,
    picker: Entity<ColorPickerState>,
    _subscriptions: [Subscription; 4],
}

/// A row on its way to another place.
#[derive(Clone)]
struct DraggedRule {
    id: usize,
    /// Its place when the drag began; nothing moves until it ends.
    ix: usize,
    pattern: SharedString,
}

impl Render for DraggedRule {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded(cx.theme().radius)
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .border_1()
            .border_color(cx.theme().border)
            .font_family(cx.theme().mono_font_family.clone())
            .text_sm()
            .child(self.pattern.clone())
    }
}

impl HighlightRulesEditor {
    pub fn new(store: Entity<SettingsStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // The rows' own edits come back as the same rules; anything else
        // (the settings file read anew) lays the rows out again.
        let store_changes = cx.observe_in(&store, window, |this, store, window, cx| {
            let rules = store.read(cx).settings().terminal_highlight.rules;
            if rules != this.rules() {
                this.rows = Vec::new();
                for rule in rules {
                    let row = this.new_row(rule, window, cx);
                    this.rows.push(row);
                }
                cx.notify();
            }
        });
        // The rows' inputs keep their placeholders; a new language gives
        // them again.
        let locale = cx.observe_global_in::<UiLocale>(window, |this, window, cx| {
            for row in &this.rows {
                row.pattern.update(cx, |input, cx| {
                    input.set_placeholder(t!("settings.highlight.pattern_placeholder"), window, cx)
                });
                row.note.update(cx, |input, cx| {
                    input.set_placeholder(t!("settings.highlight.note_placeholder"), window, cx)
                });
            }
        });
        let mut this = Self {
            store: store.clone(),
            rows: Vec::new(),
            next_id: 0,
            _store_changes: store_changes,
            _locale: locale,
        };
        for rule in store.read(cx).settings().terminal_highlight.rules {
            let row = this.new_row(rule, window, cx);
            this.rows.push(row);
        }
        this
    }

    fn rules(&self) -> Vec<HighlightRule> {
        self.rows.iter().map(|row| row.rule.clone()).collect()
    }

    fn new_row(
        &mut self,
        rule: HighlightRule,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> RuleRow {
        let id = self.next_id;
        self.next_id += 1;
        let pattern = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("settings.highlight.pattern_placeholder"))
                .default_value(rule.pattern.clone())
        });
        let note = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("settings.highlight.note_placeholder"))
                .default_value(rule.note.clone())
        });
        let hex = cx.new(|cx| InputState::new(window, cx).default_value(rule.color.to_hex()));
        let picker =
            cx.new(|cx| ColorPickerState::new(window, cx).default_value(rule.color.hsla()));
        let subscriptions = [
            cx.subscribe(&pattern, move |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let pattern = input.read(cx).value().to_string();
                    this.edit(id, |rule| rule.pattern = pattern, cx);
                }
            }),
            cx.subscribe(&note, move |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let note = input.read(cx).value().to_string();
                    this.edit(id, |rule| rule.note = note, cx);
                }
            }),
            // A color typed in whole takes effect; left half typed, the
            // field shows the rule's color again.
            cx.subscribe_in(
                &hex,
                window,
                move |this, input, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        if let Some(color) = HighlightColor::from_hex(&input.read(cx).value()) {
                            this.set_color(id, color, ColorSource::Hex, window, cx);
                        }
                    }
                    InputEvent::Blur => {
                        if let Some(row) = this.row(id) {
                            let hex = row.rule.color.to_hex();
                            input.update(cx, |input, cx| input.set_value(hex, window, cx));
                        }
                    }
                    _ => {}
                },
            ),
            cx.subscribe_in(
                &picker,
                window,
                move |this, _, event: &ColorPickerEvent, window, cx| {
                    if let ColorPickerEvent::Change(Some(color)) = event {
                        let color = HighlightColor::from_hsla(*color);
                        this.set_color(id, color, ColorSource::Picker, window, cx);
                    }
                },
            ),
        ];
        RuleRow {
            id,
            error: rule.compile().err(),
            rule,
            pattern,
            note,
            hex,
            picker,
            _subscriptions: subscriptions,
        }
    }

    fn row(&self, id: usize) -> Option<&RuleRow> {
        self.rows.iter().find(|row| row.id == id)
    }

    /// Change the rule of row `id` and write the rules.
    fn edit(&mut self, id: usize, change: impl FnOnce(&mut HighlightRule), cx: &mut Context<Self>) {
        let Some(row) = self.rows.iter_mut().find(|row| row.id == id) else {
            return;
        };
        change(&mut row.rule);
        row.error = row.rule.compile().err();
        self.write(cx);
    }

    /// Give row `id` `color`, and show it in whichever of its color field
    /// and picker did not set it.
    fn set_color(
        &mut self,
        id: usize,
        color: HighlightColor,
        source: ColorSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.row(id) else {
            return;
        };
        match source {
            ColorSource::Hex => row.picker.update(cx, |picker, cx| {
                picker.set_value(color.hsla(), window, cx);
            }),
            ColorSource::Picker => row.hex.update(cx, |hex, cx| {
                hex.set_value(color.to_hex(), window, cx);
            }),
        }
        self.edit(id, |rule| rule.color = color, cx);
    }

    fn write(&mut self, cx: &mut Context<Self>) {
        let rules = self.rules();
        self.store.update(cx, |store, cx| {
            store.update(|settings| settings.terminal_highlight.rules = rules, cx)
        });
        cx.notify();
    }

    /// A rule at the end, enabled and empty, its pattern taking the keyboard.
    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let row = self.new_row(HighlightRule::default(), window, cx);
        let pattern = row.pattern.clone();
        self.rows.push(row);
        self.write(cx);
        pattern.update(cx, |pattern, cx| pattern.focus(window, cx));
    }

    fn remove(&mut self, id: usize, cx: &mut Context<Self>) {
        self.rows.retain(|row| row.id != id);
        self.write(cx);
    }

    /// Put row `id` where row `target` is: after it when it came from
    /// above, before it when from below.
    fn move_row(&mut self, id: usize, target: usize, cx: &mut Context<Self>) {
        let from = self.rows.iter().position(|row| row.id == id);
        let to = self.rows.iter().position(|row| row.id == target);
        let (Some(from), Some(to)) = (from, to) else {
            return;
        };
        if from != to {
            let row = self.rows.remove(from);
            self.rows.insert(to, row);
            self.write(cx);
        }
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex().w_full().justify_end().child(
            Button::new("add-highlight-rule")
                .outline()
                .small()
                .icon(Icon::new(IconName::Plus))
                .label(t!("settings.highlight.add_rule"))
                .on_click(cx.listener(|this, _, window, cx| this.add(window, cx))),
        )
    }

    fn render_header(&self, cx: &App) -> impl IntoElement {
        h_flex()
            .w_full()
            .gap_3()
            .pb_2()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            // Over the drag handles.
            .child(div().w_4().flex_shrink_0())
            .child(
                div()
                    .w_8()
                    .flex_shrink_0()
                    .child(t!("settings.highlight.column_enabled")),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(t!("settings.highlight.column_pattern")),
            )
            .child(
                div()
                    .w_40()
                    .flex_shrink_0()
                    .child(t!("settings.highlight.column_note")),
            )
            .child(
                div()
                    .w_32()
                    .flex_shrink_0()
                    .child(t!("settings.highlight.column_color")),
            )
            .child(
                // Wide enough for "Notify" as well as 通知.
                div()
                    .w_12()
                    .flex_shrink_0()
                    .child(t!("settings.highlight.column_notify")),
            )
            .child(div().w_6().flex_shrink_0())
    }

    fn render_row(&self, ix: usize, row: &RuleRow, cx: &mut Context<Self>) -> impl IntoElement {
        let id = row.id;
        let mono = cx.theme().mono_font_family.clone();
        h_flex()
            .id(("highlight-rule", id))
            .test_support()
            .w_full()
            .gap_3()
            .py_2()
            .border_t_1()
            .border_color(cx.theme().border)
            // Where a dragged row lands: after this one when it came from
            // above, before it when from below.
            .drag_over::<DraggedRule>(move |style, dragged, _, cx| {
                if dragged.id == id {
                    style
                } else if dragged.ix < ix {
                    style.border_b_1().border_color(cx.theme().primary)
                } else {
                    style.border_color(cx.theme().primary)
                }
            })
            .on_drop(cx.listener(move |this, dragged: &DraggedRule, _, cx| {
                this.move_row(dragged.id, id, cx);
            }))
            // Only the handle starts a drag: elsewhere a press selects text.
            .child(
                div()
                    .id(("highlight-rule-handle", id))
                    .test_support()
                    .w_4()
                    .flex_shrink_0()
                    .cursor_grab()
                    .text_color(cx.theme().muted_foreground)
                    .tooltip(|window, cx| {
                        Tooltip::new(t!("settings.highlight.drag")).build(window, cx)
                    })
                    .child(Icon::new(CatalogIcon::GripVertical).small())
                    .on_drag(
                        DraggedRule {
                            id,
                            ix,
                            pattern: row.rule.pattern.clone().into(),
                        },
                        |dragged, _, _, cx| cx.new(|_| dragged.clone()),
                    ),
            )
            .child(
                div().w_8().flex_shrink_0().child(
                    Checkbox::new(("highlight-rule-enabled", id))
                        .checked(row.rule.enabled)
                        .small()
                        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                            let enabled = *checked;
                            this.edit(id, |rule| rule.enabled = enabled, cx);
                        })),
                ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        Input::new(&row.pattern)
                            .id(("highlight-rule-pattern", id))
                            .small()
                            .font_family(mono.clone()),
                    )
                    .when_some(row.error, |cell, error| {
                        let error = error.message();
                        cell.child(
                            div()
                                .id(("highlight-rule-error", id))
                                .test_support()
                                .aria_label(error.clone())
                                .text_xs()
                                .text_color(cx.theme().danger)
                                .child(error),
                        )
                    }),
            )
            .child(
                div().w_40().flex_shrink_0().child(
                    Input::new(&row.note)
                        .id(("highlight-rule-note", id))
                        .small(),
                ),
            )
            .child(
                h_flex()
                    .w_32()
                    .flex_shrink_0()
                    .gap_2()
                    .child(
                        div()
                            .id(("highlight-rule-picker", id))
                            .test_support()
                            .flex_shrink_0()
                            .child(
                                ColorPicker::new(&row.picker)
                                    .featured_colors(
                                        FEATURED_COLORS.iter().map(|color| color.hsla()).collect(),
                                    )
                                    .small()
                                    .accessibility_label(t!("settings.highlight.column_color")),
                            ),
                    )
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&row.hex)
                                .id(("highlight-rule-color", id))
                                .small()
                                .font_family(mono),
                        ),
                    ),
            )
            .child(
                div().w_12().flex_shrink_0().child(
                    Checkbox::new(("highlight-rule-notify", id))
                        .checked(row.rule.notify)
                        .small()
                        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                            let notify = *checked;
                            this.edit(id, |rule| rule.notify = notify, cx);
                        })),
                ),
            )
            .child(
                div().w_6().flex_shrink_0().child(
                    Button::new(("delete-highlight-rule", id))
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(CatalogIcon::Trash))
                        .tooltip(t!("common.delete"))
                        .on_click(cx.listener(move |this, _, _, cx| this.remove(id, cx))),
                ),
            )
    }
}

/// Which of a row's two color controls the user changed.
#[derive(Clone, Copy)]
enum ColorSource {
    Hex,
    Picker,
}

impl Render for HighlightRulesEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let table = if self.rows.is_empty() {
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(t!("settings.highlight.empty"))
                .into_any_element()
        } else {
            let rows: Vec<_> = self
                .rows
                .iter()
                .enumerate()
                .map(|(ix, row)| self.render_row(ix, row, cx).into_any_element())
                .collect();
            v_flex()
                .w_full()
                .child(self.render_header(cx))
                .children(rows)
                .into_any_element()
        };
        v_flex()
            .id("highlight-rules")
            .test_support()
            .w_full()
            .gap_3()
            .child(self.render_toolbar(cx))
            .child(table)
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::Colorize as _;

    use super::FEATURED_COLORS;

    #[test]
    fn featured_colors_are_each_one_swatch() {
        // The picker keys its swatches by hex.
        let mut hexes: Vec<_> = FEATURED_COLORS
            .iter()
            .map(|color| color.hsla().to_hex())
            .collect();
        hexes.sort();
        hexes.dedup();
        assert_eq!(hexes.len(), FEATURED_COLORS.len());
    }
}
