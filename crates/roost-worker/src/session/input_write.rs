//! The session layer's PTY-input entry points: coordinator input under a
//! request budget and live browser authority, worker-owned input under
//! neither, and the legacy unacknowledged binary frame. Called by
//! `terminal_input::InputOwner` (the coordinator link), the query-reply lane,
//! and the local door. Ports the input half of v2
//! `apps/worker/src/session/session-terminal-control.ts` and
//! `SessionManager.input` (`apps/worker/src/session/session-manager.ts`).
//!
//! THE TRUTH MAPPING. `Rejected` is returned only from a stage that provably
//! never wrote: validation, lane refusal, a pre-write recheck, a keeper
//! admission that never reached the socket, or the keeper's own refusal. Every
//! uncertainty after the request reached the socket is `Ambiguous`, because the
//! coordinator unwinds provisional state (and the browser retries) only on a
//! rejection, and a rejection that overstated certainty would license a
//! duplicate keystroke.

use std::sync::{Arc, Mutex};

use roost_protocol::wire::brand::SessionId;

use super::binding::CellDelivery;
use super::keeper_admission::{Admission, AdmissionKind};
use super::keeper_channels::KeeperInputResult;
use super::lifecycle::SessionManager;
use super::table::SessionTable;
use crate::uplink::OwnerFuture;

/// The outcome of one input batch. v2 `WorkerInputResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerInputResult {
    Accepted { written_bytes: u32 },
    Rejected { reason: String },
    Ambiguous { written_bytes: u32, reason: String },
}

/// Live predicates supplied only by browser-originated writers, rechecked after
/// keeper admission and immediately before the write. v2 `TerminalWriteAuthority`.
pub trait TerminalWriteAuthority: Send + Sync {
    fn is_session_authorized(&self) -> bool;
    fn is_current_input_route(&self) -> bool;
}

/// The request budget a writer honours before queued keeper work. v2
/// `TerminalRequestBudget` as the write path reads it.
pub trait TerminalWriteBudget: Send + Sync {
    /// The connection the request arrived on is still the current one.
    fn is_current_connection(&self) -> bool;
    /// No time remains (v2 `remainingMs() <= 0`).
    fn expired(&self) -> bool;
}

const SESSION_UNAVAILABLE: &str = "terminal session is unavailable";
const ROUTE_CHANGED: &str = "terminal input route changed";
const SESSION_NOT_LIVE: &str = "session is not live";

impl SessionManager {
    /// Write one coordinator (or direct-port) input batch. v2 `writeTerminalInput`.
    pub fn write_terminal_input(
        &self,
        session_id: &SessionId,
        input_seq: u64,
        bytes: Vec<u8>,
        budget: Option<Box<dyn TerminalWriteBudget>>,
        authority: Option<Box<dyn TerminalWriteAuthority>>,
    ) -> OwnerFuture<WorkerInputResult> {
        if let Some(authority) = &authority {
            if !authority.is_session_authorized() {
                return ready(rejected(SESSION_UNAVAILABLE));
            }
            if !authority.is_current_input_route() {
                return ready(rejected(ROUTE_CHANGED));
            }
        }
        let Some(channel_id) = self.sessions.channel_of(session_id) else {
            return ready(rejected(if authority.is_some() {
                SESSION_UNAVAILABLE
            } else {
                SESSION_NOT_LIVE
            }));
        };
        if input_seq == 0 {
            return ready(rejected("input sequence must be positive"));
        }
        self.write_acknowledged_batch(channel_id, bytes, budget, authority)
    }

    /// Write one worker-originated batch without manufacturing a coordinator
    /// input sequence; keeper correlation stays private to the pool. v2
    /// `writeWorkerOwnedTerminalInput`. A caller that needs two batches in
    /// order awaits the first: the write-ordering slot is taken on first poll.
    pub fn write_worker_owned_input(
        &self,
        session_id: &SessionId,
        bytes: Vec<u8>,
    ) -> OwnerFuture<WorkerInputResult> {
        let Some(channel_id) = self.sessions.channel_of(session_id) else {
            return ready(rejected(SESSION_NOT_LIVE));
        };
        self.write_acknowledged_batch(channel_id, bytes, None, None)
    }

    /// Route one legacy binary input chunk to the keeper, unacknowledged. v2
    /// `SessionManager.input`: a channel this worker does not hold is ignored,
    /// and the echo promotion is queued at the same point the acknowledged lane
    /// queues it.
    pub fn write_legacy_input(&self, channel_id: u16, bytes: &[u8]) {
        if !mark_input_sensitive(&self.sessions, &self.cells, channel_id) {
            tracing::debug!(
                channel_id,
                bytes = bytes.len(),
                "legacy input for a channel this worker does not hold"
            );
            return;
        }
        if let Err(fault) = self.keeper.write_legacy_input(channel_id, bytes) {
            tracing::warn!(channel_id, bytes = bytes.len(), %fault, "legacy input did not reach the keeper");
        }
    }

