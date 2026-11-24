//! How the SFTP lists take the pointer, as WinSCP's do without full row
//! select: the name cells are the items, which clicks select; everything
//! else in the list is empty space, where a click clears the selection and a
//! drag draws a rectangle that selects the rows it covers, scrolling the list
//! past its edges. The file panes and 查找文件's results share it, so the
//! two behave alike.

use super::{
    Selection,
    selection::{row_at, swept_rows},
};
use crate::shared::RightClickMenu as _;
use gpui_kit::component::{
    ActiveTheme as _,
    menu::PopupMenu,
    table::{TableDelegate, TableState},
};
use gpui_kit::*;
use std::cell::Cell;
use std::rc::Rc;

/// Where the list and its rows are on screen, recorded as they paint, so a
/// selection rectangle can tell which rows it covers. Rows are all the same
/// height, so one row tells where all of them are.
#[derive(Default)]
pub(super) struct ListGeometry {
    /// Where the first row's top is (scrolled out of view or not), and the
    /// row height.
    pub rows: Cell<Option<(Pixels, Pixels)>>,
    /// The list, header included.
    pub list: Cell<Option<Bounds<Pixels>>>,
    /// Where the left button went down in the list last.
    pub press: Cell<Option<Point<Pixels>>>,
    /// What the last press in the list landed on.
    pub pressed: Cell<Pressed>,
}

/// What a press in the list landed on. As in WinSCP without full row select,
/// everything but the name cells and the column titles is empty space:
/// clicking there clears the selection, and dragging from there draws a
/// rectangle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Pressed {
    #[default]
    Empty,
    /// A name cell, which clicks and right-clicks select. Dragging its icon
    /// and text moves files, in a pane; dragging the rest of it draws a
    /// rectangle.
    Name,
    /// A column title.
    Title,
}

/// How far a press may move and still be a click: GPUI's own drag
/// threshold, which is private.
const DRAG_THRESHOLD: f64 = 2.;

/// Whether a click is the end of a drag instead. GPUI forgets every pending
/// press once it paints a frame with a drag active, but a drag started and
/// let go within one frame (a quick flick) still clicks each element both
/// ends were over, bar the drag's own: a rectangle or a file drag let go
/// over the name cell it began in must not select that row alone.
pub(super) fn ends_a_drag(event: &ClickEvent) -> bool {
    match event {
        ClickEvent::Mouse(click) => {
            (click.up.position - click.down.position).magnitude() > DRAG_THRESHOLD
        }
        _ => false,
    }
}

/// A selection rectangle being dragged over the list: it starts anywhere
/// but on a column title (or, in a pane, a name's icon and text, which drag
/// the file instead).
struct Marquee {
    /// Where it started: `x` in the window, `y` from the top of the first
    /// row, so it stays put while the list scrolls.
    anchor: Point<Pixels>,
    /// The pointer now, in the window.
    pointer: Point<Pixels>,
    /// The selection it adds to, when ⌘ or Shift was held.
    base: Option<Selection>,
}

/// What a drag from empty space carries: only whose it is. GPUI tells every
/// listener for the type about every drag of it, so the other pane, other
/// SFTP tabs and the results must leave this one alone.
#[derive(Clone)]
pub(super) struct MarqueeDrag {
    list: EntityId,
}

impl Render for MarqueeDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// A list's rectangle and where its rows are.
#[derive(Default)]
pub(super) struct ListMarquee {
    pub geometry: Rc<ListGeometry>,
    marquee: Option<Marquee>,
    /// Scrolls the list while the rectangle is dragged past its edge.
    scroll: gpui_kit::base::AutoScroll,
}

/// A view whose list has a selection rectangle.
pub(super) trait MarqueeList: Sized + 'static {
    type Rows: TableDelegate;

    fn table(&self) -> &Entity<TableState<Self::Rows>>;
    fn list_marquee(&self) -> &ListMarquee;
    fn list_marquee_mut(&mut self) -> &mut ListMarquee;
    fn selection(&self, cx: &App) -> Selection;
    /// Change the selection, given the rows' keys in the order shown.
    fn update_selection(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut Selection, &[&str]),
    );
    /// What a press in the list, or a rectangle, puts away first.
    fn on_list_press(&mut self, _: &mut Context<Self>) {}
}

