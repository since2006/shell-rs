//! A volume's, an image's or a network's details: the dialog a click on its
//! card in the Docker tool opens.

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, WindowExt as _, button::Button, dialog::DialogFooter,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::details::{Read, message, reading, render_sections, run};
use super::linux::{image_details, inspect_command, network_details, volume_details};
use super::model::{DetailSection, DockerObject, ObjectSummary, Tone};
use crate::app::{CatalogIcon, RemoveDockerObject};
use crate::i18n::t;
use crate::shared::soft_tag;
use crate::terminal::TerminalView;

/// As wide as a container's details: long IDs, digests and paths.
const DIALOG_WIDTH: f32 = 46.;

/// What a tag's tone looks like.
pub(super) fn tone_color(tone: Tone, cx: &App) -> Hsla {
    match tone {
        Tone::Good => cx.theme().success,
        Tone::Quiet => cx.theme().muted_foreground,
    }
}

/// What the lines of `object`'s details are found by: 「volume-basics:mountpoint」.
fn prefix(object: DockerObject) -> &'static str {
    match object {
        DockerObject::Container => "container",
        DockerObject::Image => "image",
        DockerObject::Volume => "volume",
        DockerObject::Network => "network",
    }
}

/// Open the details of a volume, an image or a network of the host of
/// `terminal`, whose connection reads them. `dispatch` is the workspace's
/// focus handle: the dialog's button closes it and dispatches there.
pub fn open_object_dialog(
    summary: ObjectSummary,
    terminal: WeakEntity<TerminalView>,
    dispatch: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let view = cx.new(|cx| ObjectDetailsView::new(summary.clone(), terminal, cx));
    window.open_dialog(cx, move |dialog, window, cx| {
        let title = v_flex()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().min_w_0().truncate().child(summary.name.clone()))
                    .when_some(summary.tag.clone(), |line, (tag, tone)| {
                        line.child(soft_tag(tag, tone_color(tone, cx)).flex_shrink_0())
                    }),
            )
            .child(
                div()
                    .truncate()
                    .text_sm()
                    .font_weight(FontWeight::NORMAL)
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{} · {}", summary.object.label(), summary.detail)),
            );
        let remove = {
            let dispatch = dispatch.clone();
            let action = RemoveDockerObject {
                object: summary.object,
                id: summary.id.clone(),
                name: summary.name.clone(),
            };
            Button::new("docker-dialog-remove")
                .outline()
                .icon(Icon::new(CatalogIcon::Trash))
                .label(t!("common.delete"))
                .when_some(summary.removable.clone().err(), |button, why| {
                    button.disabled(true).tooltip(why)
                })
                .on_click(move |_, window, cx| {
                    // One dialog at a time: the confirmation takes its place.
                    window.close_dialog(cx);
                    dispatch.dispatch_action(&action, window, cx);
                })
        };
        dialog
            .title(title)
            // Dialog geometry is an API boundary that takes `Pixels`; the
            // width follows the interface zoom through the rem.
            .w(rems(DIALOG_WIDTH).to_pixels(window.rem_size()))
            .max_h(window.viewport_size().height * 0.9)
            // Closed by its button or Escape, like every dialog.
            .overlay_closable(false)
            .child(view.clone())
            .footer(DialogFooter::new().justify_start().child(remove))
    });
}

/// The dialog's body: what `docker … inspect` says about it.
pub struct ObjectDetailsView {
    summary: ObjectSummary,
    details: Read<Vec<DetailSection>>,
    _task: Task<()>,
}

impl ObjectDetailsView {
    fn new(
        summary: ObjectSummary,
        terminal: WeakEntity<TerminalView>,
        cx: &mut Context<Self>,
    ) -> Self {
        let task = run(
            &terminal,
            inspect_command(summary.object, &summary.id),
            cx,
            |this: &mut Self, answer| {
                let summary = &this.summary;
                this.details = match answer {
                    Ok(output) => match match summary.object {
                        DockerObject::Volume => volume_details(&output, &summary.used_by),
                        DockerObject::Image => image_details(&output, &summary.used_by),
                        DockerObject::Network => network_details(&output),
                        DockerObject::Container => None,
                    } {
                        Some(sections) => Read::Known(sections),
                        None => Read::Unknown(gone(summary.object).into()),
                    },
                    Err(why) => Read::Unknown(why),
                };
            },
        );
        Self {
            summary,
            details: Read::Reading,
            _task: task,
        }
    }
}

impl Render for ObjectDetailsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.details {
            Read::Known(sections) => {
                render_sections(prefix(self.summary.object), sections, cx).into_any_element()
            }
            Read::Unknown(why) => message("docker-details-unknown", why.clone(), cx),
            Read::Reading | Read::NotYet => reading("docker-details-reading", cx),
        };
        div().id("docker-details").test_support().child(body)
    }
}

/// Why there are no details: it went since the list was read.
fn gone(object: DockerObject) -> SharedString {
    match object {
        DockerObject::Container => t!("docker.details.container_gone"),
        DockerObject::Image => t!("docker.details.image_gone"),
        DockerObject::Volume => t!("docker.details.volume_gone"),
        DockerObject::Network => t!("docker.details.network_gone"),
    }
}
