//! The document's input routing: admission caps, the hold, the route epoch, and
//! the retirement that decides `rejected` versus `ambiguous`.
//!
//! `InputRouter` owns no transport and no timer. It admits, it hands the caller
//! a batch to send, and it settles what comes back — which is why it can be
//! driven from a test with no socket anywhere in the process.
//!
//! Depends on the vocabulary in the parent module; called by `handle_event`.

use std::collections::BTreeMap;

use crate::terminal::input::{
    HELD_INPUT_ADMISSION_TIMEOUT_MS, InputLane, InputOutcome, InputPhase, InputRefusal,
    MAX_INPUT_BYTES, MAX_PENDING_INPUT_BYTES_PER_SESSION, MAX_PENDING_INPUTS_PER_SESSION,
    MAX_TERMINAL_INPUT_ROUTE_REVISION, PendingInput,
};
use crate::terminal::token::TerminalToken;

/// The whole document's input routing.
#[derive(Debug, Default)]
pub struct InputRouter {
    lanes: BTreeMap<String, InputLane>,
    next_input_seq: u64,
}

impl InputRouter {
    /// A router with no lanes.
    pub fn new() -> Self {
        Self::default()
    }

    /// One session's lane, if it has one.
    pub fn lane(&self, session_id: &str) -> Option<&InputLane> {
        self.lanes.get(session_id)
    }

    /// How many sequences this router has allocated. A test reads it to prove a
    /// refused batch consumed one.
    pub fn allocated_count(&self) -> u64 {
        self.next_input_seq
    }

    fn lane_mut(&mut self, session_id: &str) -> &mut InputLane {
        self.lanes
            .entry(session_id.to_string())
            .or_insert_with(|| InputLane {
                session_id: session_id.to_string(),
                phase: InputPhase::Sending,
                pending: Vec::new(),
                pending_bytes: 0,
                route_epoch: String::new(),
                route_revision: 0,
                route_epoch_token: None,
                ambiguous: Vec::new(),
            })
    }

    /// Admit one batch, or refuse it before it is ever queued.
    ///
    /// The sequence is allocated even for a batch that is then refused, because
    /// the pane correlates what it typed with what came back, and a sequence that
    /// skips is a sequence the pane waits for forever.
    ///
    /// `now_ms` is the host's clock, passed in so this stays pure: the hold
    /// timeout is a deadline, and a deadline a test cannot move is a deadline it
    /// cannot prove.
    pub fn admit(
        &mut self,
        session_id: &str,
        view_id: Option<String>,
        bytes: Vec<u8>,
        now_ms: u64,
    ) -> Result<PendingInput, InputRefusal> {
        match self.lane_mut(session_id).phase {
            InputPhase::Closed => {
                return Err(InputRefusal {
                    reason: "terminal session is closed".to_string(),
                });
            }
            InputPhase::Blocked => {
                return Err(InputRefusal {
                    reason: "terminal input route is reconnecting".to_string(),
                });
            }
            _ => {}
        }
        if bytes.len() > MAX_INPUT_BYTES {
            return Err(InputRefusal {
                reason: format!("input exceeds {MAX_INPUT_BYTES} bytes"),
            });
        }
        self.next_input_seq += 1;
        let pending = PendingInput {
            session_id: session_id.to_string(),
            view_id,
            input_seq: self.next_input_seq,
            bytes,
            fence: None,
            started: false,
            admitted_at_ms: now_ms,
        };
        let byte_length = pending.bytes.len();
        let lane = self.lane_mut(session_id);
        if lane.pending.len() >= MAX_PENDING_INPUTS_PER_SESSION
            || lane.pending_bytes + byte_length > MAX_PENDING_INPUT_BYTES_PER_SESSION
        {
            return Err(InputRefusal {
                reason: format!(
                    "terminal input lane is full ({MAX_PENDING_INPUTS_PER_SESSION} batches, \
                     {MAX_PENDING_INPUT_BYTES_PER_SESSION} bytes)"
                ),
            });
        }
        lane.pending_bytes += byte_length;
        lane.pending.push(pending.clone());
        Ok(pending)
    }

