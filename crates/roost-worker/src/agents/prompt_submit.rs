//! How an admitted agent prompt reaches the PTY: the encoded text, then the CR
//! that submits it, as two separately acknowledged keeper batches on the input
//! lane `super::prompt_control` already holds. The keeper's acknowledgements
//! are the only truth about what landed. Called by `super::prompt_control`;
//! the keeper batches themselves go through `session::input_write`. Ports
//! `apps/worker/src/agents/agent-prompt-submit.ts`.

use std::time::Duration;

use roost_protocol::terminal_input::CR_BYTES;

use crate::session::input_write::{HeldInputLane, WorkerInputResult};
use crate::session::keeper_channels::KeeperInputResult;

/// Gap between the prompt text and its CR. Agents that debounce bracketed-paste
/// assembly see a fused `…ESC[201~\r` as one burst and keep the text as an
/// unsubmitted draft, so the CR only goes out once the paste has settled.
pub const PROMPT_SUBMIT_DELAY: Duration = Duration::from_millis(300);

/// What the keeper said about one batch, before it becomes a prompt outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeeperWriteOutcome {
    Acked {
        written_bytes: u32,
    },
    Unwritten {
        reason: &'static str,
    },
    Uncertain {
        written_bytes: u32,
        reason: &'static str,
    },
}

/// Write the prompt text, then the CR alone once the paste has settled. The
/// caller must still hold the input lane across both writes: another writer's
/// bytes landing between them would be submitted along with the prompt.
///
/// `Accepted` means BOTH batches were acknowledged. An acknowledged text whose
/// CR did not land is `Ambiguous` and never a rejection, because those bytes
/// are already on the PTY and the agent may hold an unsubmitted draft.
pub async fn submit_agent_prompt(
    lane: &HeldInputLane,
    text: Vec<u8>,
    budget_admits_submit: impl Fn() -> bool,
) -> WorkerInputResult {
    let text_bytes = match write_keeper_batch(lane, text).await {
        KeeperWriteOutcome::Acked { written_bytes } => written_bytes,
        KeeperWriteOutcome::Unwritten { reason } => {
            tracing::info!(
                channel_id = lane.channel_id(),
                reason,
                "the agent prompt text was not written"
            );
            return WorkerInputResult::Rejected {
                reason: reason.to_owned(),
            };
        }
        KeeperWriteOutcome::Uncertain {
            written_bytes,
            reason,
        } => {
            return ambiguous(lane, written_bytes, reason);
        }
    };
    tracing::debug!(
        channel_id = lane.channel_id(),
        written_bytes = text_bytes,
        "the agent prompt text is on the PTY; the submit waits for the paste to settle"
    );
    tokio::time::sleep(PROMPT_SUBMIT_DELAY).await;
    if !budget_admits_submit() {
        return ambiguous(lane, text_bytes, "agent prompt submit was not written");
    }
    match write_keeper_batch(lane, CR_BYTES.to_vec()).await {
        KeeperWriteOutcome::Acked { written_bytes } => {
            let written_bytes = text_bytes.saturating_add(written_bytes);
            tracing::info!(
                channel_id = lane.channel_id(),
                written_bytes,
                "the agent prompt was submitted"
            );
            WorkerInputResult::Accepted { written_bytes }
        }
        KeeperWriteOutcome::Unwritten { .. } => {
            ambiguous(lane, text_bytes, "keeper did not submit the agent prompt")
        }
        KeeperWriteOutcome::Uncertain {
            written_bytes,
            reason,
        } => ambiguous(lane, text_bytes.saturating_add(written_bytes), reason),
    }
}

/// One keeper batch on the held lane, mapped onto the prompt's truth model.
/// `Unwritten` is only ever a batch the keeper provably did not write.
async fn write_keeper_batch(lane: &HeldInputLane, bytes: Vec<u8>) -> KeeperWriteOutcome {
    let expected = bytes.len();
    let Some(command) = lane.begin_input(bytes).await else {
        return KeeperWriteOutcome::Uncertain {
            written_bytes: 0,
            reason: "keeper input admission failed",
        };
    };
    if command.admission.is_err() {
        return KeeperWriteOutcome::Unwritten {
            reason: "keeper did not admit the agent prompt",
        };
    }
    match command.result.await {
        KeeperInputResult::Ack { written }
            if usize::try_from(written).is_ok_and(|written| written == expected) =>
        {
            KeeperWriteOutcome::Acked {
                written_bytes: written,
            }
        }
        KeeperInputResult::Ack { written } => KeeperWriteOutcome::Uncertain {
            written_bytes: bounded_written_bytes(Some(written), expected),
            reason: "keeper acknowledged an incomplete input batch",
        },
        KeeperInputResult::Reject { .. } => KeeperWriteOutcome::Unwritten {
            reason: "keeper rejected the agent prompt",
        },
        KeeperInputResult::Ambiguous { written, .. } => KeeperWriteOutcome::Uncertain {
            written_bytes: bounded_written_bytes(written, expected),
            reason: "keeper agent prompt outcome is ambiguous",
        },
    }
}

/// A keeper count the batch could not have produced is no count at all.
fn bounded_written_bytes(value: Option<u32>, expected_bytes: usize) -> u32 {
    value
        .filter(|written| usize::try_from(*written).is_ok_and(|written| written <= expected_bytes))
        .unwrap_or(0)
}

fn ambiguous(lane: &HeldInputLane, written_bytes: u32, reason: &'static str) -> WorkerInputResult {
    tracing::warn!(
        channel_id = lane.channel_id(),
        written_bytes,
        reason,
        "the agent prompt outcome is ambiguous"
    );
    WorkerInputResult::Ambiguous {
        written_bytes,
        reason: reason.to_owned(),
    }
}
