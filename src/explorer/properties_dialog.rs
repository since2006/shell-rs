//! 属性: what the selection is, and its rwx permissions as WinSCP edits
//! them: a 3×3 grid and an octal field that stay in step. With several items,
//! a bit that differs between them is left alone unless the user changes it.

use super::{ExplorerPanel, FileEntry, PaneOperation, format_changed, format_size};
use crate::app::ExplorerCommand;
use crate::i18n::{t, tn};
use crate::sftp::PermissionEdit;
use crate::shared::commit_footer;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _,
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputEvent, InputState},
    separator::Separator,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// The nine rwx bits, owner read first.
const BITS: [u32; 9] = [
    0o400, 0o200, 0o100, 0o040, 0o020, 0o010, 0o004, 0o002, 0o001,
];

/// The permission grid's state for one or more items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PermissionDraft {
    value: u32,
    /// Bits that differ between the items.
    mixed: u32,
    /// Bits the user has set, by checkbox or octal field.
    touched: u32,
}

impl PermissionDraft {
    pub fn from_modes(modes: &[u32]) -> Self {
        let all = modes.iter().fold(0o777, |bits, mode| bits & mode) & 0o777;
        let any = modes.iter().fold(0, |bits, mode| bits | mode) & 0o777;
        Self {
            value: all,
            mixed: any & !all,
            touched: 0,
        }
    }

    pub fn is_checked(&self, bit: u32) -> bool {
        self.value & bit != 0
    }

    /// Differs between the items and has not been set by the user.
    #[cfg(test)]
    pub fn is_mixed(&self, bit: u32) -> bool {
        self.mixed & !self.touched & bit != 0
    }

    pub fn has_mixed(&self) -> bool {
        self.mixed & !self.touched != 0
    }

    pub fn set_bit(&mut self, bit: u32, on: bool) {
        self.touched |= bit;
        if on {
            self.value |= bit;
        } else {
            self.value &= !bit;
        }
    }

    /// A complete octal value replaces all nine bits.
    pub fn set_octal(&mut self, mode: u32) {
        self.value = mode & 0o777;
        self.touched = 0o777;
    }

    /// The octal field's text; empty while any bit is mixed.
    pub fn octal(&self) -> String {
        if self.has_mixed() {
            String::new()
        } else {
            format!("{:03o}", self.value)
        }
    }

    /// The change to apply: only the bits the user touched.
    pub fn edit(&self) -> PermissionEdit {
        PermissionEdit::new(self.value & self.touched, !self.value & self.touched)
    }
}

/// Parse three octal digits.
fn parse_octal(text: &str) -> Option<u32> {
    (text.len() == 3)
        .then(|| u32::from_str_radix(text, 8).ok())
        .flatten()
}

struct PropertiesForm {
    items: Vec<FileEntry>,
    location: String,
    remote: bool,
    draft: PermissionDraft,
    /// Links carry no permissions of their own; only the rest change.
    editable: bool,
    has_dirs: bool,
    recursive: bool,
    add_x_to_dirs: bool,
    octal: Entity<InputState>,
    _subscription: Subscription,
}

impl PropertiesForm {
    fn new(
        items: Vec<FileEntry>,
        location: String,
        remote: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let modes: Vec<u32> = items
            .iter()
            .filter(|item| !item.is_link())
            .filter_map(|item| item.permissions)
            .collect();
        let draft = PermissionDraft::from_modes(&modes);
        let octal = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("—")
                .validate(|text, _| {
                    text.len() <= 3 && text.chars().all(|c| ('0'..='7').contains(&c))
                })
                .default_value(draft.octal())
        });
        let subscription = cx.subscribe_in(&octal, window, |this, input, event, _, cx| {
            if let InputEvent::Change = event
                && let Some(mode) = parse_octal(input.read(cx).value().as_ref())
            {
                this.draft.set_octal(mode);
                cx.notify();
            }
        });
        Self {
            has_dirs: items.iter().any(|item| item.kind == super::FileKind::Dir),
            editable: !modes.is_empty(),
            items,
            location,
            remote,
            draft,
            recursive: false,
            add_x_to_dirs: true,
            octal,
            _subscription: subscription,
        }
    }

    fn row(label: SharedString, value: String, cx: &App) -> impl IntoElement {
        h_flex()
            .gap_3()
            .text_sm()
            .child(
                div()
                    .w_16()
                    .flex_shrink_0()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(div().min_w_0().text_ellipsis().child(value))
    }

    /// One value when every item agrees, otherwise a note that they differ.
    fn shared(values: impl Iterator<Item = Option<String>>) -> String {
        let values: Vec<_> = values.collect();
        match values.first() {
            Some(first) if values.iter().all(|value| value == first) => {
                first.clone().unwrap_or_else(|| "—".into())
            }
            Some(_) => t!("explorer.properties.mixed").into(),
            None => "—".into(),
        }
    }
}

