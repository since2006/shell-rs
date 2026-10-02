//! A color's light tint, for the tags and buttons that carry a state: the
//! right sidebar's 「运行中」 tags and its stop and start buttons.

use gpui_kit::component::{Sizable as _, button::ButtonCustomVariant, tag::Tag};
use gpui_kit::*;

/// A tag in `color`'s tint: 「运行中」.
pub fn soft_tag(text: impl Into<SharedString>, color: Hsla) -> Tag {
    Tag::custom(color.opacity(0.12), color, color.opacity(0.3))
        .small()
        .rounded_full()
        .child(text.into())
}

/// A button in `color`'s tint: the stop button in red, start in green.
pub fn tinted(color: Hsla, cx: &App) -> ButtonCustomVariant {
    ButtonCustomVariant::new(cx)
        .color(color.opacity(0.1))
        .foreground(color)
        .hover(color.opacity(0.2))
        .active(color.opacity(0.25))
}
