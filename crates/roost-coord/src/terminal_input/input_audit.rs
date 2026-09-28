//! The bounded queue that persists every audited terminal-input outcome
//! before the outcome is reported: FIFO batches of at most 64 rows, 1,024 slots
//! held through a failed write, and producers beyond that waiting for room
//! rather than being dropped. Owned by `TerminalInputRuntime`; fed by
//! `terminal_input::input_control`; writes through `middleware::audit`.
//! Ports the audit pump of `apps/coord/src/terminal/input/input-control.ts`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use roost_observability::{LogFields, SignalKind};
use tokio::sync::oneshot;

use crate::coord_core::CoordCore;
use crate::middleware::audit::{AuditRecord, write_audit_rows};
use crate::services::CoordServices;
use crate::terminal_input::write_control::TerminalWriteStatus;

/// Audits queued or in flight before a producer must wait.
pub const INPUT_AUDIT_QUEUE_CAP: usize = 1_024;

/// Rows one audit write carries at most.
pub const INPUT_AUDIT_BATCH_MAX: usize = 64;

/// One terminal-input outcome to persist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputAuditEntry {
    /// The writing device.
    pub caller_fingerprint: String,
    /// The outcome being reported.
    pub status: TerminalWriteStatus,
    /// Bytes the outcome reports written.
    pub written_bytes: u32,
}

impl InputAuditEntry {
    fn into_record(self) -> AuditRecord {
        let status = match self.status {
            TerminalWriteStatus::Accepted => 200,
            TerminalWriteStatus::Ambiguous => 409,
            TerminalWriteStatus::Rejected => 422,
        };
        let path = format!(
            "/ws/coord-sync/input/{}/{}/SessionsInput",
            self.status.as_str(),
            self.written_bytes
        );
        AuditRecord::sync_terminal_input(path, status, self.caller_fingerprint)
    }
}

/// The queue and its capacity waiters.
#[derive(Debug, Default)]
pub struct InputAuditQueue {
    state: Mutex<AuditQueueState>,
}

#[derive(Debug, Default)]
struct AuditQueueState {
    queued: VecDeque<QueuedAudit>,
    waiting: VecDeque<QueuedAudit>,
    in_flight: usize,
    pumping: bool,
}

#[derive(Debug)]
struct QueuedAudit {
    record: AuditRecord,
    settled: oneshot::Sender<Result<(), String>>,
}

impl InputAuditQueue {
    /// An empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue one outcome now, and resolve once its batch committed or failed.
    ///
    /// Queued synchronously, so audits commit in the order outcomes arrived;
    /// the returned future only waits.
    pub fn persist(
        &self,
        services: &Arc<CoordServices>,
        entry: InputAuditEntry,
    ) -> impl Future<Output = Result<(), String>> + Send + 'static {
        let caller_fp = entry.caller_fingerprint.clone();
        let (settled, receiver) = oneshot::channel();
        let queued = QueuedAudit {
            record: entry.into_record(),
            settled,
        };
        let (start_pump, backpressured) = {
            let mut state = self.state();
            if state.queued.len() + state.in_flight >= INPUT_AUDIT_QUEUE_CAP {
                state.waiting.push_back(queued);
                (false, true)
            } else {
                state.queued.push_back(queued);
                (!std::mem::replace(&mut state.pumping, true), false)
            }
        };
        if backpressured {
            roost_observability::signal::emit(
                SignalKind::AuditInputQueueBackpressure,
                LogFields::new()
                    .set("caller_fp", caller_fp)
                    .set("cooldownKey", "terminal-input"),
            );
        }
        if start_pump {
            tokio::spawn(pump_input_audits(Arc::clone(services)));
        }
        async move {
            receiver
                .await
                .unwrap_or_else(|_| Err("input audit queue released the record".to_owned()))
        }
    }

    /// The next FIFO batch, or `None` after marking the pump idle -- in one
    /// critical section, so an audit queued meanwhile starts a new pump.
    fn take_batch(&self) -> Option<Vec<QueuedAudit>> {
        let mut state = self.state();
        if state.queued.is_empty() {
            state.pumping = false;
            return None;
        }
        let take = state.queued.len().min(INPUT_AUDIT_BATCH_MAX);
        let batch: Vec<QueuedAudit> = state.queued.drain(..take).collect();
        state.in_flight += batch.len();
        Some(batch)
    }

    /// Return a settled batch's slots and admit waiters into them.
    fn finish_batch(&self, rows: usize) {
        let mut state = self.state();
        state.in_flight = state.in_flight.saturating_sub(rows);
        while state.queued.len() + state.in_flight < INPUT_AUDIT_QUEUE_CAP {
            let Some(waiting) = state.waiting.pop_front() else {
                break;
            };
            state.queued.push_back(waiting);
        }
    }

    fn state(&self) -> MutexGuard<'_, AuditQueueState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Write queued audits batch by batch until none remain. A failed batch fails
/// every record in it and still returns its slots.
async fn pump_input_audits(services: Arc<CoordServices>) {
    let core = CoordCore::new(Arc::clone(&services));
    let queue = services.terminal_input.audit();
    while let Some(batch) = queue.take_batch() {
        let records: Vec<AuditRecord> = batch.iter().map(|queued| queued.record.clone()).collect();
        let written = write_audit_rows(&core, &records)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string());
        if let Err(error) = &written {
            tracing::warn!(rows = records.len(), %error, "a terminal input audit batch failed");
        }
        queue.finish_batch(batch.len());
        for queued in batch {
            let _ = queued.settled.send(written.clone());
        }
    }
}
