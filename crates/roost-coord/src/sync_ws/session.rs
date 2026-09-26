//! One Sync v2 socket's state: its per-domain queues and generations, the
//! retention budget every queue shares, the terminal lanes, and the
//! cumulative-ACK window that bounds what may be in flight.
//!
//! Owned by the Sync link's per-connection task and reachable from nowhere
//! else. Nothing here opens a socket, reads a clock, or touches a database: the
//! bounds, the queue cutovers and the fences are the properties worth testing
//! and none of them is observable through a live socket without flakiness.
//!
//! WHY THE SHARED PRIVATE STATE IS ONE STRUCT AND NOT SEVERAL TYPES. v2 kept
//! this in seven files whose closures all reached back into one `ws.data`
//! record, and the interesting invariants -- a charge is held by exactly one of
//! {a domain queue, a lane cursor's materialization, a lane's delta tail, a
//! lane's pending states}, and every release passes through one counter -- are
//! invariants of that ONE record. Splitting the record would move the invariant
//! to a protocol between the halves, which is the shape
//! `apps/web/src/renderer/cellRenderer.ts` is not allowed to take for exactly
//! the opposite reason. The methods are split across files by CONCEPT (queue
//! ordering in `send_queue`, egress in `egress`, terminal fan-out in `terminal/`)
//! and the type is not.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use roost_proto::SyncDomain;

use super::ack_window::{AckWindow, BackpressureReason, WindowStats};
use super::domain_table::{
    domain_at_slot, domain_slot, is_lazy_slot, DomainGenerations, DomainState, AGGREGATE_MAX_RETAINED_BYTES,
    AGGREGATE_MAX_RETAINED_FRAMES, DOMAIN_SLOTS, NONTERMINAL_MAX_RETAINED_BYTES,
    NONTERMINAL_MAX_RETAINED_FRAMES, TERMINAL_CELL_MAX_RETAINED_BYTES,
    TERMINAL_CELL_MAX_RETAINED_FRAMES, TERMINAL_MAX_RETAINED_BYTES, TERMINAL_MAX_RETAINED_FRAMES,
};

use super::retained_frame::{AggregateCharge, OwnedFrame, RetainedFrame};
use super::terminal::TerminalLane;

/// Why this socket cannot stay open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SessionClose {
    /// Backpressure. The shell closes `1013` and the client renegotiates.
    #[error("sync backpressure: {reason}")]
    Backpressure {
        /// Which bound was passed.
        reason: BackpressureReason,
        /// The window's counters, for the signal the close emits.
        stats: WindowStats,
    },
    /// An acknowledgement above the last sent sequence. The shell closes
    /// `1008`, which is a policy violation and not backpressure.
    #[error("invalid sync ack")]
    InvalidAck,
}

impl From<super::ack_window::WindowClose> for SessionClose {
    fn from(close: super::ack_window::WindowClose) -> Self {
        match close {
            super::ack_window::WindowClose::Backpressure(reason, stats) => {
                Self::Backpressure { reason, stats }
            }
            super::ack_window::WindowClose::InvalidAck => Self::InvalidAck,
        }
    }
}

