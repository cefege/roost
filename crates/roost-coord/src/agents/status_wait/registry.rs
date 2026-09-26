//! The coordinator's wait registry: the table of outstanding waits, the waiter
//! one caller holds, and the single evaluation every retained-status change
//! runs.
//!
//! Owned by `super`: the vocabulary in `mod.rs` says what a wait is, and this
//! says how one is admitted, released and accounted for.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use roost_observability::LogFields;
use roost_protocol::wire::agent_status::agent_status_identity;
use roost_protocol::wire::{AgentRuntimeState, AgentStatusFields, SessionId};
use tokio::sync::oneshot;

use super::{
    AGENT_STATUS_WAIT_MAX_GLOBAL, AGENT_STATUS_WAIT_MAX_PER_SESSION, AgentStatusWaitCapacity,
    AgentStatusWaitError, AgentStatusWaitOutcome, AgentStatusWaitRequest, AgentStatusWaitView,
};

/// What a waiter receives: an outcome, or the refusal that ended it early.
type WaitSettlement = Result<AgentStatusWaitOutcome, AgentStatusWaitError>;

/// One registered wait, as the registry holds it.
struct WaiterEntry {
    request: AgentStatusWaitRequest,
    sender: Mutex<Option<oneshot::Sender<WaitSettlement>>>,
}

#[derive(Default)]
struct WaitTables {
    by_session: HashMap<SessionId, Vec<Arc<WaiterEntry>>>,
    total: usize,
}

/// The coordinator's wait registry: one table, one owner, bounded twice.
#[derive(Clone, Default)]
pub struct AgentStatusWaitRegistry {
    tables: Arc<Mutex<WaitTables>>,
}

impl std::fmt::Debug for AgentStatusWaitRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentStatusWaitRegistry")
            .field("waiters", &self.waiter_count())
            .finish()
    }
}

impl AgentStatusWaitRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Admit a wait, or refuse it by which bound ran out.
    ///
    /// The waiter is registered BEFORE the caller evaluates it, so a status
    /// that changes between the client's last read and this call is not missed.
    pub fn register(
        &self,
        request: AgentStatusWaitRequest,
    ) -> Result<AgentStatusWaiter, AgentStatusWaitError> {
        let mut tables = self.lock();
        let held = tables
            .by_session
            .get(&request.session_id)
            .map_or(0, Vec::len);
        if held >= AGENT_STATUS_WAIT_MAX_PER_SESSION {
            return Err(AgentStatusWaitError::capacity_exhausted(
                AgentStatusWaitCapacity::Session,
            ));
        }
        if tables.total >= AGENT_STATUS_WAIT_MAX_GLOBAL {
            return Err(AgentStatusWaitError::capacity_exhausted(
                AgentStatusWaitCapacity::Global,
            ));
        }
        let (sender, receiver) = oneshot::channel();
        let entry = Arc::new(WaiterEntry {
            request: request.clone(),
            sender: Mutex::new(Some(sender)),
        });
        tables
            .by_session
            .entry(request.session_id.clone())
            .or_default()
            .push(Arc::clone(&entry));
        tables.total += 1;
        drop(tables);
        log_wait("wait_registered", &request, "registered", None);
        Ok(AgentStatusWaiter {
            registry: self.clone(),
            request,
            entry: Some(entry),
            receiver: Some(receiver),
        })
    }

    /// Re-evaluate one session's waiters after a retained-status or closure
    /// change. A waiter that matches leaves the registry here, not later.
    pub fn evaluate(&self, view: &dyn AgentStatusWaitView, session_id: &SessionId) {
        let entries = {
            let tables = self.lock();
            tables
                .by_session
                .get(session_id)
                .cloned()
                .unwrap_or_default()
        };
        for entry in entries {
            if let Some(outcome) = evaluate_entry(&entry, view) {
                self.resolve(&entry, outcome);
            }
        }
    }

    /// End every wait on a closed session, before its synthetic deletion is
    /// published: a wait that resolved as `occupant_changed` would tell the
    /// client its agent had been replaced when the session had gone away.
    pub fn close_session(&self, session_id: &SessionId) {
        let entries = {
            let mut tables = self.lock();
            tables.by_session.remove(session_id).unwrap_or_default()
        };
        for entry in entries {
            self.settle_detached(&entry, Ok(AgentStatusWaitOutcome::SessionClosed));
        }
    }

    /// End every wait, for a hub that is stopping.
    pub fn cancel_all(&self) {
        let entries = {
            let mut tables = self.lock();
            let entries: Vec<Arc<WaiterEntry>> =
                tables.by_session.values().flatten().cloned().collect();
            tables.by_session.clear();
            tables.total = 0;
            entries
        };
        for entry in entries {
            self.settle_detached(&entry, Err(AgentStatusWaitError::canceled()));
        }
    }

    /// How many waits are registered, for a diagnostics answer and for the
    /// tests that prove a released wait is not left behind.
    #[must_use]
    pub fn waiter_count(&self) -> usize {
        self.lock().total
    }

    /// How many sessions hold at least one wait.
    #[must_use]
    pub fn session_count(&self) -> usize {
        self.lock().by_session.len()
    }

    /// Remove one waiter from the registry.
    ///
    /// The single bookkeeping point every exit path goes through -- a match, a
    /// close, a cancellation and a dropped subscriber alike -- so no path can
    /// leave a session's slot occupied.
    fn remove(&self, session_id: &SessionId, entry: &Arc<WaiterEntry>) -> bool {
        let mut tables = self.lock();
        let Some(entries) = tables.by_session.get_mut(session_id) else {
            return false;
        };
        let Some(index) = entries.iter().position(|held| Arc::ptr_eq(held, entry)) else {
            return false;
        };
        entries.remove(index);
        if entries.is_empty() {
            tables.by_session.remove(session_id);
        }
        tables.total = tables.total.saturating_sub(1);
        true
    }

    /// Resolve a waiter that is still registered.
    fn resolve(&self, entry: &Arc<WaiterEntry>, outcome: AgentStatusWaitOutcome) {
        if !self.remove(&entry.request.session_id, entry) {
            return;
        }
        self.settle_detached(entry, Ok(outcome));
    }

    /// Hand a settlement to a waiter already out of the registry.
    fn settle_detached(&self, entry: &Arc<WaiterEntry>, settlement: WaitSettlement) {
        match &settlement {
            Ok(outcome) => log_wait(
                "wait_resolved",
                &entry.request,
                outcome.as_str(),
                match outcome {
                    AgentStatusWaitOutcome::Matched { matched_revision } => Some(*matched_revision),
                    _ => None,
                },
            ),
            Err(error) => log_wait("wait_rejected", &entry.request, error.kind().as_str(), None),
        }
        if let Some(sender) = take_sender(entry) {
            // A caller that already gave up is not an error: the oneshot simply
            // has no receiver left, which is the drop path working.
            let _ = sender.send(settlement);
        }
    }

    fn lock(&self) -> MutexGuard<'_, WaitTables> {
        self.tables.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Take a waiter's one-shot sender, leaving the slot empty.
