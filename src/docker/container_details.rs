//! A container's details: the dialog a click on it in the Docker tool
//! opens.

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::DialogFooter,
    h_flex,
    tab::{Tab, TabBar},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::details::{Read, message, reading, render_sections, run};
use super::docker_panel::state_color;
use super::linux::{inspect_command, logs_command, parse_details};
use super::model::{Container, ContainerCommand, ContainerDetails, ContainerSubject, DockerObject};
use crate::app::{CatalogIcon, ControlContainers, RemoveDockerObject};
use crate::i18n::t;
use crate::shared::soft_tag;
use crate::terminal::TerminalView;

/// How wide the dialog is: long IDs, image names and environment lines.
const DIALOG_WIDTH: f32 = 46.;

/// Open the details of a container of the host of `terminal`, whose
/// connection reads them. `dispatch` is the workspace's focus handle: the
/// dialog's buttons close it and dispatch there.
pub fn open_container_dialog(
    container: Container,
    terminal: WeakEntity<TerminalView>,
    dispatch: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let view = cx.new(|cx| ContainerDetailsView::new(container.clone(), terminal, cx));
    window.open_dialog(cx, move |dialog, window, cx| {
        let color = state_color(container.state, cx);
        let title = v_flex()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().truncate().child(container.name.clone()))
                    .child(soft_tag(container.state.label(), color)),
            )
            .child(
                div()
                    .truncate()
                    .text_sm()
                    .font_weight(FontWeight::NORMAL)
                    .text_color(cx.theme().muted_foreground)
                    .child(container.image.clone()),
            );
        let subject = ContainerSubject::Container(container.name.clone());
        let control = |command: ContainerCommand| {
            let (dispatch, subject, id) = (dispatch.clone(), subject.clone(), container.id.clone());
            let button = Button::new(SharedString::from(format!(
                "container-dialog-{}",
                command.verb()
            )))
            .icon(Icon::new(match command {
                ContainerCommand::Start => CatalogIcon::Play,
                ContainerCommand::Stop => CatalogIcon::Square,
                ContainerCommand::Restart => CatalogIcon::RotateCw,
            }))
            .label(command.label())
            .on_click(move |_, window, cx| {
                // One dialog at a time: a confirmation takes its place.
                window.close_dialog(cx);
                dispatch.dispatch_action(
                    &ControlContainers {
                        subject: subject.clone(),
                        ids: vec![id.clone()],
                        command,
                    },
                    window,
                    cx,
                );
            });
            match command {
                ContainerCommand::Stop => button.danger(),
                ContainerCommand::Start => button.primary(),
                ContainerCommand::Restart => button.outline(),
            }
        };
        let remove = {
            let dispatch = dispatch.clone();
            let action = RemoveDockerObject {
                object: DockerObject::Container,
                id: container.id.clone(),
                name: container.name.clone(),
            };
            Button::new("container-dialog-remove")
                .outline()
                .icon(Icon::new(CatalogIcon::Trash))
                .label(t!("common.delete"))
                .on_click(move |_, window, cx| {
                    window.close_dialog(cx);
                    dispatch.dispatch_action(&action, window, cx);
                })
        };
        let up = container.state.is_up();
        let footer = DialogFooter::new()
            .justify_start()
            .child(control(if up {
                ContainerCommand::Stop
            } else {
                ContainerCommand::Start
            }))
            .child(control(ContainerCommand::Restart))
            // Docker removes a running container only by force.
            .when(!up, |footer| footer.child(remove));
        dialog
            .title(title)
            // Dialog geometry is an API boundary that takes `Pixels`; the
            // width follows the interface zoom through the rem.
            .w(rems(DIALOG_WIDTH).to_pixels(window.rem_size()))
            .max_h(window.viewport_size().height * 0.9)
            // Closed by its buttons or Escape, like every dialog.
            .overlay_closable(false)
            .child(view.clone())
            .footer(footer)
    });
}

