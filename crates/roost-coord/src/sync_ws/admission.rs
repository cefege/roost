//! Bounded queue admission: whether one frame may join a domain's queue, where
//! it lands in that queue, and what an overflow means for that domain.
//!
//! Owned by the Sync session, and split from `egress.rs` by concept rather than
//! only by the line cap: admission answers "may this frame be held", the flush
//! turn answers "which held frame goes out next", and the two are separately
//! testable. v2 had both in `sync-ws-v2-egress.ts` (396 lines), which is exactly
//! the file that does not survive a 400-line cap as a transliteration.
//!
//! EVERY REFUSAL IS DECIDED BEFORE THE FRAME IS CONSUMED. A lane that cannot
//! queue its next view-state has to keep holding it -- v2 leaves the frame in
//! `pendingStates` and retries on the next turn, and a port that consumed it
//! would drop a state the client is waiting an answer to. So the refusal is a
//! value computed from an immutable read, and only a frame that passed is
//! charged and moved.

use roost_proto::{FirehoseFrame, SyncDomain};

use crate::sync_ws::ack_window::BackpressureReason;
use crate::sync_ws::control_frames::{control_frame, ResetNotice};
use crate::sync_ws::domain_table::{domain_slot, DOMAIN_MAX_QUEUED_BYTES, DOMAIN_MAX_QUEUED_FRAMES};
use crate::sync_ws::frame_meta::{FeedLane, SyncFrameMeta};
use crate::sync_ws::retained_frame::{OwnedFrame, RetainedFrame};
use crate::sync_ws::session::{SessionClose, SyncV2Session};
use crate::sync_ws::terminal::snapshot::TerminalSnapshotHub;

use super::send_queue::{retained_supersedes_buffered, terminal_priority_insert_index};

/// What one admission attempt did.
#[derive(Debug)]
pub enum EnqueueOutcome {
    /// The socket now holds the frame and owes a flush turn.
    Queued,
    /// A control-lane frame: send it unsequenced, and do not queue it.
    Control(FirehoseFrame),
    /// Not this socket's: an unsubscribed domain, a stale generation, or a
    /// socket that is already closing.
    Dropped,
    /// The domain overflowed and was reset. The caller sends
    /// [`ResetNotice::to_frame`] and the feed re-seeds the domain.
    Reset(ResetNotice),
    /// The socket must close.
    Fault(SessionClose),
}

impl EnqueueOutcome {
    /// Whether the frame is now queued.
    #[must_use]
    pub fn is_queued(&self) -> bool {
        matches!(self, Self::Queued)
    }
}

/// Why one frame cannot join a domain's queue right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueueRefusal {
    /// The socket is already closing.
    Closing,
    /// A control, or a domain this socket never subscribed to.
    NotSubscribed,
    /// The frame was retained under a generation the domain has moved past.
    StaleGeneration,
    /// The domain's queue, or the socket's retention budget, is full.
    /// `rebaseline` is the difference between the two recoveries: a fresh
    /// canonical full for ONE session (a dropped cell, which output can be
    /// rebuilt from) and a domain reset (a non-terminal projection, which the
    /// client rebuilds from its durable log).
    Overflow { rebaseline: bool },
    /// A terminal SEMANTIC frame is full and cannot be dropped.
    TerminalSemanticOverflow,
}

impl SyncV2Session {
    /// Admit one frame from the feed, or say why not.
    ///
    /// `meta` of `None` means a control: controls are unsequenced, are never
    /// queued, and never consume the window, so they go straight to the socket.
    pub fn enqueue_frame(
        &mut self,
        frame: &FirehoseFrame,
        meta: Option<&SyncFrameMeta>,
        now_ms: u64,
        hub: &mut dyn TerminalSnapshotHub,
    ) -> EnqueueOutcome {
        if self.pressure_closing {
            return EnqueueOutcome::Dropped;
        }
        let meta = match meta {
            Some(meta) if meta.domain.is_some() && meta.lane != FeedLane::Control => meta,
            _ => return EnqueueOutcome::Control(control_frame(frame.clone())),
        };
        let domain_id = meta.domain.unwrap_or(SyncDomain::Unspecified);
        let generation = self.domain_generation(domain_id).unwrap_or_default();
        let owned = OwnedFrame::of_copy(frame, domain_id, generation);
        if meta.before_buffered {
            self.coalesce_buffered(domain_id, &owned);
        }
        let estimated_bytes = owned.conservative_bytes();
        match self.queue_refusal(&owned, meta, estimated_bytes, true) {
            Some(refusal) => self.act_on_refusal(refusal, meta, now_ms, hub),
            None => self.admit(owned, meta, estimated_bytes, now_ms),
        }
    }

