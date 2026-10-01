use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use crate::connection::{Latency, LatencyLevel};

/// A connection's latest round trip, beside a tab's buttons: green below
/// 100 ms, amber up to 200 ms, red beyond or when the ping timed out.
///
/// Shared so a host's terminal tabs and SFTP tabs read the same way.
#[derive(IntoElement)]
pub struct LatencyLabel {
    id: ElementId,
    latency: Latency,
}

impl LatencyLabel {
    pub fn new(id: impl Into<ElementId>, latency: Latency) -> Self {
        Self {
            id: id.into(),
            latency,
        }
    }
}

impl RenderOnce for LatencyLabel {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = match self.latency.level() {
            LatencyLevel::Good => cx.theme().success,
            LatencyLevel::Fair => cx.theme().warning,
            LatencyLevel::Poor => cx.theme().danger,
        };
        let label = self.latency.label();
        div()
            .id(self.id)
            .test_support()
            .aria_label(label.clone())
            // Keeps the buttons beside it still as the digits change.
            .min_w_12()
            .text_xs()
            .font_weight(FontWeight::BOLD)
            .text_right()
            .text_color(color)
            .child(label)
    }
}
