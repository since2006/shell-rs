use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable, Size, ThemeStyled as _, h_flex, tooltip::Tooltip,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::host::{ConnectionState, HostOs, ProtocolKind, mark_color};
use crate::i18n::t;

/// What a glyph is drawn in on a [`mark_color`] colour.
const MARK_INK: u32 = 0xFF_FFFF;

/// The mark that stands for a host: the operating system found on its host,
/// drawn on a badge in that project's own colour, or the first character of
/// the host name until a probe succeeds, as for a Telnet or serial host,
/// which nothing probes. Whatever has no colour of its own (the name's character, Apple's
/// mark) gets its address's.
///
/// Shared so the host tree and the start page show one identity for the
/// same host. The badge borrows `Avatar`'s treatment, a circle at the theme's
/// radius, so identity marks look alike wherever the product shows one.
#[derive(IntoElement)]
pub struct HostMark {
    id: ElementId,
    name: SharedString,
    address: SharedString,
    os: Option<HostOs>,
    protocol: ProtocolKind,
    /// The connection the mark stands for on a tab, when it has one.
    state: Option<ConnectionState>,
    size: Size,
    tooltip: bool,
}

impl HostMark {
    /// `address` picks the colour of a mark that has none of its own: the
    /// host's address, or a serial host's device.
    pub fn new(
        id: impl Into<ElementId>,
        name: impl Into<SharedString>,
        address: impl Into<SharedString>,
        os: Option<HostOs>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            address: address.into(),
            os,
            protocol: ProtocolKind::Ssh,
            state: None,
            size: Size::Medium,
            tooltip: true,
        }
    }

    /// The mark of a Telnet or serial host, whatever `os` says: the name's
    /// character, which the tooltip calls Telnet or a serial port.
    pub fn protocol(mut self, protocol: ProtocolKind) -> Self {
        self.protocol = protocol;
        self
    }

    /// The state of the connection a tab's mark stands for. Anything but
    /// connected draws the mark grey, glyph and all, and says so.
    pub fn connection_state(mut self, state: ConnectionState) -> Self {
        self.state = Some(state);
        self
    }

    /// No tooltip of its own, for a row that shows one for the whole row, as
    /// the host tree does, so two tooltips never stack.
    pub fn without_tooltip(mut self) -> Self {
        self.tooltip = false;
        self
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
        let icon = |icon: Icon| {
            if compact {
                icon.size_3()
            } else {
                icon.size_5()
            }
            .into_any_element()
        };
        let own: Hsla = rgb(mark_color(&self.address)).into();
        let white: Hsla = rgb(MARK_INK).into();
        let os = self.os.filter(|_| self.protocol.is_ssh());
        let (background, ink, glyph, description) = match os {
            Some(os) => {
                let (background, ink) = match (os.brand_color(), os.brand_foreground()) {
                    (Some(background), Some(ink)) => (rgb(background).into(), rgb(ink).into()),
                    // A monochrome mark (Apple's) on the address's colour,
                    // so no mark is ever grey.
                    _ => (own, white),
                };
                (
                    background,
                    ink,
                    icon(Icon::default().path(os.icon_path())),
                    SharedString::from(os.label()),
                )
            }
            // The name's first character, a Telnet or serial host's too: it
            // has no system to find, which is all the tooltip says otherwise.
            None => (
                own,
                white,
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
                match self.protocol {
                    ProtocolKind::Ssh => t!("shared.host_mark.unknown_os"),
                    ProtocolKind::Telnet => t!("shared.host_mark.telnet"),
                    ProtocolKind::Serial => t!("shared.host_mark.serial"),
                },
            ),
        };
        let offline = self.state.filter(|state| !state.is_connected());
        let (background, ink, description) = match offline {
            Some(state) => (
                cx.theme().muted,
                cx.theme().muted_foreground,
                t!(
                    "shared.host_mark.with_state",
                    mark = description,
                    state = state.label()
                ),
            ),
            None => (background, ink, description),
        };
        let tooltip = description.clone();
        h_flex()
            .id(self.id)
            .test_support()
            .aria_label(description)
            .when(self.tooltip, |badge| {
                badge.tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            })
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