impl Render for PropertiesForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let size = if self.items.iter().any(FileEntry::is_dir) {
            "—".to_string()
        } else {
            let bytes: u64 = self.items.iter().map(|item| item.size).sum();
            tn!("explorer.properties.size", bytes, size = format_size(bytes)).into()
        };
        let classes = [
            t!("explorer.properties.class_owner"),
            t!("explorer.properties.class_group"),
            t!("explorer.properties.class_others"),
        ];
        let verbs = [
            t!("explorer.properties.read"),
            t!("explorer.properties.write"),
            t!("explorer.properties.execute"),
        ];
        let grid = v_flex()
            .gap_1()
            .children(classes.iter().enumerate().map(|(row, class)| {
                h_flex()
                    .gap_3()
                    .text_sm()
                    .child(div().w_16().flex_shrink_0().child(class.clone()))
                    .children(verbs.iter().enumerate().map(|(column, verb)| {
                        let index = row * 3 + column;
                        let bit = BITS[index];
                        Checkbox::new(("perm", index))
                            .label(verb.clone())
                            .small()
                            .checked(self.draft.is_checked(bit))
                            .disabled(!self.editable)
                            .on_change(cx.listener(move |this, checked: &bool, window, cx| {
                                this.draft.set_bit(bit, *checked);
                                let octal = this.draft.octal();
                                this.octal
                                    .update(cx, |input, cx| input.set_value(octal, window, cx));
                                cx.notify();
                            }))
                    }))
            }));
        v_flex()
            .gap_3()
            .child(Self::row(
                t!("explorer.properties.location"),
                self.location.clone(),
                cx,
            ))
            .child(Self::row(t!("explorer.column.size"), size, cx))
            .when(self.items.len() == 1, |this| {
                this.child(Self::row(
                    t!("explorer.column.modified"),
                    self.items[0]
                        .modified
                        .map(format_changed)
                        .unwrap_or_default(),
                    cx,
                ))
            })
            .when(self.remote, |this| {
                this.child(Self::row(
                    t!("explorer.column.owner"),
                    Self::shared(
                        self.items
                            .iter()
                            .map(|item| item.owner.as_ref().map(|o| o.to_string())),
                    ),
                    cx,
                ))
                .child(Self::row(
                    t!("explorer.properties.group"),
                    Self::shared(
                        self.items
                            .iter()
                            .map(|item| item.group.as_ref().map(|g| g.to_string())),
                    ),
                    cx,
                ))
            })
            .child(Separator::horizontal())
            .child(grid)
            .child(
                h_flex()
                    .gap_3()
                    .text_sm()
                    .child(
                        div()
                            .w_16()
                            .flex_shrink_0()
                            .child(t!("explorer.properties.octal")),
                    )
                    .child(
                        div().w_20().child(
                            Input::new(&self.octal)
                                .id("perm-octal")
                                .small()
                                .disabled(!self.editable),
                        ),
                    ),
            )
            .when(self.draft.has_mixed(), |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(t!("explorer.properties.mixed_note")),
                )
            })
            .when(!self.editable, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(if self.items.iter().all(FileEntry::is_link) {
                            t!("explorer.properties.links")
                        } else {
                            t!("explorer.properties.unsupported")
                        }),
                )
            })
            .when(self.has_dirs && self.editable, |this| {
                this.child(
                    v_flex()
                        .gap_1()
                        .child(
                            Checkbox::new("perm-recursive")
                                .label(t!("explorer.properties.recursive"))
                                .small()
                                .checked(self.recursive)
                                .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                    this.recursive = *checked;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Checkbox::new("perm-dir-x")
                                .label(t!("explorer.properties.dir_x"))
                                .small()
                                .checked(self.add_x_to_dirs)
                                .disabled(!self.recursive)
                                .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                    this.add_x_to_dirs = *checked;
                                    cx.notify();
                                })),
                        ),
                )
            })
    }
}

