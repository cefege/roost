//! What the pane's predictive echo does with each input answer: place a guess
//! for an admitted keystroke, acknowledge a written batch, and wipe every
//! guess (and the predicted caret) the moment a batch's fate is refused or
//! uncertain. Target-independent; read by the wasm `pane_mount::echo`. Ports
//! `reportImmediateRejection` / `watchAcceptedAdmission` of
//! `apps/web/src/components/terminal/cell-terminal-input.ts`.

use roost_client_core::InputOutcome;
use roost_client_core::terminal::input::outcome_feed::ViewAdmission;

/// One instruction to the pane's `PredictiveEchoHost`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EchoFeedback {
    /// Predict the keystroke's bytes under this sequence.
    Predict {
        /// The admitted batch's sequence.
        input_seq: u64,
    },
    /// The PTY wrote everything up to this sequence.
    Written {
        /// The acknowledged batch's sequence.
        input_seq: u64,
    },
    /// Fail closed: clear every guess and the predicted caret.
    Clear {
        /// `refused`, `rejected` or `ambiguous`, for the drop-burst log line.
        status: &'static str,
        /// Why, for the same line.
        reason: String,
    },
}

/// The echo's answer to an admission. Only controller keystrokes are
/// predicted; composed text and pastes are acknowledged but never guessed.
pub fn admission_echo_feedback(admission: &ViewAdmission, predicts: bool) -> Option<EchoFeedback> {
    match admission {
        ViewAdmission::Admitted { input_seq } => predicts.then_some(EchoFeedback::Predict {
            input_seq: *input_seq,
        }),
        ViewAdmission::Refused { reason } => Some(EchoFeedback::Clear {
            status: "refused",
            reason: reason.clone(),
        }),
    }
}

/// The echo's answer to a settled batch: only a write keeps the guesses.
pub fn outcome_echo_feedback(outcome: &InputOutcome) -> EchoFeedback {
    match outcome {
        InputOutcome::Accepted { input_seq, .. } => EchoFeedback::Written {
            input_seq: *input_seq,
        },
        InputOutcome::Rejected { reason, .. } | InputOutcome::Ambiguous { reason, .. } => {
            EchoFeedback::Clear {
                status: outcome.status_name(),
                reason: reason.clone(),
            }
        }
    }
}
