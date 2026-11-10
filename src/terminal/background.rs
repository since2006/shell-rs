//! 背景图: the picture behind every terminal's text, under a veil of the
//! theme's background color.

use std::path::Path;
use std::sync::Arc;

use gpui_kit::*;

/// The picture terminals show now, which `settings::apply` sets for the
/// app's appearance: none while the user shows none.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TerminalBackground(Option<BackgroundImage>);

impl Global for TerminalBackground {}

/// A picture and how much of it shows through the veil.
#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundImage {
    image: Arc<Path>,
    /// From 0, the theme's color alone, to 1, the picture as it is.
    opacity: f32,
}

impl BackgroundImage {
    pub fn new(image: impl Into<Arc<Path>>, opacity: f32) -> Self {
        Self {
            image: image.into(),
            opacity: opacity.clamp(0., 1.),
        }
    }

    pub fn image(&self) -> &Arc<Path> {
        &self.image
    }

    pub fn opacity(&self) -> f32 {
        self.opacity
    }

    /// The veil over the picture: `background`, as opaque as the picture
    /// is not.
    pub fn veil(&self, background: Hsla) -> Hsla {
        background.opacity(1. - self.opacity)
    }
}

impl TerminalBackground {
    pub fn new(image: Option<BackgroundImage>) -> Self {
        Self(image)
    }

    /// The picture in effect; before the settings set one, none.
    pub fn current(cx: &App) -> Option<&BackgroundImage> {
        cx.try_global::<Self>()?.0.as_ref()
    }

    /// The picture covering its parent, then the veil of `color` over it:
    /// children laid first, under what the parent draws. The parent's own
    /// color shows until the picture loads, and if it cannot.
    pub fn layers(background: &BackgroundImage, color: Hsla) -> [AnyElement; 2] {
        [
            img(background.image.clone())
                .absolute()
                .inset_0()
                .size_full()
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
            div()
                .absolute()
                .inset_0()
                .bg(background.veil(color))
                .into_any_element(),
        ]
    }
}
