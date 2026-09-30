use std::rc::Rc;

use gpui_kit::component::{
    IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{ContextMenuExt as _, PopupMenu},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::ToggleSessionPanel;

type MenuBuilder = Rc<dyn Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu>;

/// A dock tab title: a leading mark, the label and, for closable panels, a
/// close button that dispatches the panel's close action. A panel may also
/// give the title a context menu, which opens on a right click anywhere on it.
///
/// Closing goes through an action rather than `TabGroup::close_panel` on
/// purpose: the group refuses to close the last panel of a region, while the
/// workspace wants every tab closable (an empty center shows the recent
/// sessions instead) and removes the panel from the dock itself.
///
/// Double-clicking the title shows or hides the session sidebar, the way a
/// double click on an editor tab gives it the room elsewhere.
///
/// The context menu and the double click hang on the title rather than on
/// the dock's `Tab`, which the application does not render. The tab's own
/// horizontal padding is therefore outside them; clicks there only select.
#[derive(IntoElement)]
pub struct ClosableTabTitle {
    id: ElementId,
    leading: AnyElement,
    label: SharedString,
    close: Option<(ElementId, Box<dyn Action>)>,
    menu: Option<MenuBuilder>,
}

impl ClosableTabTitle {
    /// `id` identifies the tab and must be unique among the tabs of a bar:
    /// the context menu keeps its open state under it.
    pub fn new(
        id: impl Into<ElementId>,
        leading: impl IntoElement,
        label: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            leading: leading.into_any_element(),
            label: label.into(),
            close: None,
            menu: None,
        }
    }

    /// Show a close button that dispatches `action` on the focused path.
    pub fn closable(mut self, id: impl Into<ElementId>, action: Box<dyn Action>) -> Self {
        self.close = Some((id.into(), action));
        self
    }

    /// Open a menu built by `builder` on a right click. The builder runs when
    /// the menu opens, not while the tab renders.
    pub fn context_menu(
        mut self,
        builder: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> Self {
        self.menu = Some(Rc::new(builder));
        self
    }
}

impl RenderOnce for ClosableTabTitle {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let row = h_flex()
            .id("title")
            .gap_1()
            .child(self.leading)
            .child(self.label.clone())
            .when_some(self.close, |row, (id, action)| {
                row.child(
                    Button::new(id)
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip("关闭")
                        .on_click(move |_, window, cx| {
                            // Closing must not also select or drag the tab.
                            cx.stop_propagation();
                            window.dispatch_action(action.boxed_clone(), cx);
                        }),
                )
            });
        div()
            .id(self.id)
            // The first click selects the tab as usual.
            .on_click(|event, window, cx| {
                if event.click_count() == 2 {
                    window.dispatch_action(Box::new(ToggleSessionPanel), cx);
                }
            })
            .test_support()
            .aria_label(self.label)
            .child(match self.menu {
                Some(menu) => row
                    .context_menu(move |popup, window, cx| menu(popup, window, cx))
                    .into_any_element(),
                None => row.into_any_element(),
            })
    }
}