/// The dialog's body: what `docker inspect` says about the container, and
/// its output.
pub struct ContainerDetailsView {
    container: Container,
    terminal: WeakEntity<TerminalView>,
    details: Read<ContainerDetails>,
    output: Read<String>,
    /// 0 the details, 1 the output.
    tab: usize,
    output_scroll: ScrollHandle,
    _details_task: Task<()>,
    _output_task: Option<Task<()>>,
}

impl ContainerDetailsView {
    fn new(
        container: Container,
        terminal: WeakEntity<TerminalView>,
        cx: &mut Context<Self>,
    ) -> Self {
        let details = run(
            &terminal,
            inspect_command(DockerObject::Container, &container.id),
            cx,
            |this, answer| {
                this.details = match answer {
                    Ok(output) => match parse_details(&output) {
                        Some(details) => Read::Known(details),
                        None => Read::Unknown(t!("docker.details.container_gone").into()),
                    },
                    Err(why) => Read::Unknown(why),
                };
            },
        );
        Self {
            container,
            terminal,
            details: Read::Reading,
            output: Read::NotYet,
            tab: 0,
            output_scroll: ScrollHandle::new(),
            _details_task: details,
            _output_task: None,
        }
    }

    fn select(&mut self, tab: usize, cx: &mut Context<Self>) {
        self.tab = tab;
        if tab == 1 && matches!(self.output, Read::NotYet) {
            self.output = Read::Reading;
            self._output_task = Some(run(
                &self.terminal,
                logs_command(&self.container.id),
                cx,
                |this, answer| {
                    this.output = match answer {
                        Ok(output) if output.trim().is_empty() => {
                            Read::Unknown(t!("docker.details.no_output").into())
                        }
                        Ok(output) => Read::Known(output.trim_end().to_owned()),
                        Err(why) => Read::Unknown(why),
                    };
                    // The newest lines are the last.
                    this.output_scroll.scroll_to_bottom();
                },
            ));
        }
        cx.notify();
    }

    fn render_details(&self, cx: &App) -> AnyElement {
        let details = match &self.details {
            Read::Known(details) => details,
            Read::Unknown(why) => return message("container-details-unknown", why.clone(), cx),
            Read::Reading | Read::NotYet => return reading("container-details-reading", cx),
        };
        v_flex()
            .id("container-details")
            .test_support()
            .child(render_sections("container", &details.sections(), cx))
            .into_any_element()
    }

    fn render_output(&self, cx: &App) -> AnyElement {
        match &self.output {
            Read::Known(output) => div()
                .id("container-output")
                .test_support()
                .aria_label(output.clone())
                .h(rems(20.))
                .overflow_y_scroll()
                .track_scroll(&self.output_scroll)
                .p_3()
                .rounded(cx.theme().radius)
                .bg(cx.theme().muted)
                .font_family(cx.theme().mono_font_family.clone())
                .text_xs()
                .child(output.clone())
                .into_any_element(),
            Read::Unknown(why) => message("container-output-unknown", why.clone(), cx),
            Read::Reading | Read::NotYet => reading("container-output-reading", cx),
        }
    }
}

impl Render for ContainerDetailsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .child(
                div().id("container-tabs").test_support().child(
                    TabBar::new("container-tab-bar")
                        .underline()
                        .small()
                        .selected_index(self.tab)
                        .on_click(cx.listener(|this, index: &usize, _, cx| this.select(*index, cx)))
                        .child(
                            Tab::new()
                                .icon(Icon::new(IconName::Info))
                                .label(t!("docker.details.tab_details")),
                        )
                        .child(
                            Tab::new()
                                .icon(Icon::new(CatalogIcon::FileText))
                                .label(t!("docker.details.tab_logs")),
                        ),
                ),
            )
            .child(if self.tab == 0 {
                self.render_details(cx)
            } else {
                self.render_output(cx)
            })
    }
}
