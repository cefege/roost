//! Deterministic journal outages for loop and run-admission regressions.
//! The parent suite wraps the real in-memory store; successful commits remain real.
//! User, run-intent and repeat-stage failures are independently controlled.

use futures::future::BoxFuture;
use roost_agent::{
    AgentError, AgentSettings, AgentStore, ConversationRecord, Entry, InMemoryAgentStore,
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
    ) -> BoxFuture<'a, Result<(), AgentError>> {
        self.inner.save_conversation(record)
    }
    fn delete_conversation<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), AgentError>> {
        self.inner.delete_conversation(id)
    }
    fn append_entry<'a>(
        &'a self,
        id: &'a str,
        entry: &'a Entry,
        cancellation: &'a [&'a CancellationToken],
    ) -> BoxFuture<'a, Result<u64, AgentError>> {
        if matches!(entry, Entry::User { text, .. } if text == "unaccepted")
            && self.fail_next.swap(false, Ordering::SeqCst)
        {
            return Box::pin(async {
                Err(AgentError::Store("injected User append failure".into()))
            });
        }
        if matches!(entry, Entry::RunIntent { active, aborted: false, .. } if *active == self.fail_active)
            && spend_failure(&self.intent_failures)
        {
            return Box::pin(async { Err(AgentError::Store("injected run-intent outage".into())) });
        }
        if self.repeat_failures.load(Ordering::SeqCst) > 0
            && matches!(entry, Entry::LoopState { state_json: Some(json) }
                if serde_json::from_str::<serde_json::Value>(json).is_ok_and(|state| !state["pending_repeat"].is_null()))
            && spend_failure(&self.repeat_failures)
        {
            return Box::pin(async {
                Err(AgentError::Store("injected repeat-admission outage".into()))
            });
        }
        self.inner.append_entry(id, entry, cancellation)
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
}

fn spend_failure(remaining: &AtomicUsize) -> bool {
    remaining
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
            count.checked_sub(1)
        })
        .is_ok()
}
