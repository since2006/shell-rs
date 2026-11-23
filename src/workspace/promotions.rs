//! 开始页的推广: where they are read from, and opening one.

use gpui_kit::*;

use super::Workspace;
use crate::app::OpenPromotion;
use crate::promotion::{PromotionServices, Promotions};

impl Workspace {
    /// Read the promotions from `services` (none: never), at once if the
    /// start page shows.
    pub fn set_promotion_services(
        &mut self,
        services: Option<PromotionServices>,
        cx: &mut Context<Self>,
    ) {
        let shown = self.skin.is_center_empty();
        self.promotions.update(cx, |promotions, cx| {
            promotions.set_services(services, cx);
            if shown {
                promotions.page_shown(cx);
            }
        });
    }

    pub fn promotions(&self) -> &Entity<Promotions> {
        &self.promotions
    }

    pub(super) fn on_open_promotion(
        &mut self,
        action: &OpenPromotion,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.open_url(&action.url);
    }
}
