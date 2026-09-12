use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable, Size, ThemeStyled as _, h_flex, tooltip::Tooltip,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::session::HostOs;

/// The mark that stands for a session: the operating system found on its host,
/// drawn on a badge in that project's own colour, or the first character of
/// the session name until a probe succeeds.
///
/// Shared so the session tree and the start page show one identity for the
/// same host. The badge borrows `Avatar`'s treatment, a circle at the theme's
/// radius, so identity marks look alike wherever the product shows one.
#[derive(IntoElement)]
pub struct HostMark {
    id: ElementId,
    name: SharedString,
    os: Option<HostOs>,
    size: Size,
}

impl HostMark {
    pub fn new(
        id: impl Into<ElementId>,
        name: impl Into<SharedString>,
        os: Option<HostOs>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            os,
            size: Size::Medium,
        }
    }
}

impl Sizable for HostMark {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}

impl RenderOnce for HostMark {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let compact = matches!(self.size, Size::XSmall | Size::Small);
        let (background, ink, glyph, description) = match self.os {
            Some(os) => {
                let (background, ink) = match (os.brand_color(), os.brand_foreground()) {
                    (Some(background), Some(ink)) => (background.into(), ink.into()),
                    // A monochrome mark follows the theme, which is also how
                    // Apple's own guidance draws it.
                    _ => (cx.theme().foreground, cx.theme().background),
                };
                let glyph = Icon::default().path(os.icon_path());
                (
                    background,
                    ink,
                    if compact {
                        glyph.size_3()
                    } else {
                        glyph.size_5()
                    }
                    .into_any_element(),
                    SharedString::from(os.label()),
                )
            }
            None => (
                cx.theme().muted,
                cx.theme().muted_foreground,
                div()
                    .map(|text| {
                        if compact {
                            text.text_xs()
                        } else {
                            text.text_base()
                        }
                    })
                    .child(
                        self.name
                            .chars()
                            .next()
                            .map(|character| character.to_uppercase().to_string())
                            .unwrap_or_default(),
                    )
                    .into_any_element(),
                SharedString::from("未探测到系统"),
            ),
        };
        let tooltip = description.clone();
        h_flex()
            .id(self.id)
            .test_support()
            .aria_label(description)
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .flex_shrink_0()
            .map(|badge| {
                if compact {
                    badge.size_5()
                } else {
                    badge.size_8()
                }
            })
            .justify_center()
            .rounded_full_style(cx)
            .bg(background)
            .text_color(ink)
            .child(glyph)
    }
}
