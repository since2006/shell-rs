//! The image in a preview: fitted into its frame and centered, or zoomed,
//! with scrolling once it is larger than the frame.
//!
//! The zoom is the preview's own state, like a list's search text: its
//! actions are handled here, where the keys reach it while the preview has
//! focus, not by the workspace, which never holds a dialog's content.

use std::{cell::Cell, rc::Rc, sync::Arc};

use gpui_kit::component::{
    ActiveTheme as _, Icon, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    scroll::ScrollableElement as _,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::format_size;
use crate::app::{
    ActualSizePreview, CatalogIcon, FitPreview, IMAGE_PREVIEW_CONTEXT, ZoomPreviewIn,
    ZoomPreviewOut,
};
use crate::i18n::t;

/// The zoom levels the buttons and keys step through, as a share of the
/// image's own size (one image pixel to one point at 100%).
const STEPS: [f32; 14] = [
    0.1, 0.25, 0.33, 0.5, 0.67, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0, 4.0, 5.0, 8.0,
];
const MIN_SCALE: f32 = 0.1;
const MAX_SCALE: f32 = 8.0;

/// How the image is sized: to fit the frame (never above its own size),
/// or at a scale of its own.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Zoom {
    Fit,
    Scale(f32),
}

/// The scale at which the whole image fits the frame; a small image keeps
/// its own size.
pub(super) fn fit_scale(frame: Size<Pixels>, image: Size<Pixels>) -> f32 {
    if image.width <= px(0.) || image.height <= px(0.) {
        return 1.0;
    }
    (frame.width / image.width)
        .min(frame.height / image.height)
        .min(1.0)
}

/// The next step above `scale`.
pub(super) fn zoom_in(scale: f32) -> f32 {
    STEPS
        .into_iter()
        .find(|step| *step > scale + 0.001)
        .unwrap_or(MAX_SCALE)
}

/// The next step below `scale`.
pub(super) fn zoom_out(scale: f32) -> f32 {
    STEPS
        .into_iter()
        .rev()
        .find(|step| *step < scale - 0.001)
        .unwrap_or(MIN_SCALE)
}

/// The area scrolled over: the image, or the frame where the image is
/// smaller, so that it stays in the middle.
pub(super) fn scroll_area(frame: Size<Pixels>, shown: Size<Pixels>) -> Size<Pixels> {
    size(shown.width.max(frame.width), shown.height.max(frame.height))
}

/// The scroll offset that keeps the point in the middle of the frame where
/// it was when the area changes size (zooming).
pub(super) fn recenter(
    frame: Size<Pixels>,
    old_area: Size<Pixels>,
    new_area: Size<Pixels>,
    offset: Point<Pixels>,
) -> Point<Pixels> {
    let axis = |offset: Pixels, frame: Pixels, old: Pixels, new: Pixels| {
        let center = (-offset + frame / 2.) / old;
        let start = new * center - frame / 2.;
        -start.clamp(px(0.), (new - frame).max(px(0.)))
    };
    point(
        axis(offset.x, frame.width, old_area.width, new_area.width),
        axis(offset.y, frame.height, old_area.height, new_area.height),
    )
}

/// 「37%」.
pub(super) fn percent(scale: f32) -> String {
    format!("{}%", (scale * 100.).round())
}

pub(super) struct ImagePreview {
    /// The decoded image: `None` while it is decoded, an error when it
    /// cannot be.
    decoded: Option<Result<Arc<RenderImage>, String>>,
    name: SharedString,
    /// The file's size in bytes.
    bytes: u64,
    zoom: Zoom,
    scroll: ScrollHandle,
    /// The frame's size as last laid out: the fit, the centering and the
    /// scroll area go by it.
    frame: Rc<Cell<Option<Size<Pixels>>>>,
    focus_handle: FocusHandle,
    _decode: Task<()>,
}

