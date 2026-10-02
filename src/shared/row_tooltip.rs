//! The tooltips of the rows in the sidebar's lists: hosts, port forwards
//! and credentials.

use std::{cell::Cell, rc::Rc, time::Duration};

use gpui_kit::base::animation::{EffectTransition, ease_in_out_cubic, ease_out_cubic};
use gpui_kit::base::{Placement, TooltipOverlay, TooltipRequest, TooltipTransition};
use gpui_kit::component::{ActiveTheme as _, tooltip::Tooltip, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// The longest note a row tooltip shows, in lines; longer ones are cut with
/// an ellipsis.
const NOTE_LINES: usize = 6;

/// What a row's tooltip says: a line, and under it, smaller and fainter, a
/// note that wraps (a host's notes, a key's whole path). Without a note the
/// line stands alone.
#[derive(Clone, Default)]
pub struct RowTooltip {
    text: SharedString,
    note: SharedString,
}

impl RowTooltip {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            note: SharedString::default(),
        }
    }

    /// The note under the line; an empty one is left out.
    pub fn note(mut self, note: impl Into<SharedString>) -> Self {
        self.note = note.into();
        self
    }
}

/// One list's row tooltips, keyed by what each row shows.
///
/// A tooltip opens beside its row, on the right, rather than under the
/// pointer as GPUI's own `.tooltip()` does: the pointer goes on down the
/// list, and the rows below stay in sight. Moving from row to row it follows
/// the pointer without waiting again. It is drawn in the theme's inverse,
/// dark on the light theme and light on the dark one, so it stands out
/// against the sidebar and the terminal in both; gpui-kit's tooltips share
/// the popover colour with menus.
///
/// The list's panel renders `overlay()` as a child, so the tooltip goes when
/// the panel does.
pub struct RowTooltips<K> {
    id: &'static str,
    overlay: Entity<TooltipOverlay>,
    /// The row the tooltip was last opened for. Only it may put the tooltip
    /// away: going down the list, the next row hears of the pointer before
    /// this one lets go of it, and this row's hide would cancel the tooltip
    /// the next row just asked for.
    owner: Rc<Cell<Option<K>>>,
}

impl<K> Clone for RowTooltips<K> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            overlay: self.overlay.clone(),
            owner: self.owner.clone(),
        }
    }
}

impl<K: Copy + PartialEq + 'static> RowTooltips<K> {
    /// `id` names the tooltip's line, and with `-note` after it the note.
    pub fn new(id: &'static str, cx: &mut App) -> Self {
        Self {
            id,
            overlay: cx.new(|_| TooltipOverlay::new().render_with(animate)),
            owner: Rc::new(Cell::new(None)),
        }
    }

    pub fn overlay(&self) -> Entity<TooltipOverlay> {
        self.overlay.clone()
    }

    /// The tooltip of the row showing `key`: `attach` it to the row, and
    /// `exclude` the parts of the row with tooltips of their own.
    pub fn row(&self, key: K, tooltip: RowTooltip) -> RowTooltipTrigger<K> {
        RowTooltipTrigger {
            tooltips: self.clone(),
            key,
            content: Rc::new(content(self.id, tooltip)),
            bounds: Rc::new(Cell::new(None)),
            excluded_bounds: Rc::new(Cell::new(None)),
            excluded: Rc::new(Cell::new(false)),
        }
    }
}

/// Builds a row's tooltip each time it shows.
type BuildTooltip = Rc<dyn Fn(&mut Window, &mut App) -> AnyView>;

/// One row's tooltip, from `RowTooltips::row`.
pub struct RowTooltipTrigger<K> {
    tooltips: RowTooltips<K>,
    key: K,
    content: BuildTooltip,
    /// The row's content, as laid out in the last frame.
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The excluded part, which may lie outside the content: a ListItem's
    /// suffix sits beside its content box.
    excluded_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Whether the pointer is on the excluded part.
    excluded: Rc<Cell<bool>>,
}

impl<K: Copy> Clone for RowTooltipTrigger<K> {
    fn clone(&self) -> Self {
        Self {
            tooltips: self.tooltips.clone(),
            key: self.key,
            content: self.content.clone(),
            bounds: self.bounds.clone(),
            excluded_bounds: self.excluded_bounds.clone(),
            excluded: self.excluded.clone(),
        }
    }
}

