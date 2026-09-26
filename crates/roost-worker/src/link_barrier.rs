//! The coordinator link's application barrier: the states a link moves through
//! before it may carry live traffic, and the rules about what may be written
//! when.
//!
//! Owned by the worker. This is a pure state machine with no socket in it, so
//! the ordering rules are testable without a network — and these are ordering
//! rules, which is exactly the class that is never tested by hand.
//!
//! The contract is `protocol/spec/worker-link.md` §State machine, and this
//! file is where its three-stage barrier actually lives. The shape is:
//!
//! ```text
//!   idle ──dial──▶ open ──▶ hello ──DHelloAck──▶ replay ──▶ snapshot ──ack──▶ live
//! ```
//!
//! `open` is NOT application-ready. The only forced first write is the hello.
//! Everything after that is gated on the coordinator acknowledging the previous
//! step, because each step establishes something the next one relies on.

use std::collections::VecDeque;

/// Where a link is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Barrier {
    /// No socket. Nothing may be written.
    Idle,
    /// Socket is up. NOT application-ready: the only permitted write is the
    /// hello, and anything else here is a bug rather than a race.
    Open,
    /// Hello sent, waiting for the coordinator to acknowledge it.
    Hello,
    /// Replaying durable events, exactly one in flight.
    Replay,
    /// A snapshot is in flight.
    Snapshot,
    /// Live traffic may flow.
    Live,
}

impl Barrier {
    /// Whether live traffic is permitted.
    ///
    /// The answer is `Live` and nothing else. A link that has merely opened is
    /// not ready, and a caller that treated "open" as "ready" would put cells
    /// on the wire before the coordinator knew the worker existed.
    pub fn allows_live_traffic(self) -> bool {
        self == Barrier::Live
    }

    /// Whether durable events may be written.
    pub fn allows_durable_write(self) -> bool {
        matches!(self, Barrier::Live)
    }
}

/// What the pump should do after an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Nothing. The machine is waiting on the coordinator.
    Wait,
    /// Send the hello. The only forced first write.
    SendHello,
    /// Write the next durable event, which the machine has selected.
    WriteDurable {
        /// The sequence the coordinator will ACK this under.
        seq: u64,
    },
    /// Write the authoritative snapshot.
    WriteSnapshot,
    /// The coordinator acknowledged something the machine was not waiting for.
    ///
    /// Not an error: a duplicate or stale ACK is normal on a reconnect, and
    /// closing on one would turn a benign duplicate into an outage.
    IgnoredAck { seq: u64 },
}

/// The coordinator link's barrier and the bookkeeping that enforces it.
#[derive(Debug)]
pub struct Pump {
    barrier: Barrier,
    /// Durable events not yet acknowledged, oldest first.
    ///
    /// ONE is in flight at a time. The coordinator ACKs each durable event with
    /// its exact sequence, so a second in flight would make the two ACKs
    /// ambiguous — and an ambiguous ACK is an event that may or may not have
    /// been written, which is the one state a durable path cannot recover from
    /// on its own.
    durable: VecDeque<PendingEvent>,
    /// Whether a durable event is currently awaiting its ACK.
    in_flight: Option<u64>,
    /// A durable event or blocking reservation that appeared while a snapshot
    /// was in flight.
    ///
    /// This is the subtle one. A snapshot taken before an event is durable
    /// describes a state that has already moved on, so the snapshot must be
    /// retaken — the barrier goes back to `replay` rather than to `live`.
    replay_again: bool,
    /// The sequence the in-flight snapshot will be acknowledged under.
    snapshot_seq: Option<u64>,
    next_seq: u64,
    /// Durable events acknowledged so far, for `roost doctor` and for the
    /// tests that assert pacing.
    acked: u64,
}

/// A durable event waiting to be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingEvent {
    pub seq: u64,
    /// The encoded frame. The pump never inspects it; the sequence is the
    /// whole contract, so a clone is only paid when a caller asks for one.
    pub bytes: Vec<u8>,
}

impl Default for Pump {
    fn default() -> Self {
        Self::new()
    }
}

impl Pump {
    pub fn new() -> Self {
        Self {
            barrier: Barrier::Idle,
            durable: VecDeque::new(),
            in_flight: None,
            replay_again: false,
            snapshot_seq: None,
            next_seq: 1,
            acked: 0,
        }
    }

    pub fn barrier(&self) -> Barrier {
        self.barrier
    }

    /// Durable events still waiting to reach the coordinator. Not cleared by a
    /// disconnect: a durable row that vanished on reconnect would be a hole in
    /// the coordinator's record of what happened.
    pub fn durable_pending(&self) -> usize {
        self.durable.len()
    }

    pub fn durable_acked(&self) -> u64 {
        self.acked
    }

    /// The socket came up. Not application-ready.
    pub fn on_open(&mut self) -> Action {
        self.barrier = Barrier::Open;
        self.in_flight = None;
        self.snapshot_seq = None;
        Action::SendHello
    }