    /// The route epoch to put on a batch going out on `token`.
    ///
    /// Empty unless the epoch was acknowledged FOR THIS GENERATION. Sending an
    /// old generation's epoch is worse than sending none: the worker rechecks
    /// live route authority after keeper admission and refuses a stale epoch, so
    /// the batch would be lost rather than written.
    pub fn route_epoch_for(&self, session_id: &str, token: &TerminalToken) -> String {
        self.lanes
            .get(session_id)
            .filter(|lane| lane.route_epoch_token.as_ref() == Some(token))
            .map(|lane| lane.route_epoch.clone())
            .unwrap_or_default()
    }

    /// Install an acknowledged route epoch.
    ///
    /// Returns false for a revision that is zero, past the wire's ceiling, or
    /// older than the installed one. All three are refused rather than
    /// accepted-and-wrapped: a wrapped revision reads as an older epoch to the
    /// worker, which would let a stale claim pass a check it should fail.
    pub fn install_route_epoch(
        &mut self,
        session_id: &str,
        token: &TerminalToken,
        epoch: impl Into<String>,
        revision: u64,
    ) -> bool {
        if revision == 0 || revision > MAX_TERMINAL_INPUT_ROUTE_REVISION {
            return false;
        }
        let lane = self.lane_mut(session_id);
        if revision < lane.route_revision {
            return false;
        }
        lane.route_epoch = epoch.into();
        lane.route_revision = revision;
        lane.route_epoch_token = Some(token.clone());
        true
    }

    /// Move a lane into a new phase, and settle what that implies.
    ///
    /// Entering `Holding` or `Claiming` settles nothing: those phases are where
    /// batches WAIT, which is the whole point of holding them unsent. Entering
    /// `Blocked` or `Closed` refuses every batch that has not started — those
    /// cannot reach the PTY, so refusing them loses nothing.
    pub fn set_phase(&mut self, session_id: &str, phase: InputPhase) -> Vec<InputOutcome> {
        self.lane_mut(session_id).phase = phase;
        match phase {
            InputPhase::Blocked | InputPhase::Closed => {
                let reason = if phase == InputPhase::Closed {
                    "terminal session is closed"
                } else {
                    "terminal input route is reconnecting"
                };
                self.refuse_unstarted(session_id, reason)
            }
            _ => Vec::new(),
        }
    }

    /// Mark a batch as handed to a transport on `token`.
    ///
    /// From here the outcome can be `ambiguous`, which is why nothing about a
    /// started batch is ever replayed.
    pub fn mark_started(&mut self, input_seq: u64, token: &TerminalToken) -> bool {
        let Some(lane) = self.lanes.values_mut().find(|lane| {
            lane.pending
                .iter()
                .any(|pending| pending.input_seq == input_seq)
        }) else {
            return false;
        };
        let Some(pending) = lane
            .pending
            .iter_mut()
            .find(|pending| pending.input_seq == input_seq)
        else {
            return false;
        };
        pending.started = true;
        pending.fence = Some(crate::terminal::input::TerminalFence::new(token.clone()));
        true
    }

    /// Settle one batch with the result its transport reported.
    ///
    /// A result for a batch that has already settled is ignored: a carrier that
    /// answers twice must not be able to turn a refusal into a write.
    pub fn settle(&mut self, input_seq: u64, outcome: InputOutcome) -> bool {
        if outcome.input_seq() != input_seq {
            return false;
        }
        self.lanes
            .values_mut()
            .any(|lane| settle_in(lane, input_seq))
    }

