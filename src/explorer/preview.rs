//! Previews of files an SFTP tab shows: images and Markdown, in a large
//! dialog in the manner of Quick Look. Nothing stays behind: closing the
//! dialog drops what was read.

use gpui_kit::component::{
    ActiveTheme as _, WindowExt as _, button::Button, dialog::DialogFooter, text::TextView, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::format_size;
use super::image_preview::ImagePreview;
use crate::app::{ExplorerAction, ExplorerDispatch as _};

/// The largest image a preview reads.
pub const IMAGE_LIMIT: u64 = 20 * 1024 * 1024;

/// How a file can be previewed, by its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewKind {
    Image(ImageFormat),
    Markdown,
}

impl PreviewKind {
    pub fn of(file_name: &str) -> Option<Self> {
        let (_, extension) = file_name.rsplit_once('.')?;
        Some(match extension.to_ascii_lowercase().as_str() {
            "png" => PreviewKind::Image(ImageFormat::Png),
            "jpg" | "jpeg" => PreviewKind::Image(ImageFormat::Jpeg),
            "gif" => PreviewKind::Image(ImageFormat::Gif),
            "webp" => PreviewKind::Image(ImageFormat::Webp),
            "svg" => PreviewKind::Image(ImageFormat::Svg),
            "bmp" => PreviewKind::Image(ImageFormat::Bmp),
            "ico" => PreviewKind::Image(ImageFormat::Ico),
            "tif" | "tiff" => PreviewKind::Image(ImageFormat::Tiff),
            "md" | "markdown" => PreviewKind::Markdown,
            _ => return None,
        })
    }

    /// Only previewed, never edited: a picture is not text. An SVG is text
    /// (XML), so it opens in the editor and is previewed on request, like
    /// Markdown.
    pub fn preview_only(&self) -> bool {
        matches!(self, PreviewKind::Image(format) if *format != ImageFormat::Svg)
    }
}

pub(super) enum PreviewContent {
    Image(Image),
    Markdown(SharedString),
}

/// What a preview dialog shows, and the commands at its foot.
pub(super) struct Preview {
    pub name: String,
    /// The file's size in bytes.
    pub size: u64,
    pub content: PreviewContent,
    /// 下载…, for a remote file.
    pub download: Option<ExplorerAction>,
    /// 编辑, for a Markdown file.
    pub edit: Option<ExplorerAction>,
    /// The workspace's focus handle, where those are dispatched.
    pub dispatch: FocusHandle,
}

/// Show a preview over the window: most of it, the image fitted (and
/// zoomable) or the Markdown laid out and scrollable. Esc closes it.
pub(super) fn open_preview_dialog(preview: Preview, window: &mut Window, cx: &mut App) {
    let Preview {
        name,
        size,
        content,
        download,
        edit,
        dispatch,
    } = preview;
    let focus = window.focused(cx);
    let name: SharedString = name.into();
    let body = match content {
        PreviewContent::Image(image) => {
            let view = cx.new(|cx| ImagePreview::new(image, name.clone(), size, cx));
            PreviewBody::Image(view)
        }
        PreviewContent::Markdown(text) => PreviewBody::Markdown(text),
    };
    let image = match &body {
        PreviewBody::Image(view) => Some(view.clone()),
        PreviewBody::Markdown(_) => None,
    };
    window.open_dialog(cx, move |dialog, window, cx| {
        let viewport = window.viewport_size();
        let height = viewport.height * 0.7;
        let content = match &body {
            PreviewBody::Image(view) => view.clone().into_any_element(),
            PreviewBody::Markdown(text) => {
                let info = format!("Markdown · {}", format_size(size));
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .id("preview-info")
                            .test_support()
                            .aria_label(info.clone())
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(info),
                    )
                    .child(
                        div()
                            .id("preview-markdown")
                            .test_support()
                            .aria_label(name.clone())
                            .h(height)
                            .w_full()
                            .child(
                                TextView::markdown("preview-markdown-text", text.clone())
                                    .scrollable(true)
                                    .selectable(true),
                            ),
                    )
                    .into_any_element()
            }
        };
        // A command at the foot closes the preview first: what it opens
        // (the download question, the editor) goes in front.
        let command = |id: &'static str, label: &'static str, action: ExplorerAction| {
            let dispatch = dispatch.clone();
            Button::new(id).label(label).on_click(move |_, window, cx| {
                window.close_dialog(cx);
                dispatch.dispatch_explorer_action(&action, window, cx);
            })
        };
        dialog
            .title(name.clone())
            .w(viewport.width * 0.8)
            .margin_top(viewport.height * 0.05)
            .overlay_closable(false)
            .child(
                div()
                    .id("file-preview")
                    .test_support()
                    .aria_label(name.clone())
                    .child(content),
            )
            .footer(
                DialogFooter::new()
                    .when_some(download.clone(), |footer, action| {
                        footer.child(command("preview-download", "下载…", action))
                    })
                    .when_some(edit.clone(), |footer, action| {
                        footer.child(command("preview-edit", "编辑", action))
                    })
                    .child(
                        Button::new("preview-close")
                            .label("关闭")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ),
            )
            .on_close({
                let focus = focus.clone();
                move |_, window, cx| {
                    if let Some(focus) = &focus {
                        window.focus(focus, cx);
                    }
                }
            })
    });
    // The zoom keys reach the image while it has focus. Only a focus taken
    // in this same update holds: the dialog took it just now.
    if let Some(view) = image {
        let focus = view.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
    }
}

/// What the dialog shows, made once: the image's view keeps its zoom and
/// scroll across frames.
enum PreviewBody {
    Image(Entity<ImagePreview>),
    Markdown(SharedString),
}

#[cfg(test)]
mod tests {
    use super::PreviewKind;
    use gpui_kit::ImageFormat;

    #[test]
    fn images_and_markdown_are_previewed_by_their_names() {
        assert_eq!(
            PreviewKind::of("photo.JPG"),
            Some(PreviewKind::Image(ImageFormat::Jpeg))
        );
        assert_eq!(
            PreviewKind::of("logo.svg"),
            Some(PreviewKind::Image(ImageFormat::Svg))
        );
        assert_eq!(PreviewKind::of("README.md"), Some(PreviewKind::Markdown));
        assert_eq!(
            PreviewKind::of("notes.markdown"),
            Some(PreviewKind::Markdown)
        );
        assert_eq!(PreviewKind::of("archive.tar.gz"), None);
        assert_eq!(PreviewKind::of("Makefile"), None);
        assert_eq!(PreviewKind::of("nginx.conf"), None);
        assert!(PreviewKind::of("a.png").unwrap().preview_only());
        assert!(!PreviewKind::of("a.svg").unwrap().preview_only());
        assert!(!PreviewKind::Markdown.preview_only());
    }
}
