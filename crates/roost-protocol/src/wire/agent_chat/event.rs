//! Agent chat events that mutate transcript state.
//! Events are folded into the shared transcript contract by `fold`.

use serde::{Deserialize, Serialize};

use super::model::{
    AgentRunState, ModelRef, QueuedMessage, ScheduledMessage, Transcript, TranscriptBlock,
    TranscriptItem,
};
use super::omp::{OmpInteraction, OmpSessionState, OmpSlashCommand};
use super::usage::UsageTotals;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatEvent {
    Reset {
        transcript: Box<Transcript>,
    },
    Item {
        item: TranscriptItem,
    },
    TextDelta {
        item_id: String,
        block: usize,
        delta: String,
    },
    ThinkingDelta {
        item_id: String,
        block: usize,
        delta: String,
    },
    BlockSet {
        item_id: String,
        block: usize,
        value: TranscriptBlock,
    },
    ToolStarted {
        item_id: String,
    },
    ToolOutput {
        item_id: String,
        trim_start: Option<u64>,
        append: Option<String>,
        set: Option<String>,
    },
    RunState {
        run_state: AgentRunState,
        error: Option<String>,
    },
    Agent {
        model: Option<ModelRef>,
        thinking_level: Option<String>,
        mode: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        omp_session: Option<OmpSessionState>,
    },
    Usage {
        usage: UsageTotals,
    },
    /// A message joined the queue; its `Item` follows when the run reads it.
    Queued {
        message: QueuedMessage,
    },
    /// A queued submission was withdrawn before model dispatch.
    QueueRemoved {
        submission_id: String,
    },
    /// Queued inputs crossed the durable model-dispatch boundary.
    QueueDelivered {
        submission_ids: Vec<String>,
    },
    /// A schedule was created or advanced; upserts by `schedule.id`.
    ScheduleSet {
        schedule: ScheduledMessage,
    },
    /// A schedule was cancelled or its last one-shot fired.
    ScheduleRemoved {
        id: String,
    },
    /// Replaces the active OMP slash-command catalog.
    OmpCommands {
        commands: Vec<OmpSlashCommand>,
    },
    /// Inserts or updates an ACP interaction awaiting a decision.
    OmpInteraction {
        interaction: OmpInteraction,
    },
    /// Removes an interaction after its original responder is completed.
    OmpInteractionResolved {
        id: String,
    },
    OmpHistoryReset,
    OmpUiReset {
        #[serde(default)]
        transport_id: Option<String>,
    },
    /// A semantic frame from the actual OMP command and session UI.
    OmpUi {
        message: serde_json::Value,
    },
    OmpUiError {
        error: String,
    },
}
