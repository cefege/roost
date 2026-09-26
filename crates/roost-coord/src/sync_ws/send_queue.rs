//! Which queued frame goes next, and the ordering rules that decide it: the
//! current-state seed cutover, the announcement fence, the weighted-lane
//! round-robin, and the per-session removals.
//!
//! Owned by the Sync session. v2 kept this in `sync-ws-v2-queue.ts` and called
//! it "not coupled to socket writes"; that separation is the reason the
//! ordering is testable here, and it is preserved: every function in this file
//! answers a question about ORDER, and none of them sends anything.
//!
//! THE WEIGHTED LANE IS A ROUND ROBIN WITH AN AGE OVERRIDE, NOT A PRIORITY
//! QUEUE. A streaming terminal would starve every other domain if cells simply
//! won, so the cursor visits each lane a fixed number of times per pass, and a
//! non-cell frame that has waited past [`LOW_LANE_MAX_AGE_MS`] outranks cells
//! outright. The override is one-directional on purpose: a cell that waited
//! three seconds does not get to jump the line, because unbounded cell delay is
//! exactly the state the age rule exists to bound.

use std::collections::{BTreeMap, BTreeSet};

use roost_proto::SyncDomain;

use super::domain_table::{LOW_LANE_MAX_AGE_MS, domain_slot};
use super::frame_meta::{FeedLane, WEIGHTED_LANES};
use super::retained_frame::RetainedFrame;
use super::session::SyncV2Session;

/// One frame the scheduler has chosen, named by where it sits rather than by
/// what it holds, so the caller can remove exactly that frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    /// Which domain's queue.
    pub slot: usize,
    /// Which index in it.
    pub index: usize,
}

/// How a candidate was chosen, for a log line that has to explain a stall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionRule {
    /// A non-cell frame outranked every cell because it waited too long.
    AgedOut,
    /// The weighted-lane round robin picked this lane.
    WeightedLane,
    /// Nothing matched a lane, so the oldest frame went.
    Oldest,
}

/// A candidate and the rule that chose it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// Which frame.
    pub candidate: Candidate,
    /// Why it beat the others.
    pub rule: SelectionRule,
}

impl SyncV2Session {
    /// The next frame to send, or `None` when nothing is eligible.
    ///
    /// Two passes, in this order, and the order is the policy: an overdue
    /// non-cell frame first, then the weighted round robin, then the oldest
    /// frame as a floor for a lane the round robin does not cover.
    pub(in crate::sync_ws) fn select_candidate(
        &mut self,
        now_ms: u64,
        ack_seq: u64,
    ) -> Option<Selection> {
        let Self {
            domains,
            announced_sessions,
            pending_session_announcements,
            lane_cursor,
            ..
        } = self;
        let mut heads: Vec<(Candidate, FeedLane, u64)> = Vec::new();
        for (slot, domain) in domains.iter().enumerate() {
            if !domain.subscribed || !domain.ready {
                continue;
            }
            // The first ELIGIBLE frame at a domain's head. Stepping past a
            // fenced cell to reach its own announcement is the point; two
            // eligible frames are never reordered.
            for (index, item) in domain.queue.iter().enumerate() {
                if !is_eligible(
                    item,
                    announced_sessions,
                    pending_session_announcements,
                    ack_seq,
                ) {
                    continue;
                }
                heads.push((Candidate { slot, index }, item.meta.lane, item.queued_at_ms));
                break;
            }
        }
        if heads.is_empty() {
            return None;
        }

        if let Some(overdue) = heads
            .iter()
            .filter(|(_, lane, queued_at_ms)| {
                *lane != FeedLane::Cell
                    && now_ms.saturating_sub(*queued_at_ms) >= LOW_LANE_MAX_AGE_MS
            })
            .min_by_key(|(_, _, queued_at_ms)| *queued_at_ms)
        {
            return Some(Selection {
                candidate: overdue.0,
                rule: SelectionRule::AgedOut,
            });
        }

        for _ in 0..WEIGHTED_LANES.len() {
            let lane = WEIGHTED_LANES[*lane_cursor];
            *lane_cursor = (*lane_cursor + 1) % WEIGHTED_LANES.len();
            if let Some(selected) = heads
                .iter()
                .filter(|(_, head_lane, _)| *head_lane == lane)
                .min_by_key(|(_, _, queued_at_ms)| *queued_at_ms)
            {
                return Some(Selection {
                    candidate: selected.0,
                    rule: SelectionRule::WeightedLane,
                });
            }
        }
        // A frame in a lane the round robin does not visit -- a control, which
        // is never queued in the first place. Kept as a floor so selection
        // cannot report "nothing to send" while something is queued.
        let oldest = heads
            .iter()
            .min_by_key(|(_, _, queued_at_ms)| *queued_at_ms)
            .map(|head| head.0);
        oldest.map(|candidate| Selection {
            candidate,
            rule: SelectionRule::Oldest,
        })
    }

    /// Whether any frame is eligible right now, without moving the lane cursor.
    ///
    /// This is the "is there more to send" probe at the end of a flush batch.
    /// Selection advances the round-robin cursor, so the probe saves it and puts
    /// it back: a probe must not change the order the next real turn produces.
    pub fn has_sendable_work(&mut self, now_ms: u64, ack_seq: u64) -> bool {
        let saved = self.lane_cursor;
        let found = self.select_candidate(now_ms, ack_seq).is_some();
        self.lane_cursor = saved;
        found
    }

    /// Remove the frame a selection named, returning its charge to the budget.
    pub(in crate::sync_ws) fn take_selected(&mut self, candidate: Candidate) -> RetainedFrame {
        let domain = &mut self.domains[candidate.slot];
        let mut item = domain.queue.remove(candidate.index);
        if candidate.index < domain.seed_insert_index {
            domain.seed_insert_index -= 1;
        }
        domain.queued_bytes = domain.queued_bytes.saturating_sub(item.estimated_bytes());
        self.release_charge(&mut item);
        item
    }