    /// Queue a frame the terminal lane already charged, or hand it back.
    ///
    /// The frame comes back on every refusal so the lane can keep holding it,
    /// which is the difference between a state that is retried and a client that
    /// waits out its deadline. The generation check is the fence from
    /// `sync-ws-v2-egress.ts:170-183`: a frame retained under a generation this
    /// domain has since moved past was admitted against a view of the world the
    /// client has already reset away.
    pub(in crate::sync_ws) fn try_enqueue_lane_frame(
        &mut self,
        mut retained: RetainedFrame,
        meta: &SyncFrameMeta,
        now_ms: u64,
        hub: &mut dyn TerminalSnapshotHub,
    ) -> Result<(), Box<RetainedFrame>> {
        let estimated_bytes = retained.estimated_bytes();
        match self.queue_refusal(&retained.frame, meta, estimated_bytes, false) {
            None => {
                retained.queued_at_ms = now_ms;
                self.insert_queued(retained, meta, estimated_bytes);
                self.request_flush();
                Ok(())
            }
            Some(refusal) => {
                if refusal
                    == (QueueRefusal::Overflow {
                        rebaseline: true,
                    })
                {
                    // v2 asks the hub from inside the queue admission, and the
                    // ask is what makes the drop recoverable: a fresh canonical
                    // full for the session replaces the cells this frame was
                    // carrying.
                    self.request_rebaseline_for(meta, hub);
                }
                Err(Box::new(retained))
            }
        }
    }

    /// Whether this frame may join its domain's queue, and if not, why.
    /// `check_budget` is false for a frame the lane already charged: v2 does not
    /// re-check the socket's retention budget on the retained path, because the
    /// charge is already held and re-checking it would count the same bytes
    /// twice.
    fn queue_refusal(
        &self,
        frame: &OwnedFrame,
        meta: &SyncFrameMeta,
        estimated_bytes: u64,
        check_budget: bool,
    ) -> Option<QueueRefusal> {
        if self.pressure_closing {
            return Some(QueueRefusal::Closing);
        }
        if meta.domain.is_none() || meta.lane == FeedLane::Control {
            return Some(QueueRefusal::NotSubscribed);
        }
        let domain_id = meta.domain?;
        let state = self.domain(domain_id)?;
        if !state.subscribed {
            return Some(QueueRefusal::NotSubscribed);
        }
        if !check_budget && frame.generation() != state.generation {
            return Some(QueueRefusal::StaleGeneration);
        }
        let terminal_cell = domain_id == SyncDomain::Terminal && frame.is_cell_material();
        let exceeds_domain = state.queue.len() + 1 > DOMAIN_MAX_QUEUED_FRAMES
            || state.queued_bytes + estimated_bytes > DOMAIN_MAX_QUEUED_BYTES;
        if exceeds_domain {
            return Some(classify_overflow(domain_id, terminal_cell));
        }
        if check_budget {
            let frame_count = self.queued_frames + 1;
            let byte_count = self.queued_bytes + estimated_bytes;
            let half = if domain_id == SyncDomain::Terminal {
                (
                    self.terminal_retained_frames + 1 > crate::sync_ws::domain_table::TERMINAL_MAX_RETAINED_FRAMES,
                    self.terminal_retained_bytes + estimated_bytes
                        > crate::sync_ws::domain_table::TERMINAL_MAX_RETAINED_BYTES,
                )
            } else {
                (
                    self.queued_frames - self.terminal_retained_frames + 1
                        > crate::sync_ws::domain_table::NONTERMINAL_MAX_RETAINED_FRAMES,
                    self.queued_bytes - self.terminal_retained_bytes + estimated_bytes
                        > crate::sync_ws::domain_table::NONTERMINAL_MAX_RETAINED_BYTES,
                )
            };
            let cells = terminal_cell
                && (self.terminal_cell_retained_frames + 1
                    > crate::sync_ws::domain_table::TERMINAL_CELL_MAX_RETAINED_FRAMES
                    || self.terminal_cell_retained_bytes + estimated_bytes
                        > crate::sync_ws::domain_table::TERMINAL_CELL_MAX_RETAINED_BYTES);
            let socket = frame_count > crate::sync_ws::domain_table::AGGREGATE_MAX_RETAINED_FRAMES
                || byte_count > crate::sync_ws::domain_table::AGGREGATE_MAX_RETAINED_BYTES;
            if half.0 || half.1 || cells || socket {
                return Some(classify_overflow(domain_id, terminal_cell));
            }
        }
        None
    }