    /// v2 `writeAcknowledgedInputBatch`: lane admission, the pre-write
    /// rechecks, the keeper's two halves, and the truth mapping between them.
    fn write_acknowledged_batch(
        &self,
        channel_id: u16,
        bytes: Vec<u8>,
        budget: Option<Box<dyn TerminalWriteBudget>>,
        authority: Option<Box<dyn TerminalWriteAuthority>>,
    ) -> OwnerFuture<WorkerInputResult> {
        if bytes.is_empty() {
            return ready(WorkerInputResult::Accepted { written_bytes: 0 });
        }
        let branded = match self
            .sessions
            .with_channel_record(channel_id, |record| record.channel_id())
        {
            Some(branded) => branded,
            None => return ready(rejected(SESSION_NOT_LIVE)),
        };
        let ticket = match self.lanes.admit(branded, AdmissionKind::TerminalInput) {
            Admission::Granted(ticket) => ticket,
            Admission::Refused(reason) => return ready(rejected(reason)),
        };
        let sessions = Arc::clone(&self.sessions);
        let cells = Arc::clone(&self.cells);
        let keeper = Arc::clone(&self.keeper);
        Box::pin(async move {
            ticket.granted().await;
            if let Some(refusal) = pre_write_refusal(
                &sessions,
                channel_id,
                budget.as_deref(),
                authority.as_deref(),
            ) {
                tracing::info!(
                    channel_id,
                    reason = refusal,
                    "terminal input refused before the keeper write"
                );
                return rejected(refusal);
            }
            mark_input_sensitive(&sessions, &cells, channel_id);
            let expected = bytes.len() as u32;
            let begun =
                tokio::task::spawn_blocking(move || keeper.begin_input(channel_id, bytes)).await;
            // The ordering boundary is the request on the socket, not its answer.
            ticket.release();
            let command = match begun {
                Ok(command) => command,
                Err(error) => return ambiguous_input_result(channel_id, 0, &error.to_string()),
            };
            if let Err(refusal) = command.admission {
                return rejected(&format!(
                    "keeper did not accept the input: {}",
                    refusal.as_str()
                ));
            }
            match command.result.await {
                KeeperInputResult::Ack { written } if written == expected => {
                    WorkerInputResult::Accepted {
                        written_bytes: written,
                    }
                }
                KeeperInputResult::Ack { written } => ambiguous_input_result(
                    channel_id,
                    written,
                    "keeper acknowledged an incomplete input batch",
                ),
                KeeperInputResult::Reject { reason } => rejected(&reason),
                KeeperInputResult::Ambiguous { written, reason } => {
                    ambiguous_input_result(channel_id, written.unwrap_or(0), &reason)
                }
            }
        })
    }
}

/// The rechecks that run after the lane is granted and before the keeper write,
/// each one a stage that provably wrote nothing. v2 `writeAcknowledgedInputBatch`.
fn pre_write_refusal(
    sessions: &SessionTable,
    channel_id: u16,
    budget: Option<&dyn TerminalWriteBudget>,
    authority: Option<&dyn TerminalWriteAuthority>,
) -> Option<&'static str> {
    if let Some(authority) = authority {
        if !authority.is_session_authorized() {
            return Some(SESSION_UNAVAILABLE);
        }
        if !authority.is_current_input_route() {
            return Some(ROUTE_CHANGED);
        }
    }
    if sessions.record_of_channel(channel_id).is_none() {
        return Some(if authority.is_some() {
            SESSION_UNAVAILABLE
        } else {
            "session closed before the keeper write"
        });
    }
    let budget = budget?;
    if !budget.is_current_connection() {
        return Some("worker connection superseded before the keeper write");
    }
    if budget.expired() {
        return Some("input budget expired before the keeper write");
    }
    None
}

/// Queue one input-echo promotion for a channel this worker holds (v2
/// `markInputSensitive`). The record lock is released before the cells lock is
/// taken, which is the order `respawn` holds them in. `false` when not held.
fn mark_input_sensitive(
    sessions: &SessionTable,
    cells: &Arc<Mutex<dyn CellDelivery>>,
    channel_id: u16,
) -> bool {
    let Some(branded) = sessions.with_channel_record(channel_id, |record| record.channel_id())
    else {
        return false;
    };
    cells
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .note_input_echo(branded);
    true
}

/// An unconfirmed keeper write is the only input outcome the user cannot
/// attribute from the browser alone, so it is logged where it is manufactured.
fn ambiguous_input_result(channel_id: u16, written_bytes: u32, reason: &str) -> WorkerInputResult {
    tracing::warn!(
        channel_id,
        written_bytes,
        reason,
        "terminal_input_ambiguous"
    );
    WorkerInputResult::Ambiguous {
        written_bytes,
        reason: reason.to_owned(),
    }
}

fn rejected(reason: &str) -> WorkerInputResult {
    WorkerInputResult::Rejected {
        reason: reason.to_owned(),
    }
}

fn ready(result: WorkerInputResult) -> OwnerFuture<WorkerInputResult> {
    Box::pin(std::future::ready(result))
}
