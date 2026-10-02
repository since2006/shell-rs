use std::rc::Rc;

use gpui_kit::base::{Radio, RadioGroup};
use gpui_kit::component::{ActiveTheme as _, Sizable, Size, tooltip::Tooltip};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

type ChangeHandler = Rc<dyn Fn(&usize, &mut Window, &mut App)>;

/// One choice out of a few, side by side in one track, the chosen one
/// standing out as a raised block: a segmented control, for a form's choice
/// between ways of doing the same thing (密码 / 使用凭据 / 无密码), and, small
/// and with counts, for what a right-sidebar tool's list shows (容器 9 / 卷
/// 0 …, see `count_tabs`).
///
/// Behavior comes from gpui-base's radio group: activation by pointer, Enter
/// or Space, a tab stop per segment, and the radio group's semantics for
/// assistive technology. This owns only the look. Each segment's element id
/// is its position, as with gpui-component's `RadioGroup`, so tests reach
/// one with `within(group).click(1usize)`.
#[derive(IntoElement)]
pub struct SegmentedControl {
    id: ElementId,
    segments: Vec<Segment>,
    selected: Option<usize>,
    on_change: Option<ChangeHandler>,
    size: Size,
}

/// One segment of a [`SegmentedControl`].
pub struct Segment {
    label: SharedString,
    /// How many there are of what it shows, quieter beside the label.
    count: Option<usize>,
    disabled: bool,
    /// Why a disabled segment cannot be chosen.
    tooltip: Option<SharedString>,
}

impl Segment {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            count: None,
            disabled: false,
            tooltip: None,
        }
    }

    /// How many there are of what it shows: 「容器 9」.
    pub fn count(mut self, count: usize) -> Self {
        self.count = Some(count);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }
}

impl FluentBuilder for Segment {}

impl SegmentedControl {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            segments: Vec::new(),
            selected: None,
            on_change: None,
            size: Size::Medium,
        }
    }

    pub fn selected_index(mut self, index: Option<usize>) -> Self {
        self.selected = index;
        self
    }

    /// Handle a request to choose the segment at an index. The value is
    /// controlled: the owner stores it and renders it back.
    pub fn on_change(mut self, handler: impl Fn(&usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }

    pub fn segments(mut self, segments: impl IntoIterator<Item = Segment>) -> Self {
        self.segments.extend(segments);
        self
    }
}

/// Medium, as tall as a form's other controls; small (or less) as a
/// right-sidebar tool's tabs.
impl Sizable for SegmentedControl {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}

impl RenderOnce for SegmentedControl {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        // The chosen block sits this far inside the track, its corners
        // rounded on the same centre as the track's.
        let inset = rems(0.125);
        let inner_radius = (theme.radius - inset.to_pixels(window.rem_size())).max(px(0.));
        // The chosen block's edge, a step stronger than the track's: its
        // fill is barely darker than the track, so the edge carries it. Mixed
        // from the theme rather than picked, so it is a step darker on a light
        // theme and a step lighter on a dark one. 8% of the text colour takes
        // the default light border from neutral-200 to about neutral-300.
        let edge = theme.border.blend(theme.foreground.opacity(0.08));
        let total = self.segments.len();
        let selected = self.selected;
        let on_change = self.on_change;
        let muted = theme.muted_foreground;

        RadioGroup::new(self.id)
            .axis(Axis::Horizontal)
            .flex()
            .w_full()
            .map(|track| match self.size {
                Size::XSmall | Size::Small => track.h_7(),
                _ => track.h_8(),
            })
            .p(inset)
            .gap(inset)
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .children(self.segments.into_iter().enumerate().map(|(ix, segment)| {
                let checked = selected == Some(ix);
                let hoverable = !checked && !segment.disabled;
                Radio::new(ix)
                    .checked(checked)
                    .disabled(segment.disabled)
                    .accessibility_label(match segment.count {
                        Some(count) => format!("{} {count}", segment.label).into(),
                        None => segment.label.clone(),
                    })
                    .set_position(ix + 1, total)
                    // Equal shares of the track, whatever the labels.
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px_2()
                    .rounded(inner_radius)
                    // Every segment has the border, so choosing one
                    // moves nothing.
                    .border_1()
                    .border_color(theme.transparent)
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .styles(|styles| {
                        styles
                            .checked(|style| {
                                style
                                    .bg(theme.accent)
                                    .border_color(edge)
                                    .text_color(theme.foreground)
                                    .shadow_xs()
                            })
                            .disabled(|style| style.opacity(0.5))
                    })
                    // Half of what choosing it would show.
                    .when(hoverable, |radio| {
                        radio.hover(|style| {
                            style
                                .bg(theme.accent.opacity(0.5))
                                .text_color(theme.foreground)
                        })
                    })
                    .focus_visible(|style| style.border_color(theme.ring))
                    // A click chooses without taking the keyboard from
                    // where it was, as gpui-component's radios do.
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .when_some(on_change.clone(), |radio, on_change| {
                        radio.on_change(move |_, _, window, cx| on_change(&ix, window, cx))
                    })
                    .when_some(segment.tooltip, |radio, tooltip| {
                        radio.tooltip(move |window, cx| {
                            Tooltip::new(tooltip.clone()).build(window, cx)
                        })
                    })
                    .child(div().truncate().child(segment.label))
                    .when_some(segment.count, |radio, count| {
                        radio.child(
                            div()
                                .flex_shrink_0()
                                .ml_1()
                                .text_color(muted)
                                .child(count.to_string()),
                        )
                    })
            }))
    }
}
