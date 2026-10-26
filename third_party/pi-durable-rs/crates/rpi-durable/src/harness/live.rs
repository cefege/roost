//! Built-in live conversation state (port of `packages/durable/src/harness/live.ts`
//! @ `abe508e1b`).
//!
//! The `pi.live` document carries run control (`run`), the current generation's
//! presentation, the current tool round, and live compaction statuses. Its
//! checkpoint predicate keeps a complete base whenever nothing runs, so the delta
//! chain spans at most one generation or one overlapping tool round.

use std::sync::{Arc, LazyLock};

use rpi_ai::types::{AssistantMessage, Message, StopReason};
use serde::{Deserialize, Serialize};

use crate::chord::json::{JsonObject, JsonValue};
use crate::documents::{DocDefinition, DocScope, DocToken, define_doc};
use crate::entries::ASSISTANT_ENTRY;
use crate::harness::types::{CompactionReason, ToolDiagnostic, read_state, write_state};
use crate::harness::usage::{UsageBucket, record_usage};
use crate::ids::{EntryId, SubmissionId, TaskId};
use crate::session::{
    DocDraft, DocOwner, EntryDraft, SessionResult, SubmissionSettlement, Transaction,
};
use crate::types::{DocumentFork, DocumentHistory, TaskOutcomeError, TaskRecord};

/// Built-in task kind that can own `pi.live.run`.
pub const RUN_TASK_KINDS: [&str; 1] = ["pi.generation"];
/// Built-in tool task kind.
pub const TOOL_TASK_KIND: &str = "pi.tool";
/// Built-in compaction task kind.
pub const COMPACTION_TASK_KIND: &str = "pi.compaction";

/// Presentation of one tool call of the current round.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSlot {
    /// Provider call ID.
    pub call_id: String,
    /// Tool name.
    pub name: String,
    /// Owning tool task; absent for a call not started yet and for an unoffered call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    /// Presentation status.
    pub status: ToolSlotStatus,
    /// Retained running output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Bytes the bounds dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped_bytes: Option<u64>,
    /// Lines the bounds dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped_lines: Option<u64>,
    /// Last `details()` value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
    /// Diagnostics recorded through `api.diagnostic()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<ToolDiagnostic>>,
    /// Result entry once done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<EntryId>,
}

/// Status of one [`ToolSlot`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolSlotStatus {
    /// Not started yet.
    Pending,
    /// Running.
    Running,
    /// Terminal.
    #[default]
    Done,
}

/// Presentation of a tool call made by another running tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedToolSlot {
    /// Stable child call ID, `parentCallId/key`.
    pub call_id: String,
    /// Parent tool call identity.
    pub parent_call_id: String,
    /// Parent tool task.
    pub parent_task_id: TaskId,
    /// Nested tool name.
    pub name: String,
    /// Child task owning this call.
    pub task_id: TaskId,
    /// Validated call arguments.
    pub arguments: JsonObject,
    /// Presentation status.
    pub status: ToolSlotStatus,
    /// Retained running output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Bytes the bounds dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped_bytes: Option<u64>,
    /// Lines the bounds dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped_lines: Option<u64>,
    /// Last `details()` value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
    /// Complete durable child receipt retained after its task-owned document retires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<crate::harness::types::NestedToolExecutionResult>,
    /// Diagnostics reported through the tool API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<ToolDiagnostic>>,
}

/// Remove a nested slot's running report.
pub fn clear_nested_progress(slot: &mut NestedToolSlot) {
    slot.output = None;
    slot.dropped_bytes = None;
    slot.dropped_lines = None;
    slot.details = None;
    slot.diagnostics = None;
}

/// Mark a nested call's presentation terminal.
pub fn finish_nested_slot(slot: &mut NestedToolSlot) {
    slot.status = ToolSlotStatus::Done;
    clear_nested_progress(slot);
}
/// Durable backoff before the next attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryStatus {
    /// Wall-clock time of the next attempt.
    pub at: f64,
    /// Failure text.
    pub error: String,
}

/// Presentation of one live compaction task (spec §8.7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionStatus {
    /// Compaction task.
    pub task_id: TaskId,
    /// Why it runs.
    pub reason: CompactionReason,
    /// Whether a generation waits for it.
    pub blocking: bool,
    /// Attempt number.
    pub attempt: u64,
    /// Durable backoff before the next summarization attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryStatus>,
}