    /// Queue a durable event. Admitted in every state, because durability does
    /// not wait on the network — but it is only WRITTEN once the barrier allows
    /// it, and its arrival during a snapshot changes where the barrier goes.
    pub fn enqueue_durable(&mut self, bytes: Vec<u8>) -> Action {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.durable.push_back(PendingEvent { seq, bytes });
        // Arriving DURING a snapshot is the case that matters: the snapshot in
        // flight describes a state that has already moved on, so acknowledging
        // it must not admit live traffic.
        if self.barrier == Barrier::Snapshot {
            self.replay_again = true;
        }
        self.advance()
    }

    /// A durable event or blocking reservation appeared, without bytes of its
    /// own. Same effect on the barrier as [`Pump::enqueue_durable`], and
    /// separated because the reservation is taken BEFORE the event exists.
    pub fn note_durable_appeared(&mut self) -> Action {
        if self.barrier == Barrier::Snapshot {
            self.replay_again = true;
        }
        self.advance()
    }

    /// The coordinator acknowledged the hello.
    pub fn on_hello_ack(&mut self) -> Action {
        self.barrier = Barrier::Replay;
        self.advance()
    }

    /// The coordinator acknowledged a durable event.
    ///
    /// `seq` must be the exact sequence in flight. A stale or duplicate ACK
    /// cannot release the barrier: a barrier released by the wrong ACK admits
    /// live traffic over a durable event the coordinator never confirmed, which
    /// is the at-least-once violation this whole design exists to prevent.
    pub fn on_event_ack(&mut self, seq: u64) -> Action {
        match self.in_flight {
            Some(expected) if expected == seq => {
                self.in_flight = None;
                self.durable.pop_front();
                self.acked = self.acked.max(seq);
                self.advance()
            }
            _ => Action::IgnoredAck { seq },
        }
    }

    /// The coordinator acknowledged the snapshot.
    pub fn on_snapshot_ack(&mut self, seq: u64) -> Action {
        match self.snapshot_seq {
            Some(expected) if expected == seq => {
                self.snapshot_seq = None;
                // A durable event that appeared while the snapshot was in
                // flight means the snapshot described a state that has already
                // moved on. Acknowledging it does NOT make the link live; the
                // barrier goes back to replay so the event reaches the
                // coordinator before anything else does.
                if self.replay_again {
                    self.replay_again = false;
                    self.barrier = Barrier::Replay;
                } else {
                    self.barrier = Barrier::Live;
                }
                self.advance()
            }
            _ => Action::IgnoredAck { seq },
        }
    }

    /// The socket ended. Application state resets; durable rows do not.
    pub fn on_disconnect(&mut self) -> Barrier {
        let previous = self.barrier;
        self.barrier = Barrier::Idle;
        self.in_flight = None;
        self.snapshot_seq = None;
        self.replay_again = false;
        previous
    }

    /// Choose the next thing to write, given where the barrier is.
    ///
    /// Every arm either returns an action or returns without one; none of them
    /// loops, because each state transition is made by the call that observed
    /// the coordinator's answer rather than by walking forward here.
    fn advance(&mut self) -> Action {
        match self.barrier {
            // The only forced first write is the hello, and that is `on_open`'s
            // job. Nothing else may go out from here.
            Barrier::Open | Barrier::Hello | Barrier::Idle => Action::Wait,
            // A live link still owes the coordinator its durable events, and
            // still only one at a time. What it does NOT do is take a snapshot
            // again: the snapshot exists to establish the state a reconnecting
            // worker could not describe, and a live link already has.
            Barrier::Live => {
                if self.in_flight.is_none()
                    && let Some(next) = self.durable.front()
                {
                    self.in_flight = Some(next.seq);
                    return Action::WriteDurable { seq: next.seq };
                }
                Action::Wait
            }
            Barrier::Replay => {
                if self.in_flight.is_none()
                    && let Some(next) = self.durable.front()
                {
                    self.in_flight = Some(next.seq);
                    return Action::WriteDurable { seq: next.seq };
                }
                if self.durable.is_empty() {
                    // Nothing to replay and nothing blocked: the snapshot is
                    // what establishes an authoritative state.
                    //
                    // The snapshot draws from the SAME sequence space as the
                    // durable events, and consumes one. It is acknowledged
                    // like they are, so a shared sequence that did not advance
                    // would let a durable event and the snapshot claim the same
                    // number — and two things with one identity is an ambiguous
                    // acknowledgement, which is the state a durable path cannot
                    // recover from.
                    self.barrier = Barrier::Snapshot;
                    self.snapshot_seq = Some(self.next_seq);
                    self.next_seq += 1;
                    return Action::WriteSnapshot;
                }
                Action::Wait
            }
            // While a snapshot is in flight the only thing to do is wait for
            // its acknowledgement; whether that acknowledgement releases the
            // barrier depends on `replay_again`, which `on_snapshot_ack`
            // decides.
            Barrier::Snapshot => Action::Wait,
        }
    }
}