/// One live Sync v2 socket's whole scheduling state.
#[derive(Debug)]
pub struct SyncV2Session {
    /// The opaque identity the client echoes on every command. A command from
    /// any other socket id is ignored, so a redialled client cannot ACK or
    /// mutate the socket it left.
    pub socket_id: String,
    /// The process-wide generation source, shared with every other socket so a
    /// reset on one cannot mint a generation another is already using.
    generations: Arc<DomainGenerations>,
    /// The domain table, in the coordinator's own order. A sibling module's
    /// field access, not a public one: nothing outside this session's own
    /// methods may reorder a queue behind the retention counter's back.
    pub(in crate::sync_ws) domains: [DomainState; DOMAIN_SLOTS],
    /// Sessions whose `opened` has been delivered, which is what unfences their
    /// cells.
    pub(in crate::sync_ws) announced_sessions: BTreeSet<String>,
    /// The delivery sequence each session's announcement went out under, until
    /// it is acknowledged. A cell may not pass an unacknowledged announcement,
    /// because the client folds a cell for a session it has not been told about
    /// by discarding it.
    pub(in crate::sync_ws) pending_session_announcements: BTreeMap<String, u64>,
    /// One lane per session this socket is carrying terminal traffic for.
    pub(in crate::sync_ws) terminal_sessions: BTreeMap<String, TerminalLane>,
    /// Lanes with work, in the order they became eligible. Insertion order is
    /// the fairness rule, so this is a vector and not an ordered set: a
    /// lexicographic ring would starve the session that sorts last.
    pub(in crate::sync_ws) terminal_ready_sessions: Vec<String>,
    /// Every frame this socket retains, across domain queues and lane cursors.
    pub(in crate::sync_ws) queued_frames: usize,
    /// The same, in charged bytes.
    pub(in crate::sync_ws) queued_bytes: u64,
    pub(in crate::sync_ws) terminal_retained_frames: usize,
    pub(in crate::sync_ws) terminal_retained_bytes: u64,
    pub(in crate::sync_ws) terminal_cell_retained_frames: usize,
    pub(in crate::sync_ws) terminal_cell_retained_bytes: u64,
    /// Where the next weighted-lane pass starts.
    pub(in crate::sync_ws) lane_cursor: usize,
    /// Whether the executor owes this socket a flush turn.
    pub(in crate::sync_ws) flush_requested: bool,
    /// Set once a close has been decided. Every later admission is refused, so
    /// a teardown in flight cannot push a frame into a socket nobody will read.
    pub(in crate::sync_ws) pressure_closing: bool,
    /// The first close this session decided, kept so the caller can close the
    /// socket with a reason. The FIRST one wins because a socket closes once.
    close_fault: Option<SessionClose>,
    window: AckWindow,
}

impl SyncV2Session {
    /// A socket's state, with one fresh generation per domain.
    #[must_use]
    pub fn new(
        socket_id: String,
        generations: Arc<DomainGenerations>,
        flow_control: bool,
    ) -> Self {
        let domains = std::array::from_fn(|slot| DomainState {
            domain: domain_at_slot(slot),
            generation: generations.allocate(),
            subscribed: !is_lazy_slot(slot),
            ready: false,
            queue: Vec::new(),
            queued_bytes: 0,
            seed_insert_index: 0,
        });
        Self {
            socket_id,
            generations,
            domains,
            announced_sessions: BTreeSet::new(),
            pending_session_announcements: BTreeMap::new(),
            terminal_sessions: BTreeMap::new(),
            terminal_ready_sessions: Vec::new(),
            queued_frames: 0,
            queued_bytes: 0,
            terminal_retained_frames: 0,
            terminal_retained_bytes: 0,
            terminal_cell_retained_frames: 0,
            terminal_cell_retained_bytes: 0,
            lane_cursor: 0,
            flush_requested: false,
            pressure_closing: false,
            close_fault: None,
            window: AckWindow::new(flow_control),
        }
    }

    /// The next generation from this process's monotonic source, for a domain
    /// transition that mints one outside [`Self::reset_domain`].
    pub(in crate::sync_ws) fn allocate_generation(&self) -> u64 {
        self.generations.allocate()
    }

    /// Record that this socket cannot stay open. The first fault wins, because
    /// a socket closes exactly once and the first reason is the one that
    /// describes what actually stopped it.
    pub(in crate::sync_ws) fn fault(&mut self, close: SessionClose) {
        self.pressure_closing = true;
        self.flush_requested = false;
        if self.close_fault.is_none() {
            self.close_fault = Some(close);
        }
    }

    /// The close this session decided, for the socket to perform.
    #[must_use]
    pub fn close_fault(&self) -> Option<SessionClose> {
        self.close_fault
    }

    /// Take the close this session decided, so it is reported once.
    pub fn take_close_fault(&mut self) -> Option<SessionClose> {
        self.close_fault.take()
    }

