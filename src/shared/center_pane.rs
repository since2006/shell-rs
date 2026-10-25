use gpui_kit::component::dock::Panel;
use gpui_kit::*;

/// A panel of the center: a terminal, an SFTP tab, an editor or 设置.
///
/// Leaving a dock area is not closing. A pane moves between tabs, and each
/// tab lays its panes out in a dock area of its own, so a move takes the
/// pane out of one area and puts it into another. The dock's `on_removed`
/// therefore only forgets the group the pane was in, and whatever closes a
/// pane for good calls [`CenterPane::close`] before taking it out.
pub trait CenterPane: Panel {
    /// End what the pane runs, a shell or a connection, and report it
    /// closed.
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>);
}