/// Run control and presentation of the current generation and tool round.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveState {
    /// Run control: the task that settles the run's inputs, and those inputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<LiveRun>,
    /// Presentation of the current generation attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<LiveGeneration>,
    /// The current tool round in call order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolSlot>>,
    /// Nested calls made by running tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nested_tools: Option<Vec<NestedToolSlot>>,
    /// Live compaction tasks in task ID order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compactions: Option<Vec<CompactionStatus>>,
}

/// Run control of one busy conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveRun {
    /// Task that settles the run's inputs.
    pub task_id: TaskId,
    /// Inputs the run consumes.
    pub inputs: Vec<SubmissionId>,
}

/// Presentation of the current generation attempt.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveGeneration {
    /// Attempt number.
    pub attempt: u64,
    /// Committed throttled partial of the in-flight response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<JsonValue>,
    /// Durable backoff before the next attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryStatus>,
    /// Provider-side deferred response being polled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred: Option<DeferredStatus>,
}

/// Provider-side deferred poll state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeferredStatus {
    /// Wall-clock time of the next poll.
    pub poll_at: f64,
}

fn initial() -> JsonObject {
    match serde_json::to_value(LiveState::default()) {
        Ok(JsonValue::Object(object)) => object,
        // Invariant: a `LiveState` always serializes to a JSON object.
        _ => JsonObject::new(),
    }
}

/// Built-in live document (`pi.live`), conversation scope, history latest.
pub static LIVE_DOC: LazyLock<DocToken> = LazyLock::new(|| {
    define_doc(DocDefinition {
        kind: "pi.live".to_string(),
        version: 1,
        scope: DocScope::Conversation,
        history: Some(DocumentHistory::Latest),
        fork: Some(DocumentFork::Initial),
        family: false,
        initial: Arc::new(|_seed| initial()),
        migrate: None,
        // A complete base whenever nothing runs (spec §8.2): no generation and no
        // running tool slot.
        checkpoint_when: Some(Arc::new(|value, _ops, _revision| {
            let no_generation = value.get("generation").is_none();
            let no_running = ["tools", "nestedTools"].iter().all(|field| {
                value
                    .get(*field)
                    .and_then(JsonValue::as_array)
                    .map(|slots| {
                        slots.iter().all(|slot| {
                            slot.get("status").and_then(JsonValue::as_str) != Some("running")
                        })
                    })
                    .unwrap_or(true)
            });
            no_generation && no_running
        })),
    })
});

fn conversation_owner(record: &TaskRecord) -> DocOwner<'_> {
    DocOwner::Conversation(record.conversation_id)
}

/// End the run owned by `task_id`: settle each of its inputs and remove `run`.
/// Always removes `generation` and `tools`.
pub async fn end_run(
    tx: &mut Transaction,
    live: &DocDraft,
    task_id: TaskId,
    settlement: SubmissionSettlement,
) -> SessionResult<()> {
    let mut state: LiveState = read_state(live)?;
    if state.run.as_ref().map(|run| run.task_id) == Some(task_id)
        && let Some(run) = state.run.take()
    {
        for id in run.inputs {
            tx.settle_submission(id, settlement.clone())?;
        }
    }
    state.generation = None;
    state.tools = None;
    state.nested_tools = None;
    write_state(live, &state)
}

/// Add the status of a compaction task; statuses stay in task ID order.
pub fn add_compaction_status(live: &DocDraft, status: CompactionStatus) -> SessionResult<()> {
    let mut state: LiveState = read_state(live)?;
    let mut list = state.compactions.take().unwrap_or_default();
    list.push(status);
    list.sort_by_key(|status| status.task_id);
    state.compactions = Some(list);
    write_state(live, &state)
}

/// The status of compaction task `task_id`, if listed.
pub fn compaction_status(
    live: &DocDraft,
    task_id: TaskId,
) -> SessionResult<Option<CompactionStatus>> {
    let state: LiveState = read_state(live)?;
    Ok(state
        .compactions
        .unwrap_or_default()
        .into_iter()
        .find(|status| status.task_id == task_id))
}

/// Remove the status of compaction task `task_id`, and the list once empty.
pub fn remove_compaction_status(live: &DocDraft, task_id: TaskId) -> SessionResult<()> {
    let mut state: LiveState = read_state(live)?;
    let Some(list) = state.compactions.as_mut() else {
        return Ok(());
    };
    list.retain(|status| status.task_id != task_id);
    if list.is_empty() {
        state.compactions = None;
    }
    write_state(live, &state)
}

