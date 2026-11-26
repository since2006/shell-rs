//! AI 助手's failures and notices in words: the core types them.

use gpui_kit::SharedString;
use shellrs_assistant::{ProviderError, ProviderErrorKind, TurnEnd};

use crate::host::AiProviderDraftError;
use crate::i18n::t;
use crate::terminal::{BusyReason, RunFailure};

/// What a model service's failure means for the user, with what the
/// service said after it.
pub fn provider_error(error: &ProviderError) -> SharedString {
    let what = match error.kind {
        ProviderErrorKind::KeyRejected => t!("assistant.error.key_rejected"),
        ProviderErrorKind::NotFound => t!("assistant.error.not_found"),
        ProviderErrorKind::QuotaExhausted => t!("assistant.error.quota"),
        ProviderErrorKind::RateLimited => t!("assistant.error.rate_limited"),
        ProviderErrorKind::Overloaded => t!("assistant.error.overloaded"),
        ProviderErrorKind::ContextTooLong => t!("assistant.error.context"),
        ProviderErrorKind::BadRequest => t!("assistant.error.bad_request"),
        ProviderErrorKind::ServerError => t!("assistant.error.server"),
        ProviderErrorKind::Network => t!("assistant.error.network"),
        ProviderErrorKind::Timeout => t!("assistant.error.timeout"),
        ProviderErrorKind::Protocol => t!("assistant.error.protocol"),
    };
    match &error.message {
        Some(message) => t!("assistant.error.with_detail", what = what, detail = message),
        None => what,
    }
}

/// What stands in the way of saving a model service.
pub fn draft_error(error: AiProviderDraftError) -> SharedString {
    match error {
        AiProviderDraftError::Name => t!("assistant.dialog.error.name"),
        AiProviderDraftError::BaseUrl => t!("assistant.dialog.error.base_url"),
        AiProviderDraftError::Model => t!("assistant.dialog.error.model"),
    }
}

/// The note a turn that did not simply end leaves under it.
pub fn turn_end(end: TurnEnd) -> Option<SharedString> {
    Some(match end {
        TurnEnd::Done => return None,
        TurnEnd::Refused => t!("assistant.turn.refused"),
        TurnEnd::Stopped => t!("assistant.turn.stopped"),
        TurnEnd::StepLimit => t!("assistant.turn.step_limit"),
        TurnEnd::MaxTokens => t!("assistant.turn.max_tokens"),
        TurnEnd::ContextFull => t!("assistant.turn.context_full"),
    })
}

/// Why a command could not run in the terminal.
pub fn run_failure(failure: &RunFailure) -> SharedString {
    match failure {
        RunFailure::HostNotFound | RunFailure::NoTerminal => t!("assistant.run.no_terminal"),
        RunFailure::NotConnected => t!("assistant.run.not_connected"),
        RunFailure::UnsupportedShell(shell) if shell.is_empty() => {
            t!("assistant.run.unknown_shell")
        }
        RunFailure::UnsupportedShell(shell) => t!("assistant.run.unsupported_shell", shell = shell),
        RunFailure::TooLong { limit } => t!("assistant.run.too_long", limit = limit),
        RunFailure::Busy(BusyReason::FullScreen) => t!("assistant.run.full_screen"),
        RunFailure::Busy(BusyReason::AnotherRun) => t!("assistant.run.another_run"),
        RunFailure::Busy(BusyReason::Transfer) => t!("assistant.run.transfer"),
        RunFailure::Busy(BusyReason::NoAnswer | BusyReason::NotStarted) => {
            t!("assistant.run.no_answer")
        }
        RunFailure::Disconnected => t!("assistant.run.disconnected"),
    }
}