    /// Charge a frame and put it in its domain's queue. The refusal check has
    /// already passed, so the charge cannot fail.
    fn admit(
        &mut self,
        frame: OwnedFrame,
        meta: &SyncFrameMeta,
        estimated_bytes: u64,
        now_ms: u64,
    ) -> EnqueueOutcome {
        let Some(charge) = self.try_charge(&frame) else {
            return EnqueueOutcome::Dropped;
        };
        let item = RetainedFrame::new(frame, meta.clone(), now_ms, charge);
        self.insert_queued(item, meta, estimated_bytes);
        self.request_flush();
        EnqueueOutcome::Queued
    }

    /// What a refused frame means for the socket, from the FEED's side, where
    /// the frame is this feed's own copy and can be dropped.
    ///
    /// THE TERMINAL DOMAIN IS THE EXCEPTION, AND THE REASON IS WHAT A CLIENT
    /// CAN RECOVER FROM. A non-terminal domain is a projection the client
    /// rebuilds from its durable log, so its queue overflowing resets the domain
    /// and the client re-hydrates. Terminal CELLS cannot be rebuilt: dropping
    /// one loses output, so an overflowing cell frame asks the hub for a fresh
    /// canonical full for THAT session and drops the frame. Terminal SEMANTIC
    /// state -- a view-state, which answers a command the client is waiting on --
    /// closes the socket instead, because a dropped answer is a client that
    /// waits out its deadline believing a terminal is attached
    /// (`sync-ws-v2-egress.ts:96-140`).
    fn act_on_refusal(
        &mut self,
        refusal: QueueRefusal,
        meta: &SyncFrameMeta,
        now_ms: u64,
        hub: &mut dyn TerminalSnapshotHub,
    ) -> EnqueueOutcome {
        let domain_id = meta.domain.unwrap_or(SyncDomain::Unspecified);
        match refusal {
            QueueRefusal::Closing | QueueRefusal::NotSubscribed | QueueRefusal::StaleGeneration => {
                EnqueueOutcome::Dropped
            }
            QueueRefusal::Overflow { rebaseline: true } => {
                self.request_rebaseline_for(meta, hub);
                EnqueueOutcome::Dropped
            }
            QueueRefusal::Overflow { rebaseline: false } => {
                self.reset_domain(domain_id, "queue_overflow")
            }
            QueueRefusal::TerminalSemanticOverflow => {
                let close = SessionClose::Backpressure {
                    reason: BackpressureReason::ByteLimit,
                    stats: self.window_stats(now_ms),
                };
                self.fault(close);
                EnqueueOutcome::Fault(close)
            }
        }
    }

    /// Ask the hub for a canonical full for the session this frame was about.
    fn request_rebaseline_for(
        &mut self,
        meta: &SyncFrameMeta,
        hub: &mut dyn TerminalSnapshotHub,
    ) {
        if let Some(session_id) = meta.session_id.as_deref() {
            let socket_id = self.socket_id.clone();
            hub.request_rebaseline(&socket_id, session_id);
        }
    }