fn take_sender(entry: &WaiterEntry) -> Option<oneshot::Sender<WaitSettlement>> {
    entry.sender.lock().ok().and_then(|mut slot| slot.take())
}

/// One admitted wait: released by the next change, or by being dropped.
pub struct AgentStatusWaiter {
    registry: AgentStatusWaitRegistry,
    request: AgentStatusWaitRequest,
    entry: Option<Arc<WaiterEntry>>,
    receiver: Option<oneshot::Receiver<WaitSettlement>>,
}

impl AgentStatusWaiter {
    /// The request this wait is pinned to.
    #[must_use]
    pub fn request(&self) -> &AgentStatusWaitRequest {
        &self.request
    }

    /// How long the client asked to wait.
    #[must_use]
    pub fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.request.timeout_ms)
    }

    /// Wait for the outcome.
    pub async fn settle(mut self) -> Result<AgentStatusWaitOutcome, AgentStatusWaitError> {
        let receiver = self.receiver.take().unwrap_or_else(closed_receiver);
        // The waiter leaves the registry here as well as in `Drop`: an outcome
        // is a real answer and must be released whether or not the client is
        // still listening.
        if let Some(entry) = self.entry.take() {
            self.registry.remove(&self.request.session_id, &entry);
        }
        receiver
            .await
            .unwrap_or(Err(AgentStatusWaitError::canceled()))
    }
}

impl Drop for AgentStatusWaiter {
    fn drop(&mut self) {
        if let Some(entry) = self.entry.take() {
            self.registry.remove(&self.request.session_id, &entry);
        }
    }
}

impl std::fmt::Debug for AgentStatusWaiter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentStatusWaiter")
            .field("session_id", &self.request.session_id.as_str())
            .field("status_epoch", &self.request.status_epoch.as_str())
            .finish_non_exhaustive()
    }
}

/// A receiver nobody can ever answer, for a waiter that has already settled.
fn closed_receiver() -> oneshot::Receiver<WaitSettlement> {
    oneshot::channel().1
}

/// The outcome this waiter is currently entitled to, if any.
fn evaluate_entry(
    entry: &WaiterEntry,
    view: &dyn AgentStatusWaitView,
) -> Option<AgentStatusWaitOutcome> {
    let request = &entry.request;
    if view.is_closed(&request.session_id) {
        return Some(AgentStatusWaitOutcome::SessionClosed);
    }
    let Some(status) = view.retained(&request.session_id) else {
        return Some(AgentStatusWaitOutcome::OccupantChanged);
    };
    if !is_pinned_occupant(&status.common, request) {
        return Some(AgentStatusWaitOutcome::OccupantChanged);
    }
    if !request.desired_states.contains(&status.common.state) {
        return None;
    }
    if let Some(after_revision) = request.after_revision {
        let advance = if status.common.state == AgentRuntimeState::Idle {
            status.common.completed_revision
        } else {
            view.state_change_revision(&request.session_id)
        };
        if advance <= after_revision {
            return None;
        }
    }
    Some(AgentStatusWaitOutcome::Matched {
        matched_revision: status.common.revision,
    })
}

/// Whether the retained status is the exact occupant this wait pinned.
fn is_pinned_occupant(status: &AgentStatusFields, request: &AgentStatusWaitRequest) -> bool {
    agent_status_identity(status).is_some_and(|identity| {
        identity.status_epoch == request.status_epoch && identity.occupant_id == request.occupant_id
    })
}

fn log_wait(event: &str, request: &AgentStatusWaitRequest, outcome: &str, revision: Option<i64>) {
    let mut fields = LogFields::new()
        .set("session_id", request.session_id.as_str())
        .set("status_epoch", request.status_epoch.as_str())
        .set("occupant_id", request.occupant_id.as_str())
        .set("outcome", outcome);
    if let Some(revision) = revision {
        fields = fields.set("matched_revision", revision);
    }
    roost_observability::log::debug("agents.status", event, fields);
}
