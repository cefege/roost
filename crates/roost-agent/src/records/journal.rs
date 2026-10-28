//! Durable journal entry types for agent conversations.
//! The parent module owns the surrounding conversation and settings records.
//! These types are re-exported from `records` as their canonical public paths.

use std::collections::BTreeMap;

use super::{AdvisorySeverity, Mode, PlanState};
use roost_llm::{AssistantBlock, StopReason, Usage};
use roost_protocol::wire::agent_chat::{AskQuestion, GoalState, JobState, ModelRef, TodoPhase};
use serde::{Deserialize, Serialize};

/// Durable lifecycle of a follow-up that cannot enter a running request's history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueuedInputState {
    Pending,
    Activated,
}

/// One journal entry. The journal is append-only: a plan proposal or an
/// advisory whose state changes is appended again under the same `item_id`,
/// and the latest copy wins in both the transcript and the model history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    User {
        text: String,
        /// Images and worker paths submitted with this user message.
        #[serde(default)]
        attachments: Vec<roost_protocol::wire::agent_chat::AgentChatAttachment>,
        /// An identical user steer must not be charged as a committed repeat.
        #[serde(default)]
        loop_repeat: bool,
        /// Preserves the queued submission's identity when it becomes active.
        #[serde(default)]
        item_id: Option<String>,
    },
    /// A follow-up accepted during a run but held outside its active history.
    QueuedInput {
        submission_id: String,
        text: String,
        /// Attachments remain bound to this queued submission until activation.
        #[serde(default)]
        attachments: Vec<roost_protocol::wire::agent_chat::AgentChatAttachment>,
        state: QueuedInputState,
    },
    QueuedInputWithdrawn {
        submission_id: String,
    },
    /// Durable model-dispatch boundary for user inputs. Written before the
    /// provider request so withdrawal cannot race a dispatched request.
    InputConsumed {
        through_seq: u64,
    },
    Assistant {
        item_id: String,
        blocks: Vec<AssistantBlock>,
        usage: Usage,
        model: Option<ModelRef>,
        #[serde(default)]
        metadata: Option<AssistantMetadata>,
    },
    ToolResult {
        call_id: String,
        tool_name: String,
        text: String,
        is_error: bool,
        details_json: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<Vec<roost_llm::UserContent>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_data: Option<serde_json::Value>,
    },
    Compaction {
        summary: String,
        first_kept_seq: u64,
    },
    ModeChange {
        mode: Mode,
        plan_title: Option<String>,
    },
    PlanProposal {
        item_id: String,
        title: String,
        content: String,
        state: PlanState,
    },
    Notice {
        level: String,
        title: String,
        body: String,
    },
    Advisory {
        item_id: String,
        severity: AdvisorySeverity,
        note: String,
        delivered: bool,
    },
    /// A settled background bash job, delivered back into the conversation as a
    /// follow-up. `state` is `Completed`, `Failed` (including lost to a worker
    /// disconnect) or `Cancelled`.
    BackgroundJob {
        job_id: String,
        command: String,
        state: JobState,
        is_error: bool,
        output: String,
        exit_code: Option<i32>,
    },
    /// A message from a peer conversation, journaled when admitted and read by
    /// the recipient at its next step boundary or next run.
    PeerMessage {
        from_id: String,
        from: String,
        #[serde(default)]
        broadcast: bool,
        text: String,
    },
    /// The agent's task list after a `todo` call; the latest copy is current.
    Todo {
        phases: Vec<TodoPhase>,
    },
    /// Questions the `ask` tool put to the user; re-appended with `answers`.
    Ask {
        item_id: String,
        questions: Vec<AskQuestion>,
        answers: Option<BTreeMap<String, Vec<String>>>,
    },
    /// A `/btw` side question and its answer. Never sent to the model.
    Btw {
        item_id: String,
        question: String,
        answer: Option<String>,
        error: Option<String>,
    },
    /// Every earlier entry with `seq >= to_seq` is undone: hidden from the
    /// transcript and the model history.
    Rewind {
        to_seq: u64,
    },
    /// Tokens spent outside the conversation's own turns (advisor reviews,
    /// compaction, `/btw`, judgments). Counted in usage totals only.
    ExtraUsage {
        provider: String,
        model_id: String,
        usage: Usage,
    },
    /// A subagent's shared `task` context, written once at creation.
    SubagentContext {
        context: String,
    },
    /// Durable intent for supervising accepted work across restarts.
    /// `through_seq` is the last journal sequence included in this run.
    RunIntent {
        active: bool,
        through_seq: u64,
        aborted: bool,
        /// Set when the run ended by `yield`: a subagent that yielded has
        /// finished and takes no further messages.
        #[serde(default)]
        yielded: bool,
    },
    /// The durable `/loop` snapshot; the latest entry is authoritative.
    LoopState {
        state_json: Option<String>,
    },
    /// The latest durable goal snapshot; each transition appends a replacement.
    Goal {
        state: GoalState,
    },
    /// Hidden prompt provenance for one autonomous goal continuation.
    GoalContinuation {
        goal_id: String,
        #[serde(default)]
        budget_wrap_up: bool,
        #[serde(default)]
        input_through_seq: u64,
    },
    /// The last completed goal work unit, used to detect repeated activity.
    GoalActivity {
        goal_id: String,
        fingerprint: String,
    },
}

/// Durable request and run state for one assistant response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantMetadata {
    #[serde(default)]
    pub completed: bool,
    #[serde(default)]
    pub stop_reason: Option<StopReason>,
    /// The last accepted submission represented in the model request.
    #[serde(default)]
    pub input_through_seq: u64,
    #[serde(default)]
    pub continued: bool,
    #[serde(default)]
    pub yield_reminders: u32,
    /// The eligible goal when the provider request was admitted, even if it is paused later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,
}