    /// Put a charged frame in its domain's queue, at the position its metadata
    /// demands: AT the seed cutover for a retained sample, past this session's
    /// own frames and every snapshot for a baseline or a view-state, and at the
    /// back for everything else.
    pub(in crate::sync_ws) fn insert_queued(
        &mut self,
        item: RetainedFrame,
        meta: &SyncFrameMeta,
        estimated_bytes: u64,
    ) {
        let Some(domain) = meta.domain else {
            return;
        };
        let Some(state) = self.domain_mut(domain) else {
            return;
        };
        if meta.before_buffered {
            // A retained seed is inserted AT the cutover, so it precedes every
            // live frame the domain buffered while the client was hydrating.
            state.queue.insert(state.seed_insert_index, item);
            state.seed_insert_index += 1;
        } else if meta.attach_snapshot || meta.terminal_state {
            let insert_index = terminal_priority_insert_index(&state.queue, meta.session_id.as_deref());
            state.queue.insert(insert_index, item);
            if insert_index < state.seed_insert_index {
                state.seed_insert_index += 1;
            }
        } else {
            state.queue.push(item);
        }
        state.queued_bytes += estimated_bytes;
    }

    /// Drop the buffered frames a retained seed supersedes.
    ///
    /// Only the LIVE segment is coalesced: the retained samples already before
    /// the cutover ARE the cutover, and dropping one of those would reopen the
    /// snapshot/live gap this ordering exists to close.
    fn coalesce_buffered(&mut self, domain_id: SyncDomain, retained: &OwnedFrame) {
        let slot = domain_slot(domain_id);
        let seed_index = self.domains[slot].seed_insert_index;
        let queued = std::mem::take(&mut self.domains[slot].queue);
        let mut kept = Vec::with_capacity(queued.len());
        let mut superseded_bytes = 0u64;
        for (index, mut item) in queued.into_iter().enumerate() {
            if index >= seed_index && retained_supersedes_buffered(retained, &item.frame) {
                superseded_bytes += item.estimated_bytes();
                self.release_charge(&mut item);
                continue;
            }
            kept.push(item);
        }
        let domain = &mut self.domains[slot];
        domain.queue = kept;
        domain.queued_bytes = domain.queued_bytes.saturating_sub(superseded_bytes);
    }

    /// Reset one domain: clear its queue, mint a generation, and tell the caller
    /// what to announce.
    ///
    /// The terminal domain takes its lanes with it, because a lane's baseline
    /// cursor is stamped with the generation it was admitted under and would
    /// otherwise deliver a baseline the client has already reset away.
    pub fn reset_domain(&mut self, domain_id: SyncDomain, reason: &'static str) -> EnqueueOutcome {
        let Some(state) = self.domain(domain_id) else {
            return EnqueueOutcome::Dropped;
        };
        let subscribed = state.subscribed;
        self.clear_domain_queue(domain_id);
        let terminal_sessions_dropped = domain_id == SyncDomain::Terminal;
        if terminal_sessions_dropped {
            self.announced_sessions.clear();
            self.pending_session_announcements.clear();
            self.clear_terminal_sessions();
        }
        let generation = self.allocate_generation();
        let Some(state) = self.domain_mut(domain_id) else {
            return EnqueueOutcome::Dropped;
        };
        state.generation = generation;
        state.ready = false;
        EnqueueOutcome::Reset(ResetNotice {
            domain: domain_id,
            generation,
            reason,
            subscribed,
            terminal_sessions_dropped,
        })
    }
}

/// Which recovery a full queue gets.
fn classify_overflow(domain_id: SyncDomain, terminal_cell: bool) -> QueueRefusal {
    if domain_id != SyncDomain::Terminal {
        return QueueRefusal::Overflow { rebaseline: false };
    }
    if terminal_cell {
        return QueueRefusal::Overflow { rebaseline: true };
    }
    QueueRefusal::TerminalSemanticOverflow
}
