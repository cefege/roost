//! Store fault injection and one-shot gates at native semantic boundaries.
//! Tests park admission or a confirmed kernel receipt before runtime adoption;
//! all persistence remains in the real in-memory AgentStore.

#[path = "gated/control.rs"]
mod control;

use std::sync::Arc;

use futures::future::BoxFuture;
use parking_lot::Mutex;
use roost_agent::{
    AdmitOutcome, AgentError, AgentSettings, AgentStore, ChatSink, CommandAdmission,
    ConversationRecord, Entry, InMemoryAgentStore, LeaseFence,
};
use roost_protocol::wire::agent_chat::{ChatEvent, ConversationSummary, TranscriptItem};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::RecordingSink;

#[derive(Default)]
struct Holds {
    conversation_armed: bool,
    user_appends: usize,
    user_append_target: Option<usize>,
    user_append_after_commit: bool,
    completion_admission_armed: bool,
    publication_failure: Option<PublicationKind>,
    publication_read_armed: bool,
    publication_failures_consumed: usize,
    yield_receipt_armed: bool,
    yield_receipt_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicationKind {
    Assistant,
    ToolResult,
}
/// Wraps the in-memory store. Each park is one-shot: only the call it targets
/// parks, and every other call passes straight through.
pub struct GatedStore {
    inner: Arc<InMemoryAgentStore>,
    holds: Mutex<Holds>,
    conversation_parked: Notify,
    conversation_resume: Notify,
    user_append_parked: Notify,
    user_append_resume: Notify,
    completion_admission_parked: Notify,
    completion_admission_resume: Notify,
    yield_receipt_parked: Notify,
    yield_receipt_resume: Notify,
}

impl AgentStore for GatedStore {
    fn durable_batches<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<roost_agent::durable::DurableBatch>, AgentError>> {
        self.inner.durable_batches(id)
    }
    fn mint_durable_id<'a>(
        &'a self,
        id: &'a str,
        fence: &'a LeaseFence,
    ) -> BoxFuture<'a, Result<u64, AgentError>> {
        self.inner.mint_durable_id(id, fence)
    }
    fn append_durable_batch<'a>(
        &'a self,
        id: &'a str,
        expected_sequence: u64,
        writes: &'a [roost_agent::durable::StorageWrite],
        entries: &'a [Entry],
        fence: &'a LeaseFence,
    ) -> BoxFuture<'a, Result<roost_agent::durable::DurableCommit, AgentError>> {
        Box::pin(async move {
            let result = self
                .inner
                .append_durable_batch(id, expected_sequence, writes, entries, fence)
                .await?;
            let hold_yield = {
                let mut holds = self.holds.lock();
                let matches_projection = entries.iter().any(|entry| {
                    matches!(
                        (holds.publication_failure, entry),
                        (Some(PublicationKind::Assistant), Entry::Assistant { .. })
                            | (Some(PublicationKind::ToolResult), Entry::ToolResult { .. })
                    )
                });
                if matches_projection {
                    holds.publication_failure = None;
                    holds.publication_read_armed = true;
                }
                let yielded = writes.iter().any(|write| matches!(write,
                    roost_agent::durable::StorageWrite::Task { value }
                    if matches!(&value.state, rpi_durable::TaskState::Terminal {
                        outcome: rpi_durable::types::TaskOutcome::Completed { result },
                    } if result.pointer("/control/handoff").and_then(serde_json::Value::as_str).is_some())
                ));
                let armed = holds.yield_receipt_armed && yielded;
                if armed {
                    holds.yield_receipt_armed = false;
                    holds.yield_receipt_id = Some(id.to_owned());
                }
                armed
            };
            if hold_yield {
                self.yield_receipt_parked.notify_one();
                self.yield_receipt_resume.notified().await;
            }
            Ok(result)
        })
    }

    fn conversations(&self) -> BoxFuture<'_, Result<Vec<ConversationRecord>, AgentError>> {
        self.inner.conversations()
    }

    fn conversation<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<ConversationRecord>, AgentError>> {
        Box::pin(async move {
            if self.take_conversation_hold() {
                self.conversation_parked.notify_one();
                self.conversation_resume.notified().await;
            }
            self.inner.conversation(id).await
        })
    }

    fn save_conversation<'a>(
        &'a self,
        record: &'a ConversationRecord,
        expected_revision: u64,
    ) -> BoxFuture<'a, Result<u64, AgentError>> {
        self.inner.save_conversation(record, expected_revision)
    }

    fn delete_conversation<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), AgentError>> {
        self.inner.delete_conversation(id)
    }

    fn append_entry<'a>(
        &'a self,
        id: &'a str,
        entry: &'a Entry,
        cancellation: &'a [&'a CancellationToken],
        fence: Option<&'a LeaseFence>,
    ) -> BoxFuture<'a, Result<u64, AgentError>> {
        Box::pin(async move {
            let held = matches!(entry, Entry::User { .. })
                .then(|| self.take_user_append_hold())
                .flatten();
            if held == Some(false) {
                self.user_append_parked.notify_one();
                self.user_append_resume.notified().await;
            }
            let result = self
                .inner
                .append_entry(id, entry, cancellation, fence)
                .await;
            if held == Some(true) {
                self.user_append_parked.notify_one();
                self.user_append_resume.notified().await;
            }
            result
        })
    }

    fn admit_command<'a>(
        &'a self,
        id: &'a str,
        admission: CommandAdmission,
    ) -> BoxFuture<'a, Result<AdmitOutcome, AgentError>> {
        Box::pin(async move {
            let completion_held = admission.intent == roost_agent::IntentChange::Complete
                && self.take_completion_admission_hold();
            if completion_held {
                self.completion_admission_parked.notify_one();
                self.completion_admission_resume.notified().await;
            }
            let held = if completion_held {
                None
            } else {
                admission
                    .entries
                    .iter()
                    .any(|entry| matches!(entry, Entry::User { .. }))
                    .then(|| self.take_user_append_hold())
                    .flatten()
            };
            if held == Some(false) {
                self.user_append_parked.notify_one();
                self.user_append_resume.notified().await;
            }
            let result = self.inner.admit_command(id, admission).await;
            if held == Some(true) {
                self.user_append_parked.notify_one();
                self.user_append_resume.notified().await;
            }
            result
        })
    }

    fn complete_command<'a>(
        &'a self,
        id: &'a str,
        request_id: &'a str,
        result_json: &'a str,
    ) -> BoxFuture<'a, Result<(), AgentError>> {
        self.inner.complete_command(id, request_id, result_json)
    }

    fn acquire_lease<'a>(
        &'a self,
        id: &'a str,
        owner: &'a str,
        ttl_ms: u64,
    ) -> BoxFuture<'a, Result<Option<u64>, AgentError>> {
        self.inner.acquire_lease(id, owner, ttl_ms)
    }

    fn renew_lease<'a>(
        &'a self,
        id: &'a str,
        fence: &'a LeaseFence,
        ttl_ms: u64,
    ) -> BoxFuture<'a, Result<bool, AgentError>> {
        self.inner.renew_lease(id, fence, ttl_ms)
    }

    fn release_lease<'a>(
        &'a self,
        id: &'a str,
        fence: &'a LeaseFence,
    ) -> BoxFuture<'a, Result<(), AgentError>> {
        self.inner.release_lease(id, fence)
    }

    fn orphaned_runs(&self, now_ms: i64) -> BoxFuture<'_, Result<Vec<String>, AgentError>> {
        self.inner.orphaned_runs(now_ms)
    }

    fn entries<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<(u64, Entry)>, AgentError>> {
        Box::pin(async move {
            let fail = {
                let mut holds = self.holds.lock();
                if std::mem::take(&mut holds.publication_read_armed) {
                    holds.publication_failures_consumed += 1;
                    true
                } else {
                    false
                }
            };
            if fail {
                Err(AgentError::Store(
                    "injected post-settlement publication read failure".into(),
                ))
            } else {
                self.inner.entries(id).await
            }
        })
    }

    fn settings(&self) -> BoxFuture<'_, Result<AgentSettings, AgentError>> {
        self.inner.settings()
    }

    fn save_settings<'a>(
        &'a self,
        settings: &'a AgentSettings,
    ) -> BoxFuture<'a, Result<(), AgentError>> {
        self.inner.save_settings(settings)
    }

    fn advisor_state<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, AgentError>> {
        self.inner.advisor_state(id)
    }

    fn save_advisor_state<'a>(
        &'a self,
        id: &'a str,
        state_json: &'a str,
    ) -> BoxFuture<'a, Result<(), AgentError>> {
        self.inner.save_advisor_state(id, state_json)
    }

    fn set_schedule<'a>(
        &'a self,
        conversation_id: &'a str,
        row: &'a roost_agent::schedules::ScheduleRow,
    ) -> BoxFuture<'a, Result<(), AgentError>> {
        self.inner.set_schedule(conversation_id, row)
    }

    fn schedules<'a>(
        &'a self,
        conversation_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<roost_agent::schedules::ScheduleRow>, AgentError>> {
        self.inner.schedules(conversation_id)
    }

    fn due_schedules(
        &self,
        now_ms: i64,
    ) -> BoxFuture<'_, Result<Vec<(String, roost_agent::schedules::ScheduleRow)>, AgentError>> {
        self.inner.due_schedules(now_ms)
    }

    fn advance_schedule<'a>(
        &'a self,
        conversation_id: &'a str,
        id: &'a str,
        from_due_ms: i64,
    ) -> BoxFuture<'a, Result<bool, AgentError>> {
        self.inner
            .advance_schedule(conversation_id, id, from_due_ms)
    }

    fn remove_schedule<'a>(
        &'a self,
        conversation_id: &'a str,
        id: &'a str,
    ) -> BoxFuture<'a, Result<bool, AgentError>> {
        self.inner.remove_schedule(conversation_id, id)
    }
}

/// Arms the store's conversation park when the loop announces its condition
/// check, so the park lands on the check's record lookup.
pub struct ArmOnCheckSink {
    pub inner: Arc<RecordingSink>,
    pub gate: Arc<GatedStore>,
}

impl ChatSink for ArmOnCheckSink {
    fn conversation<'a>(&'a self, summary: &'a ConversationSummary) -> BoxFuture<'a, ()> {
        self.inner.conversation(summary)
    }

    fn events<'a>(&'a self, conversation_id: &'a str, events: Vec<ChatEvent>) -> BoxFuture<'a, ()> {
        if events.iter().any(is_checking_status) {
            self.gate.hold_next_conversation();
        }
        self.inner.events(conversation_id, events)
    }

    fn removed<'a>(&'a self, conversation_id: &'a str) -> BoxFuture<'a, ()> {
        self.inner.removed(conversation_id)
    }
}

fn is_checking_status(event: &ChatEvent) -> bool {
    let ChatEvent::Item {
        item: TranscriptItem::LoopStatus { status, .. },
    } = event
    else {
        return false;
    };
    status.contains("checking")
}
