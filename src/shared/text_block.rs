//! Text read off a host, shown as it came: a service's journal, a
//! container's logs, its environment. Its lines can be selected and copied
//! like any other text (issue #164).

use gpui_kit::base::SelectableText;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

/// `text` in a monospace box. The caller sizes it and makes it scroll.
pub fn text_block(
    id: ElementId,
    text: SharedString,
    cx: &App,
) -> impl IntoElement + Styled + StatefulInteractiveElement + use<> {
    div()
        .id(id.clone())
        .test_support()
        .aria_label(text.clone())
        .p_3()
        .rounded(cx.theme().radius)
        .bg(cx.theme().muted)
        .font_family(cx.theme().mono_font_family.clone())
        .text_xs()
        .child(SelectableText::new((id, "text"), text).selection_color(cx.theme().selection))
}