    /// The last cumulative acknowledgement this socket accepted.
    #[must_use]
    pub fn acknowledged_sequence(&self) -> u64 {
        self.window.acknowledged()
    }

    /// The sequence the next application frame will carry.
    #[must_use]
    pub fn next_delivery_seq(&self) -> u64 {
        self.window.next_sequence()
    }

    /// Whether one more frame of `encoded_bytes` fits the unacknowledged
    /// window, ignoring the age deadline the delivery timer owns.
    #[must_use]
    pub fn window_has_room(&self, encoded_bytes: u64) -> bool {
        self.window.has_room(encoded_bytes)
    }

    /// One domain's state for mutation, or `None` for a value that is not a
    /// domain.
    pub(in crate::sync_ws) fn domain_mut(
        &mut self,
        domain: SyncDomain,
    ) -> Option<&mut DomainState> {
        self.domains.get_mut(domain_slot(domain))
    }

    /// One domain's state, or `None` for a value that is not a domain.
    pub(in crate::sync_ws) fn domain(&self, domain: SyncDomain) -> Option<&DomainState> {
        self.domains.get(domain_slot(domain))
    }

    /// One domain's current generation, for correlating a reset.
    pub fn domain_generation(&self, domain: SyncDomain) -> Option<u64> {
        self.domain(domain).map(|state| state.generation)
    }

    /// Whether the terminal domain has closed its snapshot/live gap.
    pub fn terminal_domain_ready(&self) -> bool {
        self.domain(SyncDomain::Terminal).is_some_and(|state| state.ready)
    }

    /// The cumulative-ACK window's counters, for the close signal.
    pub fn window_stats(&self, now_ms: u64) -> WindowStats {
        self.window.stats(now_ms)
    }

    /// Ask the executor for a flush turn. Idempotent, because the whole point
    /// is that many enqueues between two turns cost one.
    pub fn request_flush(&mut self) {
        if !self.pressure_closing {
            self.flush_requested = true;
        }
    }

    /// Take the pending flush request, if there is one.
    pub fn take_flush_request(&mut self) -> bool {
        std::mem::take(&mut self.flush_requested)
    }

    /// Charge one frame against the socket's retention budget, or refuse it.
    ///
    /// FOUR LIMITS, NOT ONE, because they bound different resources: the
    /// terminal half and the non-terminal half are separately bounded so a
    /// streaming terminal cannot consume the workspace feed's budget; cell
    /// material has a smaller bound again so state frames always have room;
    /// and the two halves sum to a socket-wide bound. The order of the tests is
    /// not observable, the decision is (`sync-ws-v2-state.ts:154-199`).
    pub(in crate::sync_ws) fn try_charge(&mut self, frame: &OwnedFrame) -> Option<AggregateCharge> {
        let estimated_bytes = frame.conservative_bytes();
        let terminal = frame.domain() == SyncDomain::Terminal;
        let terminal_cell = terminal && frame.is_cell_material();
        let (retained_frames, retained_bytes, frame_limit, byte_limit) = if terminal {
            (
                self.terminal_retained_frames,
                self.terminal_retained_bytes,
                TERMINAL_MAX_RETAINED_FRAMES,
                TERMINAL_MAX_RETAINED_BYTES,
            )
        } else {
            (
                self.queued_frames - self.terminal_retained_frames,
                self.queued_bytes - self.terminal_retained_bytes,
                NONTERMINAL_MAX_RETAINED_FRAMES,
                NONTERMINAL_MAX_RETAINED_BYTES,
            )
        };
        let over_half = retained_frames + 1 > frame_limit
            || retained_bytes + estimated_bytes > byte_limit;
        let over_cells = terminal_cell
            && (self.terminal_cell_retained_frames + 1 > TERMINAL_CELL_MAX_RETAINED_FRAMES
                || self.terminal_cell_retained_bytes + estimated_bytes
                    > TERMINAL_CELL_MAX_RETAINED_BYTES);
        let over_socket = self.queued_frames + 1 > AGGREGATE_MAX_RETAINED_FRAMES
            || self.queued_bytes + estimated_bytes > AGGREGATE_MAX_RETAINED_BYTES;
        if over_half || over_cells || over_socket {
            return None;
        }
        self.queued_frames += 1;
        self.queued_bytes += estimated_bytes;
        if terminal {
            self.terminal_retained_frames += 1;
            self.terminal_retained_bytes += estimated_bytes;
        }
        if terminal_cell {
            self.terminal_cell_retained_frames += 1;
            self.terminal_cell_retained_bytes += estimated_bytes;
        }
        Some(AggregateCharge {
            estimated_bytes,
            terminal,
            terminal_cell,
        })
    }

