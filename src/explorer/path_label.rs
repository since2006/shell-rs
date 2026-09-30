//! The path label above each file list, after WinSCP's: the current path with
//! each directory clickable and the stretch from the root to the hovered
//! directory highlighted. The current directory itself, and a double-click
//! beside the path, open 打开目录/书签.

use super::{FilePane, PaneSide, pane_menu::path_menu, path_ancestors};
use crate::app::ExplorerCommand;
use gpui_kit::component::{ActiveTheme as _, h_flex, menu::ContextMenuExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::ops::Range;

/// One clickable part of the path label: its text with the trailing
/// separator, as WinSCP shows `/home/tester/`, and the directory it opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathPart {
    pub label: String,
    pub path: String,
}

pub fn path_parts(path: &str, remote: bool) -> Vec<PathPart> {
    let separator = separator(remote);
    path_ancestors(path, remote)
        .into_iter()
        .enumerate()
        .map(|(ix, (title, path))| PathPart {
            // The root's title is the root itself, separator included.
            label: if ix == 0 {
                title
            } else {
                format!("{title}{separator}")
            },
            path,
        })
        .collect()
}

fn separator(remote: bool) -> char {
    if remote {
        '/'
    } else {
        std::path::MAIN_SEPARATOR
    }
}

/// The parts to fold into `…` so the label fits `available`, as WinSCP
/// shortens it: the root stays, then `…`, then as many of the last parts as
/// fit. Empty when everything fits. When even the last part alone does not,
/// everything between the root and it folds and the last part is cut short.
pub fn folded_parts(widths: &[f32], ellipsis: f32, available: f32) -> Range<usize> {
    if widths.len() < 3 || widths.iter().sum::<f32>() <= available {
        return 0..0;
    }
    let mut start = widths.len() - 1;
    let mut used = widths[0] + ellipsis + widths[start];
    while start > 2 && used + widths[start - 1] <= available {
        start -= 1;
        used += widths[start];
    }
    1..start
}

