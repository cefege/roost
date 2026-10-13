//! Store and sink seams that park a loop at a chosen point. A test parks the
//! loop's condition-record read or its repeat's prompt append, delivers `/loop
//! off` or an abort while it is parked, then releases it.

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
}

impl GatedStore {
    pub fn new(inner: Arc<InMemoryAgentStore>) -> Self {
        Self {
            inner,
            holds: Mutex::new(Holds::default()),
            conversation_parked: Notify::new(),
            conversation_resume: Notify::new(),
            user_append_parked: Notify::new(),
            user_append_resume: Notify::new(),
        }
    }

    /// Parks the next conversation read. Arm it when the loop announces its
    /// condition check: the read that follows is the check's record lookup.
    pub fn hold_next_conversation(&self) {
        self.holds.lock().conversation_armed = true;
    }

    pub async fn conversation_held(&self) {
        self.conversation_parked.notified().await;
    }

    pub fn release_conversation(&self) {
        self.conversation_resume.notify_one();
    }

    /// Parks the `nth` user entry the store appends, counting from its first.
    pub fn hold_user_append(&self, nth: usize) {
        let mut holds = self.holds.lock();
        holds.user_append_target = Some(nth);
        holds.user_append_after_commit = false;
    }

    pub fn hold_committed_user_append(&self, nth: usize) {
        let mut holds = self.holds.lock();
        holds.user_append_target = Some(nth);
        holds.user_append_after_commit = true;
    }

    pub async fn user_append_held(&self) {
        self.user_append_parked.notified().await;
    }

    pub fn release_user_append(&self) {
        self.user_append_resume.notify_one();
    }

    fn take_conversation_hold(&self) -> bool {
        std::mem::take(&mut self.holds.lock().conversation_armed)
    }

    fn take_user_append_hold(&self) -> Option<bool> {
        let mut holds = self.holds.lock();
        holds.user_appends += 1;
        if holds.user_append_target != Some(holds.user_appends) {
            return None;
        }
        holds.user_append_target = None;
        Some(holds.user_append_after_commit)
    }
}

impl AgentStore for GatedStore {
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
            let held = admission
                .entries
                .iter()
                .any(|entry| matches!(entry, Entry::User { .. }))
                .then(|| self.take_user_append_hold())
                .flatten();
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
