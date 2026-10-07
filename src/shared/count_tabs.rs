//! The tabs over a right-sidebar tool's list.

use gpui_kit::component::Sizable as _;
use gpui_kit::*;

use super::{Segment, SegmentedControl};

/// What a right-sidebar tool's list shows, as a small segmented control
/// across the panel, its segments sharing the width equally, each with how
/// many it holds beside its label: 「容器 9」. For short labels: Docker's.
/// Longer ones (系统服务's 「已停止 202」) are cut short in an equal share of
/// the panel's 320 px, and keep gpui-kit's underlined tabs, which scroll
/// sideways instead; so do a dialog's sections.
///
/// Not gpui-kit's `TabBar`: its tabs take their labels' width whatever
/// they are told (the segmented one wraps each in a box of its own for its
/// sliding marker), so they bunch up at the start of the bar. `id` names
/// the box around the control, whose segments are found in it by their
/// place: `within(id).click(1usize)`.
pub fn count_tabs(
    id: &'static str,
    tabs: impl IntoIterator<Item = (impl Into<SharedString>, usize)>,
    selected: usize,
    on_click: impl Fn(&usize, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div().id(id).test_support().child(
        SegmentedControl::new(SharedString::from(format!("{id}-control")))
            .small()
            .selected_index(Some(selected))
            .on_change(on_click)
            .segments(
                tabs.into_iter()
                    .map(|(label, count)| Segment::new(label).count(count)),
            ),
    )
}
