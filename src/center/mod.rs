//! 中间区: the tab bar and, for each tab, its panes laid out in a dock area
//! of their own. Knows nothing of the workspace around it, so that each
//! window can hold one.

mod drag;
pub mod layout;
pub mod model;
mod naming;
mod pane_controls;
mod pane_skin;
mod pane_view;
mod places;
mod tab_bar;
mod view;

pub use layout::Layout;
pub use model::{PaneKind, TabId, TabTitle, Tabs};
pub use naming::TabNaming;
pub use places::{TabPlace, tab_place};
pub use tab_bar::LeadingControls;
pub use view::{CenterEvent, CenterView, split_items};

use crate::app::PaneContent;

impl PaneKind for PaneContent {
    fn is_terminal(&self) -> bool {
        matches!(self, Self::Terminal(_) | Self::LocalTerminal(_))
    }

    fn stands_alone(&self) -> bool {
        matches!(self, Self::Settings)
    }
}