/// The slot of tool task `task_id` in the current round, if the round lists it.
pub fn tool_slot(live: &DocDraft, task_id: TaskId) -> SessionResult<Option<ToolSlot>> {
    let state: LiveState = read_state(live)?;
    Ok(state
        .tools
        .unwrap_or_default()
        .into_iter()
        .find(|slot| slot.task_id == Some(task_id)))
}

/// The nested slot of tool task `task_id`, if progress was enabled.
pub fn nested_tool_slot(live: &DocDraft, task_id: TaskId) -> SessionResult<Option<NestedToolSlot>> {
    let state: LiveState = read_state(live)?;
    Ok(state
        .nested_tools
        .unwrap_or_default()
        .into_iter()
        .find(|slot| slot.task_id == task_id))
}

/// Mark a slot done: the result entry, if any, now carries its running output.
pub fn finish_slot(slot: &mut ToolSlot, entry: Option<EntryId>) {
    slot.status = ToolSlotStatus::Done;
    if entry.is_some() {
        slot.entry = entry;
    }
    clear_progress(slot);
}

/// Remove what a tool published while running; its result entry or a rerun
/// replaces it.
pub fn clear_progress(slot: &mut ToolSlot) {
    slot.output = None;
    slot.dropped_bytes = None;
    slot.dropped_lines = None;
    slot.details = None;
    slot.diagnostics = None;
}

/// Terminal outcome the scheduler writes itself.
#[derive(Debug, Clone, PartialEq)]
pub enum SchedulerOutcome {
    /// A contract failure detected by the run loop.
    Faulted {
        /// Failure snapshot.
        error: TaskOutcomeError,
    },
    /// A task no registered definition can take.
    Orphaned {
        /// Why the task is orphaned.
        reason: String,
    },
}

/// Harness cleanup for a terminal outcome the scheduler writes itself (`faulted`
/// or `orphaned`).
///
/// A run task ends its run (after turning a committed partial into an aborted
/// assistant entry); a tool task's slot is marked done without an entry; a
/// compaction task's status is removed. Other kinds are ignored so this never
/// creates `pi.live` elsewhere.
pub async fn settle_scheduler_outcome(
    tx: &mut Transaction,
    record: &TaskRecord,
    outcome: &SchedulerOutcome,
) -> SessionResult<()> {
    if record.kind == TOOL_TASK_KIND {
        let live = tx.doc(&LIVE_DOC, conversation_owner(record), None).await?;
        let mut state: LiveState = read_state(&live)?;
        if let Some(slot) = state
            .nested_tools
            .as_mut()
            .and_then(|slots| slots.iter_mut().find(|slot| slot.task_id == record.id))
        {
            finish_nested_slot(slot);
            write_state(&live, &state)?;
        } else if let Some(tools) = state.tools.as_mut()
            && let Some(slot) = tools
                .iter_mut()
                .find(|slot| slot.task_id == Some(record.id))
        {
            finish_slot(slot, None);
            write_state(&live, &state)?;
        }
        return Ok(());
    }
    if record.kind == COMPACTION_TASK_KIND {
        let live = tx.doc(&LIVE_DOC, conversation_owner(record), None).await?;
        return remove_compaction_status(&live, record.id);
    }
    if !RUN_TASK_KINDS.contains(&record.kind.as_str()) {
        return Ok(());
    }
    let live = tx.doc(&LIVE_DOC, conversation_owner(record), None).await?;
    let state: LiveState = read_state(&live)?;
    if state.run.as_ref().map(|run| run.task_id) != Some(record.id) {
        return Ok(());
    }
    convert_partial(tx, &live).await?;
    let settlement = match outcome {
        SchedulerOutcome::Faulted { error } => SubmissionSettlement::Unanswered {
            reason: "faulted".to_string(),
            detail: Some(JsonValue::String(error.message.clone())),
        },
        SchedulerOutcome::Orphaned { reason } => SubmissionSettlement::Unanswered {
            reason: reason.clone(),
            detail: None,
        },
    };
    end_run(tx, &live, record.id, settlement).await
}