impl ImagePreview {
    /// The image is decoded on the background executor, here rather than
    /// by `img`'s loading, so that one that cannot be decoded says so
    /// instead of loading for ever.
    pub(super) fn new(
        image: Image,
        name: SharedString,
        bytes: u64,
        cx: &mut Context<Self>,
    ) -> Self {
        let renderer = cx.svg_renderer();
        let decode = cx.spawn(async move |this, cx| {
            let decoded = cx
                .background_spawn(async move { image.to_image_data(renderer) })
                .await
                .map_err(|error| format!("{error:#}"));
            let _ = this.update(cx, |this, cx| {
                this.decoded = Some(decoded);
                cx.notify();
            });
        });
        // The decoded image lives in the window's texture atlas until it is
        // dropped from there; the window is free again once the dialog that
        // released this view has closed.
        cx.on_release(|this, cx| {
            if let Some(Ok(image)) = this.decoded.take() {
                cx.defer(move |cx| cx.drop_image(image, None));
            }
        })
        .detach();
        Self {
            decoded: None,
            name,
            bytes,
            zoom: Zoom::Fit,
            scroll: ScrollHandle::new(),
            frame: Rc::new(Cell::new(None)),
            focus_handle: cx.focus_handle(),
            _decode: decode,
        }
    }

    pub(super) fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    /// The image's own size, one image pixel to one point; `None` until it
    /// is decoded, or when it cannot be.
    fn pixels(&self) -> Option<Size<Pixels>> {
        let Some(Ok(decoded)) = &self.decoded else {
            return None;
        };
        let pixels = decoded.size(0);
        Some(size(
            px(u32::from(pixels.width) as f32),
            px(u32::from(pixels.height) as f32),
        ))
    }

    fn scale(&self, frame: Size<Pixels>, image: Size<Pixels>) -> f32 {
        match self.zoom {
            Zoom::Fit => fit_scale(frame, image),
            Zoom::Scale(scale) => scale,
        }
    }

    /// Zoom, keeping what is in the middle of the frame there.
    fn set_zoom(&mut self, zoom: Zoom, cx: &mut Context<Self>) {
        if let (Some(image), Some(frame)) = (self.pixels(), self.frame.get()) {
            let old = self.scale(frame, image);
            self.zoom = zoom;
            let new = self.scale(frame, image);
            let area =
                |scale: f32| scroll_area(frame, size(image.width * scale, image.height * scale));
            self.scroll
                .set_offset(recenter(frame, area(old), area(new), self.scroll.offset()));
        } else {
            self.zoom = zoom;
        }
        cx.notify();
    }

    fn current_scale(&self) -> f32 {
        match (self.pixels(), self.frame.get()) {
            (Some(image), Some(frame)) => self.scale(frame, image),
            _ => 1.0,
        }
    }

    fn zoom_in(&mut self, _: &ZoomPreviewIn, _: &mut Window, cx: &mut Context<Self>) {
        let scale = zoom_in(self.current_scale());
        self.set_zoom(Zoom::Scale(scale), cx);
    }

    fn zoom_out(&mut self, _: &ZoomPreviewOut, _: &mut Window, cx: &mut Context<Self>) {
        let scale = zoom_out(self.current_scale());
        self.set_zoom(Zoom::Scale(scale), cx);
    }

    fn fit(&mut self, _: &FitPreview, _: &mut Window, cx: &mut Context<Self>) {
        self.set_zoom(Zoom::Fit, cx);
    }

    fn actual_size(&mut self, _: &ActualSizePreview, _: &mut Window, cx: &mut Context<Self>) {
        self.set_zoom(Zoom::Scale(1.0), cx);
    }
}