impl ListMarquee {
    /// Records where the list is; goes after the table, over it.
    pub(super) fn canvas(&self) -> impl IntoElement + use<> {
        let geometry = self.geometry.clone();
        canvas(
            move |bounds, _, _| geometry.list.set(Some(bounds)),
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    /// The rectangle as drawn, in the list's own coordinates and inside it;
    /// goes after the table, over it.
    pub(super) fn rectangle(&self, cx: &App) -> Option<impl IntoElement + use<>> {
        let marquee = self.marquee.as_ref()?;
        let list = self.geometry.list.get()?;
        let (first_row, _) = self.geometry.rows.get()?;
        let anchor = point(marquee.anchor.x, marquee.anchor.y + first_row);
        let clamp = |at: Point<Pixels>| {
            point(
                at.x.clamp(list.left(), list.right()),
                at.y.clamp(list.top(), list.bottom()),
            )
        };
        let (a, b) = (clamp(anchor), clamp(marquee.pointer));
        let origin = point(a.x.min(b.x), a.y.min(b.y)) - list.origin;
        let size = size((a.x - b.x).abs(), (a.y - b.y).abs());
        let theme = cx.theme();
        // The colors of a selected name: the rectangle is about to select
        // what it covers.
        Some(
            div()
                .id("selection-rectangle")
                .absolute()
                .left(origin.x)
                .top(origin.y)
                .w(size.width)
                .h(size.height)
                .border_1()
                .border_color(theme.table_active_border)
                .bg(theme.tokens.table_active),
        )
    }
}

/// The list's own pointer handling: where a press lands, a click on empty
/// space, and the rectangle. The view adds `ListMarquee::canvas` and
/// `ListMarquee::rectangle` after the table.
pub(super) fn select_area<T: MarqueeList>(
    area: Stateful<Div>,
    list: &T,
    cx: &mut Context<T>,
) -> Stateful<Div> {
    let geometry = list.list_marquee().geometry.clone();
    let id = cx.entity_id();
    area.relative()
        // Names and titles say so as the press bubbles up; what is left is
        // empty space.
        .capture_any_mouse_down({
            let geometry = geometry.clone();
            move |_, _, _| geometry.pressed.set(Pressed::Empty)
        })
        .on_mouse_down(MouseButton::Left, move |event, _, _| {
            geometry.press.set(Some(event.position))
        })
        .on_click(cx.listener(|list, event: &ClickEvent, _, cx| {
            if !event.is_keyboard() {
                press_empty(list, event.modifiers(), cx)
            }
        }))
        .on_drag(MarqueeDrag { list: id }, |drag, _, _, cx| {
            cx.new(|_| drag.clone())
        })
        .on_drag_move(
            cx.listener(|list, event: &DragMoveEvent<MarqueeDrag>, _, cx| {
                if event.drag(cx).list == cx.entity_id() {
                    drag_marquee(list, event.event.position, event.event.modifiers, cx)
                }
            }),
        )
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|list, _, _, cx| end(list, cx)),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|list, _, _, cx| end(list, cx)),
        )
}

/// The list's one menu, for rows, column titles and empty space alike: a
/// layer over the table, put after it.
///
/// The table has a menu of its own, which nothing here fills, and of nested
/// menus the innermost takes the right-click and stops it there, so a menu
/// on the list around the table would never open (issue #154). Painted
/// after the table, the layer is the innermost. It stops the press before
/// the rows and titles under it hear it bubble up, so they say what was
/// pressed in the capture phase, and the layer, last to capture, clears the
/// selection on a right-click in empty space.
pub(super) fn menu_layer<T: MarqueeList>(
    cx: &mut Context<T>,
    build: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
) -> impl IntoElement {
    div()
        .id("list-menu")
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .capture_any_mouse_down(cx.listener(|list, event: &MouseDownEvent, _, cx| {
            if event.button == MouseButton::Right {
                press_empty(list, event.modifiers, cx)
            }
        }))
        .context_menu(build)
}