    /// Empty one domain's queue, releasing every charge it holds.
    pub(in crate::sync_ws) fn clear_domain_queue(&mut self, domain: SyncDomain) {
        let Some(state) = self.domain_mut(domain) else {
            return;
        };
        let queued = std::mem::take(&mut state.queue);
        state.queued_bytes = 0;
        state.seed_insert_index = 0;
        for mut item in queued {
            self.release_charge(&mut item);
        }
    }

    /// Drop the queued cells of sessions that have just been closed.
    pub(in crate::sync_ws) fn remove_queued_cells(&mut self, session_ids: &[String]) {
        if session_ids.is_empty() {
            return;
        }
        self.drain_terminal_queue(|item| {
            item.meta.lane == FeedLane::Cell
                && item
                    .meta
                    .session_id
                    .as_ref()
                    .is_some_and(|session_id| session_ids.iter().any(|closed| closed == session_id))
        });
    }

    /// Drop one session's queued terminal frames.
    ///
    /// `include_semantic` is false while a lane is being reset, because a
    /// view-state is the answer to a command that is still outstanding and
    /// dropping it would leave the client waiting; it is true once the session
    /// itself is gone, where the answer no longer means anything.
    pub(in crate::sync_ws) fn remove_terminal_queued(
        &mut self,
        session_id: &str,
        include_semantic: bool,
    ) {
        self.drain_terminal_queue(|item| {
            item.meta.session_id.as_deref() == Some(session_id)
                && item.meta.lane == FeedLane::Cell
                && (include_semantic || item.frame.is_cell_material())
        });
    }

    /// Empty the terminal queue of everything `should_drop` selects, keeping the
    /// seed cutover consistent with what is left.
    ///
    /// The queue is taken out whole because releasing a charge writes the
    /// socket-wide counters while the frame being released lives in this
    /// domain's queue, and a Rust borrow cannot be two owners at once. One
    /// drain is the honest way to express "these are gone"; three separate
    /// removals pay for the same dance three times.
    fn drain_terminal_queue(&mut self, should_drop: impl Fn(&RetainedFrame) -> bool) {
        let slot = domain_slot(SyncDomain::Terminal);
        let queued = std::mem::take(&mut self.domains[slot].queue);
        let mut kept = Vec::with_capacity(queued.len());
        let mut dropped = 0usize;
        let mut dropped_bytes = 0u64;
        for mut item in queued {
            if should_drop(&item) {
                dropped += 1;
                dropped_bytes += item.estimated_bytes();
                self.release_charge(&mut item);
                continue;
            }
            kept.push(item);
        }
        let domain = &mut self.domains[slot];
        domain.queue = kept;
        domain.queued_bytes = domain.queued_bytes.saturating_sub(dropped_bytes);
        domain.seed_insert_index = domain.seed_insert_index.saturating_sub(dropped);
    }
}

/// Whether a queued frame may go out yet.
///
/// A cell is fenced behind its session's `opened` announcement, and behind that
/// announcement being ACKNOWLEDGED: a client that has not yet processed the
/// `opened` event folds a cell for an unknown session by discarding it, so
/// delivering the cell first loses the frame instead of reordering it. The one
/// exception is a view-state, which answers a command THIS socket sent and so
/// refers to a session the client already asked about.
fn is_eligible(
    item: &RetainedFrame,
    announced: &BTreeSet<String>,
    pending: &BTreeMap<String, u64>,
    ack_seq: u64,
) -> bool {
    if item.meta.lane != FeedLane::Cell {
        return true;
    }
    if item.frame.is_view_state() {
        return true;
    }
    let Some(session_id) = item.meta.session_id.as_ref() else {
        return false;
    };
    if !announced.contains(session_id) {
        return false;
    }
    pending
        .get(session_id)
        .is_none_or(|announced_at| ack_seq >= *announced_at)
}

/// Where a freshly attached baseline or a view-state may be inserted.
///
/// Past this session's own queued frames and past every snapshot, and before
/// everything else: a baseline queued behind another session's cells would
/// paint a viewer onto the wrong screen, and a view-state queued behind an hour
/// of cells would answer a command long after it was asked
/// (`sync-ws-v2-queue.ts:31-49`).
pub(in crate::sync_ws) fn terminal_priority_insert_index(
    queue: &[RetainedFrame],
    session_id: Option<&str>,
) -> usize {
    let mut insert_index = queue.len();
    for index in (0..queue.len()).rev() {
        let item = &queue[index];
        let same_session =
            item.meta.session_id.is_some() && item.meta.session_id.as_deref() == session_id;
        if same_session || item.frame.is_snapshot() {
            return index + 1;
        }
        insert_index = index;
    }
    insert_index
}

/// Whether a retained frame supersedes an already-buffered one.
///
/// Agent status is a current-value projection, so its retained sample IS the
/// cutover for that session: a live status frame already buffered can never be
/// newer than the retained one and is dropped. Every other lane keeps strict
/// FIFO, because there the buffered frame is genuinely earlier
/// (`sync-feed-frames.ts:368-374`).
pub(in crate::sync_ws) fn retained_supersedes_buffered(
    retained: &crate::sync_ws::retained_frame::OwnedFrame,
    buffered: &crate::sync_ws::retained_frame::OwnedFrame,
) -> bool {
    match (
        retained.agent_status_session(),
        buffered.agent_status_session(),
    ) {
        (Some(retained_session), Some(buffered_session)) => retained_session == buffered_session,
        _ => false,
    }
}