impl Render for ImagePreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let height = viewport.height * 0.7;
        let pixels = self.pixels();
        // Before the first layout, a guess at the frame: the next frame has
        // its measure.
        let frame = self
            .frame
            .get()
            .unwrap_or_else(|| size(viewport.width * 0.8 - px(48.), height));
        let scale = pixels.map(|image| self.scale(frame, image));
        let info: SharedString = match pixels {
            Some(image) => t!(
                "explorer.preview.image_info",
                width = f32::from(image.width) as u32,
                height = f32::from(image.height) as u32,
                size = format_size(self.bytes)
            ),
            None => format_size(self.bytes).into(),
        };
        // Only the scale: whether it fits is the 适合窗口 button's state.
        let zoom_label: SharedString = scale.map(percent).unwrap_or_default().into();
        let focus = self.focus_handle.clone();
        let command = move |action: Box<dyn Action>| {
            let focus = focus.clone();
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                focus.dispatch_action(action.as_ref(), window, cx)
            }
        };
        let toolbar = h_flex()
            .gap_1()
            .child(
                div()
                    .id("preview-info")
                    .test_support()
                    .aria_label(info.clone())
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(info),
            )
            .child(
                Button::new("preview-zoom-out")
                    .ghost()
                    .small()
                    .icon(Icon::new(CatalogIcon::ZoomOut))
                    .accessibility_label(t!("explorer.preview.zoom_out"))
                    .tooltip_with_action(
                        t!("explorer.preview.zoom_out"),
                        &ZoomPreviewOut,
                        Some(IMAGE_PREVIEW_CONTEXT),
                    )
                    .on_click(command(Box::new(ZoomPreviewOut))),
            )
            .child(
                div()
                    .id("preview-zoom")
                    .test_support()
                    .aria_label(zoom_label.clone())
                    .min_w_20()
                    .text_center()
                    .text_sm()
                    .font_family(cx.theme().mono_font_family.clone())
                    .child(zoom_label),
            )
            .child(
                Button::new("preview-zoom-in")
                    .ghost()
                    .small()
                    .icon(Icon::new(CatalogIcon::ZoomIn))
                    .accessibility_label(t!("explorer.preview.zoom_in"))
                    .tooltip_with_action(
                        t!("explorer.preview.zoom_in"),
                        &ZoomPreviewIn,
                        Some(IMAGE_PREVIEW_CONTEXT),
                    )
                    .on_click(command(Box::new(ZoomPreviewIn))),
            )
            .child(
                Button::new("preview-fit")
                    .ghost()
                    .small()
                    .label(t!("explorer.preview.fit"))
                    .selected(self.zoom == Zoom::Fit)
                    .tooltip_with_action(
                        t!("explorer.preview.fit"),
                        &FitPreview,
                        Some(IMAGE_PREVIEW_CONTEXT),
                    )
                    .on_click(command(Box::new(FitPreview))),
            )
            .child(
                Button::new("preview-actual-size")
                    .ghost()
                    .small()
                    .label(t!("explorer.preview.actual_size"))
                    .selected(self.zoom == Zoom::Scale(1.0))
                    .tooltip_with_action(
                        t!("explorer.preview.actual_size_tip"),
                        &ActualSizePreview,
                        Some(IMAGE_PREVIEW_CONTEXT),
                    )
                    .on_click(command(Box::new(ActualSizePreview))),
            );

        let measured = self.frame.clone();
        let this = cx.entity().downgrade();
        let decoded = match &self.decoded {
            Some(Ok(decoded)) => Some(decoded.clone()),
            _ => None,
        };
        let picture = pixels
            .zip(scale)
            .zip(decoded)
            .map(|((image, scale), decoded)| {
                let shown = size(image.width * scale, image.height * scale);
                let area = scroll_area(frame, shown);
                // The area is at least the frame, so a smaller image sits in its
                // middle; a larger one fills it and scrolls.
                div()
                    .w(area.width)
                    .h(area.height)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        img(ImageSource::Render(decoded))
                            .id("preview-picture")
                            .w(shown.width)
                            .h(shown.height)
                            .flex_none()
                            // A double click goes between fitting and 100%.
                            .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                                if event.click_count() == 2 {
                                    let zoom = if this.zoom == Zoom::Fit {
                                        Zoom::Scale(1.0)
                                    } else {
                                        Zoom::Fit
                                    };
                                    this.set_zoom(zoom, cx);
                                }
                            }))
                            .test_support(),
                    )
            });
        let frame_box = div()
            .id("preview-image")
            .test_support()
            .aria_label(self.name.clone())
            .relative()
            .h(height)
            .w_full()
            .rounded(cx.theme().radius)
            .bg(cx.theme().muted)
            .overflow_hidden()
            // The frame's size decides the fit; a new size takes effect on
            // the next frame.
            .child(
                canvas(
                    move |bounds, window, _| {
                        if measured.get() != Some(bounds.size) {
                            measured.set(Some(bounds.size));
                            if let Some(this) = this.upgrade() {
                                let id = this.entity_id();
                                window.on_next_frame(move |_, cx| cx.notify(id));
                            }
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .child(
                div()
                    .id("preview-scroll")
                    .size_full()
                    .overflow_scroll()
                    .track_scroll(&self.scroll)
                    // ⌘ (Ctrl) with the wheel or trackpad zooms instead of
                    // scrolling.
                    .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                        if !event.modifiers.secondary() {
                            return;
                        }
                        cx.stop_propagation();
                        let delta = event.delta.pixel_delta(px(20.)).y;
                        let scale = this.current_scale();
                        let scale =
                            (scale * (1. + f32::from(delta) / 200.)).clamp(MIN_SCALE, MAX_SCALE);
                        this.set_zoom(Zoom::Scale(scale), cx);
                    }))
                    .children(picture)
                    .when(pixels.is_none(), |scroll| {
                        let note = match &self.decoded {
                            Some(Err(_)) => t!("explorer.preview.undecodable"),
                            _ => t!("explorer.preview.decoding"),
                        };
                        scroll.child(
                            div()
                                .id("preview-note")
                                .test_support()
                                .aria_label(note.clone())
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(note),
                        )
                    }),
            )
            .vertical_scrollbar(&self.scroll)
            .horizontal_scrollbar(&self.scroll);

        v_flex()
            .id("image-preview")
            .key_context(IMAGE_PREVIEW_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::zoom_in))
            .on_action(cx.listener(Self::zoom_out))
            .on_action(cx.listener(Self::fit))
            .on_action(cx.listener(Self::actual_size))
            .gap_2()
            .child(toolbar)
            .child(frame_box)
    }
}