/// A click or right-click on empty space clears the selection, as in
/// WinSCP. A ⌘ or Shift click there leaves it alone.
pub(super) fn press_empty<T: MarqueeList>(list: &mut T, modifiers: Modifiers, cx: &mut Context<T>) {
    list.on_list_press(cx);
    if list.list_marquee().geometry.pressed.get() != Pressed::Empty
        || modifiers.secondary(cx)
        || modifiers.shift
    {
        return;
    }
    if list.selection(cx) != Selection::default() {
        list.update_selection(cx, |selection, _| selection.clear());
    }
}

/// The selection rectangle follows the pointer: the rows it covers become
/// the selection, and the list scrolls when it goes past an edge.
fn drag_marquee<T: MarqueeList>(
    list: &mut T,
    pointer: Point<Pixels>,
    modifiers: Modifiers,
    cx: &mut Context<T>,
) {
    let geometry = list.list_marquee().geometry.clone();
    // A title may move its column. A name's icon and text, in a pane, drag
    // its files instead, and that drag, being theirs, wins.
    if geometry.pressed.get() == Pressed::Title {
        return;
    }
    let Some((first_row, _)) = geometry.rows.get() else {
        return;
    };
    if list.list_marquee().marquee.is_none() {
        let start = geometry.press.get().unwrap_or(pointer);
        list.on_list_press(cx);
        let base = (modifiers.secondary(cx) || modifiers.shift).then(|| list.selection(cx));
        list.list_marquee_mut().marquee = Some(Marquee {
            anchor: point(start.x, start.y - first_row),
            pointer,
            base,
        });
    }
    if let Some(marquee) = &mut list.list_marquee_mut().marquee {
        marquee.pointer = pointer;
    }
    apply(list, cx);
    let delta = geometry
        .list
        .get()
        .and_then(|bounds| gpui_kit::base::AutoScroll::compute_delta(pointer.y, bounds));
    list.list_marquee_mut()
        .scroll
        .set(delta, cx, |delta, list, cx| scroll_marquee(list, delta, cx));
}

/// Select the rows under the rectangle.
fn apply<T: MarqueeList>(list: &mut T, cx: &mut Context<T>) {
    let geometry = list.list_marquee().geometry.clone();
    let (Some(marquee), Some((first_row, row_height))) =
        (&list.list_marquee().marquee, geometry.rows.get())
    else {
        return;
    };
    let height = f32::from(row_height);
    let from = f32::from(marquee.anchor.y);
    let to = f32::from(marquee.pointer.y - first_row);
    let base = marquee.base.clone();
    list.update_selection(cx, |selection, order| {
        selection.sweep(
            base.as_ref(),
            swept_rows(from, to, height, order.len()),
            row_at(to, height, order.len()),
            order,
        )
    });
}

/// One step of scrolling while the rectangle is past the list's edge.
fn scroll_marquee<T: MarqueeList>(list: &mut T, delta: Pixels, cx: &mut Context<T>) {
    if list.list_marquee().marquee.is_none() {
        list.list_marquee_mut().scroll.stop();
        return;
    }
    let scroll = list
        .table()
        .read(cx)
        .vertical_scroll_handle
        .0
        .borrow()
        .base_handle
        .clone();
    let offset = scroll.offset();
    let lowest = -scroll.max_offset().y;
    let y = (offset.y - delta).clamp(lowest, px(0.));
    if y != offset.y {
        scroll.set_offset(point(offset.x, y));
        // The rows moved by as much; the next paint records it too.
        let geometry = &list.list_marquee().geometry;
        if let Some((first_row, height)) = geometry.rows.get() {
            geometry
                .rows
                .set(Some((first_row + (y - offset.y), height)));
        }
        apply(list, cx);
    }
}

fn end<T: MarqueeList>(list: &mut T, cx: &mut Context<T>) {
    let marquee = list.list_marquee_mut();
    marquee.scroll.stop();
    if marquee.marquee.take().is_some() {
        cx.notify();
    }
}