    /// Settle every held batch whose admission timeout has expired.
    ///
    /// `rejected`, never `ambiguous`: nothing was sent, so nothing is in doubt.
    pub fn sweep_held(&mut self, now_ms: u64) -> Vec<InputOutcome> {
        let expired: Vec<u64> = self
            .lanes
            .values()
            .flat_map(|lane| lane.pending.iter())
            .filter(|pending| {
                !pending.started
                    && now_ms.saturating_sub(pending.admitted_at_ms)
                        >= HELD_INPUT_ADMISSION_TIMEOUT_MS
            })
            .map(|pending| pending.input_seq)
            .collect();
        expired
            .into_iter()
            .map(|input_seq| {
                let outcome = InputOutcome::Rejected {
                    input_seq,
                    reason: "terminal input route is reconnecting".to_string(),
                };
                self.settle(input_seq, outcome.clone());
                outcome
            })
            .collect()
    }

    /// Settle everything dispatched on `token` when that carrier retires.
    ///
    /// The distinction is the safety of this whole module. A batch that had
    /// STARTED settles `ambiguous` — the transport may have handed the bytes to
    /// the worker before the route went away, and re-sending them would double
    /// them. A batch that had NOT started settles `rejected`, with a reason that
    /// says so, because it provably never left.
    pub fn retire_token(&mut self, token: &TerminalToken, reason: &str) -> Vec<InputOutcome> {
        let mut outcomes = Vec::new();
        for lane in self.lanes.values_mut() {
            let matching: Vec<(u64, bool)> = lane
                .pending
                .iter()
                .filter(|pending| {
                    pending
                        .fence
                        .as_ref()
                        .is_some_and(|fence| &fence.token == token)
                })
                .map(|pending| (pending.input_seq, pending.started))
                .collect();
            for (input_seq, started) in matching {
                let outcome = if started {
                    InputOutcome::Ambiguous {
                        input_seq,
                        written_bytes: 0,
                        reason: format!(
                            "{reason} after input was sent; the batch will not be retried"
                        ),
                    }
                } else {
                    InputOutcome::Rejected {
                        input_seq,
                        reason: format!("{reason} before input was sent"),
                    }
                };
                if !settle_in(lane, input_seq) {
                    continue;
                }
                if outcome.is_ambiguous() {
                    lane.ambiguous.push(input_seq);
                }
                outcomes.push(outcome);
            }
        }
        outcomes
    }

    /// Refuse every batch that has not started, with one reason.
    pub fn refuse_unstarted(&mut self, session_id: &str, reason: &str) -> Vec<InputOutcome> {
        let Some(lane) = self.lanes.get_mut(session_id) else {
            return Vec::new();
        };
        let matching: Vec<u64> = lane
            .pending
            .iter()
            .filter(|pending| !pending.started)
            .map(|pending| pending.input_seq)
            .collect();
        matching
            .into_iter()
            .filter_map(|input_seq| {
                if settle_in(lane, input_seq) {
                    Some(InputOutcome::Rejected {
                        input_seq,
                        reason: reason.to_string(),
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    /// Every batch still outstanding on a session, oldest first.
    pub fn outstanding(&self, session_id: &str) -> Vec<&PendingInput> {
        self.lanes
            .get(session_id)
            .map(|lane| lane.pending.iter().collect())
            .unwrap_or_default()
    }

    /// Whether a session has a batch whose fate the client cannot report.
    ///
    /// A drain refuses to complete while this is true: "the route cannot drain"
    /// is the honest answer, because nobody can say the batch was not written.
    pub fn has_ambiguity(&self, session_id: &str) -> bool {
        self.lanes
            .get(session_id)
            .is_some_and(|lane| !lane.ambiguous.is_empty())
    }
}

/// Settle one batch inside its own lane, keeping the byte accounting right.
fn settle_in(lane: &mut InputLane, input_seq: u64) -> bool {
    let Some(position) = lane
        .pending
        .iter()
        .position(|pending| pending.input_seq == input_seq)
    else {
        return false;
    };
    lane.pending_bytes = lane
        .pending_bytes
        .saturating_sub(lane.pending[position].bytes.len());
    lane.pending.remove(position);
    true
}