#[cfg(test)]
mod tests {
    use super::{fit_scale, percent, recenter, scroll_area, zoom_in, zoom_out};
    use gpui_kit::{point, px, size};

    #[test]
    fn a_large_image_fits_and_a_small_one_keeps_its_size() {
        let frame = size(px(800.), px(600.));
        assert_eq!(fit_scale(frame, size(px(4000.), px(1000.))), 0.2);
        assert_eq!(fit_scale(frame, size(px(100.), px(3000.))), 0.2);
        assert_eq!(fit_scale(frame, size(px(10.), px(10.))), 1.0);
        assert_eq!(fit_scale(frame, size(px(0.), px(0.))), 1.0);
    }

    #[test]
    fn zooming_steps_up_and_down_within_bounds() {
        assert_eq!(zoom_in(0.2), 0.25);
        assert_eq!(zoom_in(1.0), 1.25);
        assert_eq!(zoom_in(8.0), 8.0);
        assert_eq!(zoom_out(1.0), 0.75);
        assert_eq!(zoom_out(0.3), 0.25);
        assert_eq!(zoom_out(0.1), 0.1);
        assert_eq!(percent(0.333), "33%");
    }

    #[test]
    fn the_middle_stays_in_the_middle_when_zooming() {
        let frame = size(px(100.), px(100.));
        // A small image centered: nothing to scroll.
        let small = scroll_area(frame, size(px(50.), px(20.)));
        assert_eq!(small, frame);
        // From fitting to twice the frame: the middle of the area.
        let large = scroll_area(frame, size(px(200.), px(400.)));
        assert_eq!(
            recenter(frame, small, large, point(px(0.), px(0.))),
            point(px(-50.), px(-150.))
        );
        // Scrolled to the top, then zoomed out: the offset stays in range.
        assert_eq!(
            recenter(frame, large, small, point(px(-100.), px(-300.))),
            point(px(0.), px(0.))
        );
    }
}