impl FilePane {
    pub(super) fn render_path_label(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let remote = self.side == PaneSide::Remote;
        let theme = cx.theme();
        // Both labels look alike: an underline for the current pane read as
        // "this side is active" and fell behind where commands went.
        let background = theme.list_active;
        let border = theme.border;
        let bar = h_flex()
            .id(self.side.path_id())
            .test_support()
            .aria_label(format!("{}路径", self.side.label()))
            // WinSCP's current pane, which takes focus back when the tab
            // is shown again; nothing marks it on screen.
            .aria_selected(self.current)
            // The directory shown, once its listing is in.
            .when(!self.loading, |this| this.aria_value(self.path.clone()))
            .px_2()
            .py_1()
            .bg(background)
            .border_b_1()
            .border_color(border);

        let enabled = self.takes_commands();
        let parts = self.shown_parts(window);
        let hot = self
            .hovered_part
            .as_ref()
            .and_then(|path| parts.iter().position(|part| &part.path == path));
        let last = parts.len().saturating_sub(1);
        let (foreground, muted) = (theme.foreground, theme.muted_foreground);
        let width = self.label_width.clone();
        let pane = cx.entity_id();
        let row = h_flex()
            .id("path-parts")
            .test_support()
            .relative()
            .flex_1()
            .min_w_0()
            .h_6()
            .overflow_hidden()
            .text_sm()
            .children(parts.into_iter().enumerate().map(|(ix, part)| {
                let current = ix == last;
                // Clicking the current directory opens 打开目录/书签, as in
                // WinSCP.
                let command = if current {
                    ExplorerCommand::OpenDirectory { remote }
                } else {
                    ExplorerCommand::Navigate {
                        remote,
                        path: part.path.clone(),
                    }
                };
                let strong = enabled && (current || hot.is_some_and(|hot| ix <= hot));
                div()
                    .id(SharedString::from(format!("path:{}", part.path)))
                    .test_support()
                    .role(Role::Link)
                    .text_color(if strong { foreground } else { muted })
                    .map(|this| {
                        if current {
                            this.min_w_0().truncate()
                        } else {
                            this.flex_shrink_0()
                        }
                    })
                    .when(enabled, |this| {
                        let path = part.path.clone();
                        this.cursor_pointer()
                            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                                this.hover_part(&path, *hovered, cx)
                            }))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.dispatch_command(command.clone(), window, cx);
                            }))
                    })
                    .child(part.label)
            }))
            // So does double-clicking beside the path.
            .when(enabled, |this| {
                this.on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    if event.click_count() == 2 {
                        this.dispatch_command(
                            ExplorerCommand::OpenDirectory { remote },
                            window,
                            cx,
                        );
                    }
                }))
            })
            // How much room the parts had last frame decides what folds;
            // a new width takes effect on the next frame.
            .child(
                canvas(
                    move |bounds, window, _| {
                        if width.get() != Some(bounds.size.width) {
                            width.set(Some(bounds.size.width));
                            window.on_next_frame(move |_, cx| cx.notify(pane));
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            );
        let bar = bar.child(row);
        if !enabled {
            return bar.into_any_element();
        }
        let pane = cx.entity().downgrade();
        bar.context_menu(move |menu, window, cx| {
            let Some(state) = pane.upgrade().map(|pane| pane.read(cx).menu_state(cx)) else {
                return menu;
            };
            path_menu(menu, &state, window, cx)
        })
        .into_any_element()
    }

    /// The parts to show, with the middle folded into `…` when the label
    /// was too narrow for all of them last frame.
    fn shown_parts(&self, window: &mut Window) -> Vec<PathPart> {
        let remote = self.side == PaneSide::Remote;
        let parts = path_parts(&self.path, remote);
        let Some(available) = self.label_width.get() else {
            return parts;
        };
        if parts.len() < 3 {
            return parts;
        }
        // Measured as rendered: `text_sm`, parts side by side.
        let font = window.text_style().font();
        let font_size = rems(0.875).to_pixels(window.rem_size());
        let measure = |text: &str| {
            let run = TextRun {
                len: text.len(),
                font: font.clone(),
                color: Hsla::default(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let line =
                window
                    .text_system()
                    .shape_line(text.to_string().into(), font_size, &[run], None);
            f32::from(line.width)
        };
        let widths: Vec<f32> = parts.iter().map(|part| measure(&part.label)).collect();
        let ellipsis = format!("…{}", separator(remote));
        // A pixel of slack for rounding between measuring and layout.
        let folded = folded_parts(&widths, measure(&ellipsis), f32::from(available) - 1.);
        if folded.is_empty() {
            return parts;
        }
        let mut shown = vec![parts[0].clone()];
        // `…` opens the last directory it hides.
        shown.push(PathPart {
            label: ellipsis,
            path: parts[folded.end - 1].path.clone(),
        });
        shown.extend(parts[folded.end..].iter().cloned());
        shown
    }
}

#[cfg(test)]
mod tests {
    use super::{PathPart, folded_parts, path_parts};

    #[test]
    fn parts_end_in_the_separator_and_open_their_directory() {
        let parts = path_parts("/home/tester", true);
        let labels: Vec<_> = parts.iter().map(|part| part.label.as_str()).collect();
        assert_eq!(labels, ["/", "home/", "tester/"]);
        assert_eq!(
            parts[1],
            PathPart {
                label: "home/".into(),
                path: "/home".into()
            }
        );
        assert_eq!(path_parts("/", true).len(), 1);
        assert!(path_parts("", true).is_empty());
        #[cfg(unix)]
        assert_eq!(path_parts("/Users/me", false)[2].label, "me/");
    }

    #[test]
    fn folding_keeps_the_root_and_as_many_last_parts_as_fit() {
        let widths = [10., 50., 50., 50.];
        assert_eq!(folded_parts(&widths, 10., 160.), 0..0, "everything fits");
        // `/…/b/c/` is 10 + 10 + 50 + 50.
        assert_eq!(folded_parts(&widths, 10., 125.), 1..2);
        assert_eq!(folded_parts(&widths, 10., 80.), 1..3, "only the last part");
        assert_eq!(
            folded_parts(&widths, 10., 10.),
            1..3,
            "the last part is cut"
        );
        assert_eq!(
            folded_parts(&[10., 500.], 10., 100.),
            0..0,
            "nothing to fold"
        );
    }
}
