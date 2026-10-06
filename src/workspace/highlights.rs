//! The workspace's share of 关键字高亮: the commands of the rule list in
//! the settings, which open the rule dialog and ask before deleting.

use std::rc::Rc;

use gpui_kit::*;

use crate::app::{DeleteHighlightRule, EditHighlightRule, NewHighlightRule};
use crate::settings::open_highlight_dialog;
use crate::shared::confirm_delete;

use super::workspace_view::Workspace;

impl Workspace {
    pub(super) fn on_new_highlight_rule(
        &mut self,
        _: &NewHighlightRule,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        open_highlight_dialog(None, self.settings().clone(), window, cx);
    }

    pub(super) fn on_edit_highlight_rule(
        &mut self,
        action: &EditHighlightRule,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let exists = action.0
            < self
                .settings()
                .read(cx)
                .settings()
                .terminal_highlight
                .rules
                .len();
        if exists {
            open_highlight_dialog(Some(action.0), self.settings().clone(), window, cx);
        }
    }

    pub(super) fn on_delete_highlight_rule(
        &mut self,
        action: &DeleteHighlightRule,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ix = action.0;
        let Some(rule) = self
            .settings()
            .read(cx)
            .settings()
            .terminal_highlight
            .rules
            .get(ix)
            .cloned()
        else {
            return;
        };
        let settings = self.settings().clone();
        let pattern = rule.pattern.clone();
        confirm_delete(
            &pattern,
            None,
            Rc::new(move |_, cx| {
                settings.update(cx, |settings, cx| {
                    settings.update(
                        |settings| {
                            // Only the rule that was asked about.
                            let rules = &mut settings.terminal_highlight.rules;
                            if rules.get(ix) == Some(&rule) {
                                rules.remove(ix);
                            }
                        },
                        cx,
                    )
                });
            }),
            window,
            cx,
        );
    }
}