impl ExplorerPanel {
    pub(super) fn open_properties(
        &mut self,
        remote: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pane = self.pane(remote).read(cx);
        let items = pane.selected_entries(cx);
        let location = pane.path();
        if items.is_empty() || !self.can_modify(remote, cx) || window.has_active_dialog(cx) {
            return;
        }
        let title = if let [item] = items.as_slice() {
            t!("explorer.properties.title_one", name = item.name)
        } else {
            tn!("explorer.properties.title_several", items.len())
        };
        let names: Vec<String> = items
            .iter()
            .filter(|item| !item.is_link())
            .map(|item| item.name.to_string())
            .collect();
        let form = cx.new(|cx| PropertiesForm::new(items, location, remote, window, cx));
        let sender = self.sender();
        let focus = window.focused(cx);
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(title.clone())
                // Closed by its buttons or Escape, not by a click beside it.
                .overlay_closable(false)
                .child(form.clone())
                .footer(commit_footer("commit", t!("explorer.properties.apply")))
                .on_ok({
                    let (form, sender, names) = (form.clone(), sender.clone(), names.clone());
                    move |_, window, cx| {
                        let (edit, recursive, add_x_to_dirs, editable) = {
                            let form = form.read(cx);
                            let recursive = form.recursive && form.has_dirs;
                            (
                                form.draft.edit(),
                                recursive,
                                recursive && form.add_x_to_dirs,
                                form.editable,
                            )
                        };
                        // Nothing to change: close without a round trip.
                        if !editable || (edit.is_empty() && !add_x_to_dirs) {
                            return true;
                        }
                        let operation = PaneOperation::Permissions {
                            names: names.clone(),
                            edit,
                            recursive,
                            add_x_to_dirs,
                        };
                        sender.send(ExplorerCommand::Operate { remote, operation }, window, cx);
                        true
                    }
                })
                .on_close({
                    let focus = focus.clone();
                    move |_, window, cx| {
                        if let Some(focus) = &focus {
                            window.focus(focus, cx);
                        }
                    }
                })
        });
    }
}

#[cfg(test)]
mod tests {
    // Explicit imports: `gpui_kit::*` would shadow `#[test]`.
    use super::{PermissionDraft, parse_octal};
    use crate::sftp::PermissionEdit;

    #[test]
    fn one_item_edits_exactly_what_the_user_changed() {
        let mut draft = PermissionDraft::from_modes(&[0o100_644]);
        assert_eq!(draft.octal(), "644");
        assert!(!draft.has_mixed());
        draft.set_bit(0o100, true);
        assert_eq!(draft.octal(), "744");
        assert_eq!(draft.edit(), PermissionEdit::new(0o100, 0));
        draft.set_octal(0o600);
        assert_eq!(draft.edit(), PermissionEdit::exact(0o600));
        assert_eq!(
            draft.edit().apply(0o104_755, false, false),
            0o104_600,
            "setuid stays"
        );
    }

    #[test]
    fn differing_bits_stay_per_item_until_touched() {
        let mut draft = PermissionDraft::from_modes(&[0o644, 0o755]);
        assert!(draft.is_checked(0o400) && !draft.is_mixed(0o400));
        assert!(draft.is_mixed(0o100) && !draft.is_checked(0o100));
        assert_eq!(draft.octal(), "", "the octal field cannot show a mix");
        draft.set_bit(0o002, true);
        let edit = draft.edit();
        assert_eq!(edit.apply(0o644, false, false), 0o646);
        assert_eq!(edit.apply(0o755, false, false), 0o757);
        draft.set_bit(0o100, false);
        assert!(!draft.has_mixed() || draft.is_mixed(0o010));
    }

    #[test]
    fn directories_can_gain_search_where_they_can_be_read() {
        let edit = PermissionEdit::exact(0o644);
        assert_eq!(edit.apply(0o40_700, true, true), 0o40_755);
        assert_eq!(edit.apply(0o40_700, true, false), 0o40_644);
        assert_eq!(edit.apply(0o100_700, false, true), 0o100_644);
    }

    #[test]
    fn only_three_octal_digits_parse() {
        assert_eq!(parse_octal("755"), Some(0o755));
        assert_eq!(parse_octal("75"), None);
        assert_eq!(parse_octal("7555"), None);
    }
}