impl<K: Copy + PartialEq + 'static> RowTooltipTrigger<K> {
    /// Show the tooltip while the pointer is on `row`, except when a row is
    /// being dragged over the others; any press puts it away, out of the way
    /// of the menu, the click and the drag.
    pub fn attach<E: ParentElement + StatefulInteractiveElement>(&self, row: E) -> E {
        let (hover, press) = (self.clone(), self.clone());
        // On a ListItem this measures its content box, which leaves out the
        // suffix; `exclude` measures what it is given.
        row.child(measure(self.bounds.clone()))
            .on_hover(move |hovered, window, cx| {
                if *hovered && !hover.excluded.get() && !cx.has_active_drag() {
                    hover.show(window, cx);
                } else {
                    hover.release(window, cx);
                }
            })
            .on_any_mouse_down(move |_, _, cx| press.dismiss(cx))
    }

    /// No tooltip for the row while the pointer is on `element`, a button
    /// with its own tooltip: the two would show at once. Back on the rest of
    /// the row, the row's tooltip comes back. The tooltip still opens beside
    /// it, as part of the row, when it lies at the row's end.
    pub fn exclude<E: ParentElement + StatefulInteractiveElement>(&self, element: E) -> E {
        let trigger = self.clone();
        element
            .child(measure(self.excluded_bounds.clone()))
            .on_hover(move |hovered, window, cx| {
                trigger.excluded.set(*hovered);
                if *hovered {
                    trigger.dismiss(cx);
                } else if trigger.row_bounds().contains(&window.mouse_position())
                    && !cx.has_active_drag()
                {
                    // Asks where the pointer is rather than whether the row is
                    // hovered: leaving for the row below, this runs before the
                    // row hears that the pointer is gone.
                    trigger.show(window, cx);
                }
            })
    }

    fn show(&self, window: &mut Window, cx: &mut App) {
        self.tooltips.owner.set(Some(self.key));
        let content = self.content.clone();
        let request = TooltipRequest::new(self.row_bounds(), move |window, cx| content(window, cx))
            .placement(Placement::Right);
        self.tooltips
            .overlay
            .update(cx, |overlay, cx| overlay.request_show(request, window, cx));
    }

    /// Everything in the row: its content, and an excluded suffix.
    fn row_bounds(&self) -> Bounds<Pixels> {
        let bounds = self.bounds.get().unwrap_or_default();
        match self.excluded_bounds.get() {
            Some(excluded) => bounds.union(&excluded),
            None => bounds,
        }
    }

    /// The pointer left: put the tooltip away, if it is this row's.
    fn release(&self, window: &mut Window, cx: &mut App) {
        if self.tooltips.owner.get() == Some(self.key) {
            self.tooltips.owner.set(None);
            self.tooltips
                .overlay
                .update(cx, |overlay, cx| overlay.request_hide(window, cx));
        }
    }

    /// Put away whatever tooltip is showing, at once.
    fn dismiss(&self, cx: &mut App) {
        self.tooltips.owner.set(None);
        self.tooltips
            .overlay
            .update(cx, |overlay, cx| overlay.hide(cx));
    }
}

/// A canvas that records the bounds of the element it is put in. The insets
/// matter: an absolute child without them is laid out below the content
/// instead of over it.
fn measure(cell: Rc<Cell<Option<Bounds<Pixels>>>>) -> impl IntoElement {
    canvas(move |bounds, _, _| cell.set(Some(bounds)), |_, _, _, _| {})
        .absolute()
        .top_0()
        .left_0()
        .size_full()
}

fn content(
    id: &'static str,
    tooltip: RowTooltip,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let note_id = ElementId::Name(format!("{id}-note").into());
    move |window, cx| {
        let RowTooltip { text, note } = tooltip.clone();
        let note_id = note_id.clone();
        let (background, foreground) = (cx.theme().foreground, cx.theme().background);
        Tooltip::element(move |_, _| {
            v_flex()
                .gap_1()
                .child(
                    div()
                        .id(id)
                        .test_support()
                        .aria_label(text.clone())
                        .child(text.clone()),
                )
                .when(!note.is_empty(), |content| {
                    // Wraps rather than widening the tooltip across the
                    // window; a step quieter than the line above it.
                    content.child(
                        div()
                            .id(note_id.clone())
                            .test_support()
                            .aria_label(note.clone())
                            .max_w(rems(20.))
                            .whitespace_normal()
                            .line_clamp(NOTE_LINES)
                            .text_xs()
                            .text_color(foreground.opacity(0.75))
                            .child(note.clone()),
                    )
                })
        })
        .bg(background)
        .border_color(background)
        .text_color(foreground)
        .build(window, cx)
    }
}

/// The motion, after gpui-kit's own tooltips: the tooltip eases out of the
/// row when it first opens, then follows the pointer from row to row.
fn animate(
    view: AnyView,
    transition: TooltipTransition,
    _: &mut Window,
    _: &mut App,
) -> AnyElement {
    let tooltip = div().child(view);
    match transition {
        TooltipTransition::Enter { epoch } => EffectTransition::new(Duration::from_millis(150))
            .ease(ease_out_cubic)
            .slide_x(px(-4.), px(0.))
            .fade(0., 1.)
            .apply(
                tooltip,
                ElementId::NamedInteger("row-tooltip-enter".into(), epoch as u64),
            )
            .into_any_element(),
        TooltipTransition::Switch {
            epoch,
            previous,
            current,
        } => EffectTransition::new(Duration::from_millis(200))
            .ease(ease_in_out_cubic)
            .slide_y(previous.center().y - current.center().y, px(0.))
            .apply(
                tooltip,
                ElementId::NamedInteger("row-tooltip-move".into(), epoch as u64),
            )
            .into_any_element(),
    }
}
