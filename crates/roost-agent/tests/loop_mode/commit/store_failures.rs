//! Deterministic journal outages for loop and run-admission regressions.
//! The parent suite wraps the real in-memory store; successful commits remain real.
//! User, run-intent and repeat-stage failures are independently controlled.

use futures::future::BoxFuture;
use roost_agent::traits::IntentChange;
use roost_agent::{
    AdmitOutcome, AgentError, AgentSettings, AgentStore, CommandAdmission, ConversationRecord,
    Entry, InMemoryAgentStore, LeaseFence,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio_util::sync::CancellationToken;

pub(super) struct FailAppendStore {
    pub(super) inner: Arc<InMemoryAgentStore>,
    pub(super) fail_next: AtomicBool,
    pub(super) fail_active: bool,
    pub(super) intent_failures: AtomicUsize,
    pub(super) repeat_failures: AtomicUsize,
}

impl AgentStore for FailAppendStore {
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
        self.inner
            .append_durable_batch(id, expected_sequence, writes, entries, fence)
    }

    fn conversations(&self) -> BoxFuture<'_, Result<Vec<ConversationRecord>, AgentError>> {
        self.inner.conversations()
    }
    fn conversation<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<ConversationRecord>, AgentError>> {
        self.inner.conversation(id)
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
    fn admit_command<'a>(
        &'a self,
        id: &'a str,
        admission: CommandAdmission,
    ) -> BoxFuture<'a, Result<AdmitOutcome, AgentError>> {
        if admission
            .entries
            .iter()
            .any(|entry| matches!(entry, Entry::User { text, .. } if text == "unaccepted"))
            && self.fail_next.swap(false, Ordering::SeqCst)
        {
            return Box::pin(async {
                Err(AgentError::Store("injected User append failure".into()))
            });
        }
        let fail_intent = match admission.intent {
            IntentChange::Activate | IntentChange::RestartIfCurrent => {
                self.fail_active && admission.entries.is_empty()
            }
            IntentChange::Complete => !self.fail_active,
            _ => false,
        };
        if fail_intent && spend_failure(&self.intent_failures) {
            return Box::pin(async { Err(AgentError::Store("injected run-intent outage".into())) });
        }
        self.inner.admit_command(id, admission)
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
    fn append_entry<'a>(
        &'a self,
        id: &'a str,
        entry: &'a Entry,
        cancellation: &'a [&'a CancellationToken],
        fence: Option<&'a LeaseFence>,
    ) -> BoxFuture<'a, Result<u64, AgentError>> {
        if self.repeat_failures.load(Ordering::SeqCst) > 0
            && matches!(entry, Entry::LoopState { state_json: Some(json) }
                if serde_json::from_str::<serde_json::Value>(json).is_ok_and(|state| !state["pending_repeat"].is_null()))
            && spend_failure(&self.repeat_failures)
        {
            return Box::pin(async {
                Err(AgentError::Store("injected repeat-admission outage".into()))
            });
        }
        self.inner.append_entry(id, entry, cancellation, fence)
    }
    fn entries<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<(u64, Entry)>, AgentError>> {
        self.inner.entries(id)
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

fn spend_failure(remaining: &AtomicUsize) -> bool {
    remaining
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
            count.checked_sub(1)
        })
        .is_ok()
}
