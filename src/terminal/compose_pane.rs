//! A tab-owned, memory-only draft. Textarea owns editing and IME; only a
//! fresh plain Enter queues an explicit submit action for this same tab.

use gpui_kit::component::{
    ActiveTheme as _,
    input::{Textarea, TextareaState},
};
use gpui_kit::*;

use super::{RemoteTerminalId, TerminalView};
use crate::app::SubmitTerminalDraft;
use crate::i18n::t;

struct PendingDraft {
    text: SharedString,
    generation: u64,
}

pub(super) struct ComposePane {
    id: RemoteTerminalId,
    draft: Entity<TextareaState>,
    terminal: Entity<TerminalView>,
    pending: Option<PendingDraft>,
    _subscriptions: Vec<Subscription>,
}

impl ComposePane {
    pub(super) fn new(
        id: RemoteTerminalId,
        terminal: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let draft = cx.new(|cx| {
            TextareaState::new(window, cx)
                .submit_on_enter(true)
                .placeholder(t!("terminal.compose.placeholder"))
        });
        let focus = draft.focus_handle(cx);
        let subscriptions = vec![cx.on_blur(&focus, window, |this, _, _| {
            this.pending = None;
        })];
        Self {
            id,
            draft,
            terminal,
            pending: None,
            _subscriptions: subscriptions,
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.draft.focus_handle(cx).is_focused(window)
            || event.keystroke.key != "enter"
            || event.keystroke.modifiers != Modifiers::default()
        {
            return;
        }
        // Textarea's submit-on-enter binding propagates plain Enter without
        // editing. Its Shift+Enter binding consumes the key after a newline.
        // Inspect the raw event here, not PressEnter, which loses key-repeat
        // and IME metadata. Never let this Enter reach terminal reconnect.
        cx.stop_propagation();
        if event.is_held || event.prefer_character_input || self.pending.is_some() {
            return;
        }
        let composing = self.draft.update(cx, |draft, cx| {
            draft.marked_text_range(window, cx).is_some()
        });
        if composing {
            return;
        }
        let text = self.draft.read(cx).value();
        if text.trim().is_empty() {
            return;
        }
        self.pending = Some(PendingDraft {
            text,
            generation: self.terminal.read(cx).generation(cx),
        });
        window.dispatch_action(Box::new(SubmitTerminalDraft(self.id)), cx);
    }

    pub(super) fn submit(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), SharedString> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        // dispatch_action is deferred. A newer focus, draft or connection
        // must not accidentally receive a submission queued earlier.
        if !self.draft.focus_handle(cx).is_focused(window)
            || self.draft.read(cx).value() != pending.text
            || self.terminal.read(cx).generation(cx) != pending.generation
        {
            return Ok(());
        }
        self.terminal
            .update(cx, |terminal, cx| terminal.submit_draft(&pending.text, cx))?;
        // No extra persistent history or undo copy of submitted commands.
        // On failure the draft remains editable and can be retried explicitly.
        self.draft
            .update(cx, |draft, cx| draft.set_value("", window, cx));
        Ok(())
    }
}

impl Focusable for ComposePane {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.draft.focus_handle(cx)
    }
}

impl Render for ComposePane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id(("terminal-compose", self.id.0))
            .test_support()
            .occlude()
            .size_full()
            .min_w_0()
            .min_h_0()
            .bg(cx.theme().background)
            .border_t_1()
            .border_color(cx.theme().border)
            .capture_key_down(cx.listener(Self::on_key_down))
            .child(
                div()
                    .id(("terminal-compose-input", self.id.0))
                    .test_support()
                    .size_full()
                    .min_h_0()
                    .min_w_0()
                    .p_2()
                    .overflow_hidden()
                    .child(
                        Textarea::new(&self.draft)
                            .h(relative(1.))
                            .aria_label(t!("terminal.compose.title"))
                            .font_family(cx.theme().mono_font_family.clone()),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::ComposePane;
    use crate::terminal::{
        RemoteTerminalId, TerminalSize, TerminalTransport, TerminalTransportCommand,
        TerminalTransportEvent, TerminalTransportFactory, TerminalView,
    };
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AppContext as _, EntityInputHandler as _, Focusable as _, InputEvent as _, KeyDownEvent,
        Keystroke, TestAppContext, px, size,
    };
    use std::sync::{Arc, mpsc};

    struct InertTransport;
    impl TerminalTransportFactory for InertTransport {
        fn create(&self) -> Box<dyn TerminalTransport> {
            Box::new(InertTransport)
        }
    }
    impl TerminalTransport for InertTransport {
        fn run(
            self: Box<Self>,
            _: TerminalSize,
            commands: mpsc::Receiver<TerminalTransportCommand>,
            _: async_channel::Sender<TerminalTransportEvent>,
        ) -> anyhow::Result<()> {
            // No real shell or network. Only the editor/dispatch path is under test.
            while let Ok(command) = commands.recv() {
                if matches!(command, TerminalTransportCommand::Shutdown) {
                    break;
                }
            }
            Ok(())
        }
    }

    #[gpui_kit::test]
    fn compose_ime_enter_is_not_a_submission(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let mut pane = None;
        let handle = cx.open_window(size(px(600.), px(300.)), |window, cx| {
            let terminal = cx.new(|cx| {
                TerminalView::new(
                    "compose-test-terminal",
                    |_| "test".into(),
                    Arc::new(InertTransport),
                    window,
                    cx,
                )
            });
            let view = cx.new(|cx| ComposePane::new(RemoteTerminalId(1), terminal, window, cx));
            pane = Some(view.clone());
            Root::new(view, window, cx)
        });
        let pane = pane.unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.activate_window();
            window.focus(&pane.focus_handle(cx), cx);
            window.render_frame(cx);
            let draft = pane.read(cx).draft.clone();
            draft.update(cx, |draft, cx| {
                draft.replace_and_mark_text_in_range(None, "中文", Some(2..2), window, cx)
            });
            window.render_frame(cx);
            window.press("enter", cx);
            assert!(
                pane.read(cx).pending.is_none(),
                "candidate-confirming Enter must not submit"
            );
            draft.update(cx, |draft, cx| draft.unmark_text(window, cx));
            window.dispatch_event(
                KeyDownEvent {
                    keystroke: Keystroke::parse("enter").unwrap(),
                    is_held: false,
                    prefer_character_input: true,
                }
                .to_platform_input(),
                cx,
            );
            assert!(
                pane.read(cx).pending.is_none(),
                "IME-preferred Enter must not submit"
            );
            window.press("enter", cx);
            assert!(
                pane.read(cx).pending.is_some(),
                "a later fresh Enter may submit committed text"
            );
        })
        .unwrap();
    }
}