/// Append a committed partial left by an interrupted, aborted, faulted, or
/// orphaned attempt as an aborted assistant entry.
///
/// Upstream defines this in `generation.ts`; the Rust port keeps it here so the
/// scheduler-outcome cleanup and the generation abort handler share one
/// implementation.
pub async fn convert_partial(tx: &mut Transaction, live: &DocDraft) -> SessionResult<()> {
    let state: LiveState = read_state(live)?;
    let Some(partial) = state.generation.and_then(|generation| generation.message) else {
        return Ok(());
    };
    let mut message: AssistantMessage = match serde_json::from_value(partial) {
        Ok(message) => message,
        Err(error) => return Err(crate::session::SessionError::message(error.to_string())),
    };
    message.stop_reason = StopReason::Aborted;
    let conversation_id = conversation_of(live)?;
    record_usage(
        tx,
        conversation_id,
        UsageBucket::Models,
        &format!("{}/{}", message.provider, message.model),
        &message.usage,
    )
    .await?;
    let mut draft = EntryDraft::new(ASSISTANT_ENTRY);
    draft.model = Some(vec![Message::Assistant(message)]);
    tx.append_entry(conversation_id, draft).await?;
    Ok(())
}

/// The conversation a draft belongs to, decoded from its cache address identity.
fn conversation_of(live: &DocDraft) -> SessionResult<crate::ids::ConversationId> {
    // Address ids are `["kind","conversation",<id>,null]`; the conversation id is
    // the third element. Parsing the address keeps this helper independent of the
    // caller's scope.
    let parsed: JsonValue = serde_json::from_str(live.address_id())
        .map_err(|error| crate::session::SessionError::message(error.to_string()))?;
    let id = parsed
        .as_array()
        .and_then(|parts| parts.get(2))
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| {
            crate::session::SessionError::message("live document has no conversation")
        })?;
    Ok(crate::ids::id_from_number::<crate::ids::Conversation>(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_slot_clears_progress_and_keeps_entry_when_absent() {
        let mut slot = ToolSlot {
            call_id: "c".to_string(),
            name: "read".to_string(),
            status: ToolSlotStatus::Running,
            output: Some("out".to_string()),
            entry: Some(crate::ids::id_from_number::<crate::ids::Entry>(9)),
            ..ToolSlot::default()
        };
        finish_slot(&mut slot, None);
        assert_eq!(slot.status, ToolSlotStatus::Done);
        assert_eq!(slot.output, None);
        assert!(slot.entry.is_some());
    }

    #[test]
    fn live_state_default_round_trips() {
        let value = serde_json::to_value(LiveState::default()).unwrap();
        assert_eq!(value, serde_json::json!({}));
    }

    #[tokio::test]
    async fn end_run_settles_inputs_and_clears_presentation() {
        use crate::ids::{Task, id_from_number};
        use crate::session::{ConversationOwnership, SubmissionCreate, commit_fn, create_session};
        use crate::storage::MemoryStorage;

        let session = create_session(Arc::new(MemoryStorage::new()), None);
        let context = crate::chord::context::BACKGROUND_CONTEXT.clone();
        let task_id = id_from_number::<Task>(7);
        let (conversation, submission) = session
            .commit(
                commit_fn(move |tx| {
                    Box::pin(async move {
                        let record = tx
                            .create_conversation(ConversationOwnership::Ownerless)
                            .await?;
                        let submission = tx
                            .create_submission(SubmissionCreate::Input {
                                conversation_id: record.id,
                                request_id: None,
                            })
                            .await?;
                        let live = tx
                            .doc(&LIVE_DOC, DocOwner::Conversation(record.id), None)
                            .await?;
                        write_state(
                            &live,
                            &LiveState {
                                run: Some(LiveRun {
                                    task_id,
                                    inputs: vec![submission.id()],
                                }),
                                generation: Some(LiveGeneration {
                                    attempt: 1,
                                    message: None,
                                    retry: None,
                                    deferred: None,
                                }),
                                ..LiveState::default()
                            },
                        )?;
                        end_run(
                            tx,
                            &live,
                            task_id,
                            SubmissionSettlement::Unanswered {
                                reason: "done".to_string(),
                                detail: None,
                            },
                        )
                        .await?;
                        Ok((record.id, submission.id()))
                    })
                }),
                &context,
            )
            .await
            .unwrap();
        let record = session
            .storage()
            .submission(submission, &context)
            .await
            .unwrap()
            .expect("submission");
        assert_eq!(record.status(), "unanswered");
        let live = session
            .snapshot(&LIVE_DOC, DocOwner::Conversation(conversation), &context)
            .await
            .unwrap()
            .expect("live");
        let state: LiveState = serde_json::from_value(JsonValue::Object(live)).unwrap();
        assert!(state.run.is_none());
        assert!(state.generation.is_none());
    }
}
