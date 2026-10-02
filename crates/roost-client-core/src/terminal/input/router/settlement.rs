//! Settling: what a result means for a batch, and who is allowed to say it.
//!
//! Split from the admission half because the two are read at opposite ends of a
//! batch's life, and the split is the whole safety argument: everything in this
//! file is about what has ALREADY LEFT this client, and nothing in it may ever
//! put those bytes back on a wire.

use crate::terminal::input::{
    HELD_INPUT_ADMISSION_TIMEOUT_MS, INPUT_RESULT_TIMEOUT_MS, InputLane, InputOutcome,
};
use crate::terminal::token::{TerminalToken, TerminalTransport};

use super::InputRouter;

impl InputRouter {
    /// Settle one batch with the result its transport reported.
    ///
    /// A result for a batch that has already settled is ignored: a carrier that
    /// answers twice must not be able to turn a refusal into a write.
    pub fn settle(&mut self, input_seq: u64, outcome: InputOutcome) -> bool {
        if outcome.input_seq() != input_seq {
            return false;
        }
        let settled = self
            .lanes
            .values_mut()
            .any(|lane| settle_in(lane, input_seq));
        if settled {
            self.observe_settled(std::slice::from_ref(&outcome));
        }
        settled
    }

    /// Settle one batch only when this transport is the one that carried it.
    ///
    /// A direct carrier reports a result for a batch by its own sequence, and the
    /// same worker also has batches outstanding on the Sync socket it wrote
    /// before the promotion. Both answers are true, and a session that has both
    /// lanes open must not let either one settle the other's batch — the session
    /// and the `TerminalFence` the batch was dispatched under are therefore both
    /// checked, and a result that fails either changes nothing. A batch that has
    /// not started has no fence yet, because nothing has left the client, so a
    /// result for it belongs to nobody and is refused here exactly as it would be
    /// by `settle`.
    pub fn settle_from(
        &mut self,
        session_id: &str,
        token: &TerminalToken,
        input_seq: u64,
        outcome: InputOutcome,
    ) -> bool {
        if outcome.input_seq() != input_seq {
            return false;
        }
        let settled = self
            .lanes
            .get_mut(session_id)
            .and_then(|lane| {
                let position = lane
                    .pending
                    .iter()
                    .position(|pending| pending.input_seq == input_seq)?;
                let carried = lane.pending[position]
                    .fence
                    .as_ref()
                    .is_some_and(|fence| &fence.token == token);
                carried.then(|| settle_in(lane, input_seq))
            })
            .unwrap_or(false);
        if settled {
            self.observe_settled(std::slice::from_ref(&outcome));
        }
        settled
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

    /// Settle every started batch whose result is overdue (v2 `markStarted`'s
    /// timer).
    ///
    /// `ambiguous`, never `rejected` and never re-sent: the bytes left, and the
    /// worker may have written them before its answer was lost.
    pub fn sweep_unanswered(&mut self, now_ms: u64) -> Vec<InputOutcome> {
        let overdue: Vec<u64> = self
            .lanes
            .values()
            .flat_map(|lane| lane.pending.iter())
            .filter(|pending| {
                pending.started
                    && now_ms.saturating_sub(pending.started_at_ms) >= INPUT_RESULT_TIMEOUT_MS
            })
            .map(|pending| pending.input_seq)
            .collect();
        overdue
            .into_iter()
            .map(|input_seq| {
                let outcome = InputOutcome::Ambiguous {
                    input_seq,
                    written_bytes: 0,
                    reason: "input result timed out; the batch will not be retried".to_string(),
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
        self.retire_fenced(|fenced| fenced == token, reason)
    }

    /// Settle everything dispatched on the Sync socket of `socket_generation`
    /// when that socket closes, under any domain generation it carried (v2
    /// `handleGeneration` → `retireTerminalInputConnection(observedSyncToken)`).
    /// The socket that carried a batch is the only one that could answer it.
    pub fn retire_sync_generation(
        &mut self,
        socket_generation: u64,
        reason: &str,
    ) -> Vec<InputOutcome> {
        self.retire_fenced(
            |fenced| {
                fenced.transport == TerminalTransport::Sync
                    && fenced.socket_generation == socket_generation
            },
            reason,
        )
    }

    fn retire_fenced(
        &mut self,
        retired: impl Fn(&TerminalToken) -> bool,
        reason: &str,
    ) -> Vec<InputOutcome> {
        let mut outcomes = Vec::new();
        for lane in self.lanes.values_mut() {
            let matching: Vec<(u64, bool)> = lane
                .pending
                .iter()
                .filter(|pending| {
                    pending
                        .fence
                        .as_ref()
                        .is_some_and(|fence| retired(&fence.token))
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
        self.observe_settled(&outcomes);
        outcomes
    }
}

/// Settle one batch inside its own lane, keeping the byte accounting right.
pub(super) fn settle_in(lane: &mut InputLane, input_seq: u64) -> bool {
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