    /// Give back what one retained frame was charged.
    pub(in crate::sync_ws) fn release_charge(&mut self, retained: &mut RetainedFrame) {
        let Some(charge) = retained.take_charge() else {
            return;
        };
        self.queued_frames = self.queued_frames.saturating_sub(1);
        self.queued_bytes = self.queued_bytes.saturating_sub(charge.estimated_bytes);
        if charge.terminal {
            self.terminal_retained_frames = self.terminal_retained_frames.saturating_sub(1);
            self.terminal_retained_bytes =
                self.terminal_retained_bytes.saturating_sub(charge.estimated_bytes);
        }
        if charge.terminal_cell {
            self.terminal_cell_retained_frames =
                self.terminal_cell_retained_frames.saturating_sub(1);
            self.terminal_cell_retained_bytes =
                self.terminal_cell_retained_bytes.saturating_sub(charge.estimated_bytes);
        }
    }

    /// Apply a cumulative acknowledgement, releasing the window and every
    /// pending session announcement it covers.
    ///
    /// Releasing an announcement here is what unfences a session's cells: the
    /// client has now processed the `opened` event, so a cell for it is no
    /// longer a cell for a session it has never heard of.
    pub fn apply_ack(&mut self, ack_seq: u64, now_ms: u64) -> Result<u64, SessionClose> {
        let released = self.window.apply_ack(ack_seq, now_ms)?;
        self.pending_session_announcements
            .retain(|_, announced_at| *announced_at > ack_seq);
        self.request_flush();
        Ok(released)
    }

    /// Whether the window will admit one more frame of `encoded_bytes`.
    pub fn may_send(&self, encoded_bytes: u64, now_ms: u64) -> Result<(), SessionClose> {
        self.window.may_send(encoded_bytes, now_ms).map_err(Into::into)
    }

    /// Record a frame the socket accepted, consuming its delivery sequence.
    pub fn record_sent(&mut self, encoded_bytes: u64, now_ms: u64) -> u64 {
        self.window.record_sent(encoded_bytes, now_ms)
    }

    /// Mark this socket as closing. Every later admission is refused, and the
    /// queued frames are released so the budget reads zero before teardown.
    pub fn retire(&mut self) {
        self.pressure_closing = true;
        self.flush_requested = false;
        self.clear();
    }

    /// Release every retained frame and empty every queue. The socket stays
    /// alive afterwards, which is what a domain reset needs.
    pub fn clear(&mut self) {
        for slot in 0..DOMAIN_SLOTS {
            let queued = std::mem::take(&mut self.domains[slot].queue);
            let state = &mut self.domains[slot];
            state.queued_bytes = 0;
            state.seed_insert_index = 0;
            state.ready = false;
            for mut frame in queued {
                self.release_charge(&mut frame);
            }
        }
        let lanes = std::mem::take(&mut self.terminal_sessions);
        for mut lane in lanes.into_values() {
            lane.release_all(self);
        }
        self.terminal_ready_sessions.clear();
        self.announced_sessions.clear();
        self.pending_session_announcements.clear();
        self.queued_frames = 0;
        self.queued_bytes = 0;
        self.terminal_retained_frames = 0;
        self.terminal_retained_bytes = 0;
        self.terminal_cell_retained_frames = 0;
        self.terminal_cell_retained_bytes = 0;
        self.window.clear();
    }
}
