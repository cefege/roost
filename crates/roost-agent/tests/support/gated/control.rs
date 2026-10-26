//! One-shot controls for store admission, publication and kernel receipt gates.
//! Behavioral tests arm a semantic boundary and await its durable settlement;
//! the parent store wrapper owns all forwarding and persisted data.

use super::{GatedStore, Holds, PublicationKind};
use parking_lot::Mutex;
use roost_agent::InMemoryAgentStore;
use std::sync::Arc;
use tokio::sync::Notify;

impl GatedStore {
    pub fn new(inner: Arc<InMemoryAgentStore>) -> Self {
        Self {
            inner,
            holds: Mutex::new(Holds::default()),
            conversation_parked: Notify::new(),
            conversation_resume: Notify::new(),
            user_append_parked: Notify::new(),
            user_append_resume: Notify::new(),
            completion_admission_parked: Notify::new(),
            completion_admission_resume: Notify::new(),
            yield_receipt_parked: Notify::new(),
            yield_receipt_resume: Notify::new(),
        }
    }

    /// Fails the first entries read after an assistant or tool result settles durably.
    pub fn fail_next_publication_read(&self, kind: PublicationKind) {
        self.holds.lock().publication_failure = Some(kind);
    }

    pub fn publication_failures_consumed(&self) -> usize {
        self.holds.lock().publication_failures_consumed
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

    pub fn hold_completion_admission(&self) {
        self.holds.lock().completion_admission_armed = true;
    }

    pub async fn completion_admission_held(&self) {
        self.completion_admission_parked.notified().await;
    }

    pub fn release_completion_admission(&self) {
        self.completion_admission_resume.notify_one();
    }

    pub fn hold_next_yield_receipt(&self) {
        let mut holds = self.holds.lock();
        holds.yield_receipt_armed = true;
        holds.yield_receipt_id = None;
    }

    pub async fn yield_receipt_held(&self) -> String {
        self.yield_receipt_parked.notified().await;
        self.holds
            .lock()
            .yield_receipt_id
            .clone()
            .expect("held yield receipt")
    }

    pub fn release_yield_receipt(&self) {
        self.yield_receipt_resume.notify_one();
    }

    pub(super) fn take_completion_admission_hold(&self) -> bool {
        std::mem::take(&mut self.holds.lock().completion_admission_armed)
    }
    pub(super) fn take_conversation_hold(&self) -> bool {
        std::mem::take(&mut self.holds.lock().conversation_armed)
    }

    pub(super) fn take_user_append_hold(&self) -> Option<bool> {
        let mut holds = self.holds.lock();
        holds.user_appends += 1;
        if holds.user_append_target != Some(holds.user_appends) {
            return None;
        }
        holds.user_append_target = None;
        Some(holds.user_append_after_commit)
    }
}
