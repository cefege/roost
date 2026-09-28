//! The smoke observer on the terminal input path: admitted batches (bounded by
//! count and bytes), capped outcome counters, and each batch's own outcome for
//! the waiting `input()` probe. Armed only by roost-web's `smoke::backdoor`; fed
//! by `InputRouter` admission and settlement. Ports
//! `apps/web/src/client/carriers/sync-outbound-smoke.ts` and the capture half of
//! `apps/web/src/smoke/smokeTerminalInputController.ts`.

use std::collections::VecDeque;

use crate::terminal::input::{InputOutcome, InputRefusal, PendingInput};

/// Most admitted batches the capture keeps.
pub const TERMINAL_INPUT_CAPTURE_MAX_BATCHES: usize = 512;
/// Most admitted bytes the capture keeps; a larger batch is counted as dropped.
pub const TERMINAL_INPUT_CAPTURE_MAX_BYTES: usize = 1024 * 1024;
/// The ceiling each outcome counter saturates at.
pub const TERMINAL_INPUT_OUTCOME_COUNT_CAP: u32 = 512;
/// Most settled outcomes kept for a waiting probe. A batch typed by hand in a
/// smoke build is never collected, so the ledger evicts its oldest entry.
const SETTLED_OUTCOME_CAPACITY: usize = 512;

/// One admitted batch as the capture holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedInputBatch {
    /// The session the batch was admitted for.
    pub session_id: String,
    /// The bytes, copied at admission.
    pub data: Vec<u8>,
}

/// Settled outcomes by category, each capped at `TERMINAL_INPUT_OUTCOME_COUNT_CAP`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputOutcomeCounts {
    /// Written to the PTY.
    pub accepted: u32,
    /// Provably not written.
    pub rejected: u32,
    /// Fate unknown.
    pub ambiguous: u32,
}

/// What `terminalInputCapture()` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalInputCapture {
    /// Retained batches, oldest first.
    pub batches: Vec<CapturedInputBatch>,
    /// Batches evicted by the bounds, or too large to keep at all.
    pub dropped_batches: u64,
    /// Settled outcome counters.
    pub outcomes: InputOutcomeCounts,
}

/// How the most recent admission went, for the probe that just asked for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservedAdmission {
    /// Queued under this sequence.
    Admitted {
        /// The batch's own sequence.
        input_seq: u64,
    },
    /// Refused before it was queued.
    Refused {
        /// The router's reason.
        reason: String,
    },
}

/// The smoke observer. Lives in `InputRouter::smoke_observer` while armed.
#[derive(Debug, Default)]
pub struct SmokeInputObserver {
    batches: VecDeque<CapturedInputBatch>,
    bytes: usize,
    dropped_batches: u64,
    outcomes: InputOutcomeCounts,
    last_admission: Option<ObservedAdmission>,
    settled: VecDeque<InputOutcome>,
}

impl SmokeInputObserver {
    /// An empty observer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one admission answer.
    pub fn observe_admission(
        &mut self,
        session_id: &str,
        admission: &Result<PendingInput, InputRefusal>,
    ) {
        match admission {
            Ok(pending) => {
                self.capture_batch(session_id, &pending.bytes);
                self.last_admission = Some(ObservedAdmission::Admitted {
                    input_seq: pending.input_seq,
                });
            }
            Err(refusal) => {
                self.last_admission = Some(ObservedAdmission::Refused {
                    reason: refusal.reason.clone(),
                });
            }
        }
    }

    /// Record one settled outcome.
    pub fn observe_outcome(&mut self, outcome: &InputOutcome) {
        let counter = match outcome {
            InputOutcome::Accepted { .. } => &mut self.outcomes.accepted,
            InputOutcome::Rejected { .. } => &mut self.outcomes.rejected,
            InputOutcome::Ambiguous { .. } => &mut self.outcomes.ambiguous,
        };
        *counter = (*counter + 1).min(TERMINAL_INPUT_OUTCOME_COUNT_CAP);
        if self.settled.len() >= SETTLED_OUTCOME_CAPACITY {
            self.settled.pop_front();
        }
        self.settled.push_back(outcome.clone());
    }

    /// The answer to the most recent admission, consumed.
    pub fn take_admission(&mut self) -> Option<ObservedAdmission> {
        self.last_admission.take()
    }

    /// The settled outcome of `input_seq`, consumed, or `None` while it is open.
    pub fn take_outcome(&mut self, input_seq: u64) -> Option<InputOutcome> {
        let position = self
            .settled
            .iter()
            .position(|outcome| outcome.input_seq() == input_seq)?;
        self.settled.remove(position)
    }

    /// The bounded capture, as `terminalInputCapture()` reports it.
    pub fn capture(&self) -> TerminalInputCapture {
        TerminalInputCapture {
            batches: self.batches.iter().cloned().collect(),
            dropped_batches: self.dropped_batches,
            outcomes: self.outcomes,
        }
    }

    /// Clear batches, drops and counters (`resetTerminalInputCapture()`). Open
    /// admissions and outcomes a probe is still waiting on are kept.
    pub fn reset_capture(&mut self) {
        self.batches.clear();
        self.bytes = 0;
        self.dropped_batches = 0;
        self.outcomes = InputOutcomeCounts::default();
        tracing::debug!(target: "smoke", "terminal input capture reset");
    }

    fn capture_batch(&mut self, session_id: &str, data: &[u8]) {
        if data.len() > TERMINAL_INPUT_CAPTURE_MAX_BYTES {
            self.dropped_batches += 1;
            return;
        }
        while self.batches.len() >= TERMINAL_INPUT_CAPTURE_MAX_BATCHES
            || self.bytes + data.len() > TERMINAL_INPUT_CAPTURE_MAX_BYTES
        {
            let Some(evicted) = self.batches.pop_front() else {
                break;
            };
            self.bytes -= evicted.data.len();
            self.dropped_batches += 1;
        }
        self.bytes += data.len();
        self.batches.push_back(CapturedInputBatch {
            session_id: session_id.to_owned(),
            data: data.to_vec(),
        });
    }
}
