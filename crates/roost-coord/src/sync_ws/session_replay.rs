//! Which durable session events one Sync socket still owes its client: the
//! replay cutoff, the live boundary a v1 backfill must not repeat, and the live
//! events a v2 recovery holds until the durable interval below them is out.
//!
//! Owned by `sync_ws::driver::LinkState` (`replay`); `sync_ws::live_feed` asks it
//! about every live session event and `sync_ws::backfill` about every durable
//! row. Ports the recovery half of `startSyncFeed` in
//! `apps/coord/src/sync/sync-feed.ts` (`emitLiveSessionNow`,
//! `emitRecoveredSession`, `emitSession` and the tail flush of `backfill`).
//!
//! ONE SCALAR CUTOFF, AND A SET THAT NEVER GROWS AFTER RECOVERY. Every event at
//! or below the cutoff was already delivered, so it is dropped wherever it
//! arrives from. The boundary set holds only the live ids a recovery overlapped;
//! once the recovery is over, later live ids go out without being remembered, so
//! a long-lived socket's memory is not a function of how many events it saw.

use std::collections::{BTreeMap, BTreeSet};

use crate::events::bus_messages::SessionBusMessage;

/// Live session events a v2 recovery may hold before it gives up
/// (`sync-feed.ts:152`).
pub const RECOVERY_HOLD_MAX_EVENTS: usize = 512;

/// Estimated bytes of live session events a v2 recovery may hold
/// (`sync-feed.ts:153`).
pub const RECOVERY_HOLD_MAX_BYTES: usize = 4 * 1024 * 1024;

/// What to do with one live session event.
#[derive(Debug, Clone, PartialEq)]
pub enum LiveVerdict {
    /// Deliver it now.
    Emit,
    /// The client already has it: at or below the cutoff, or a boundary event
    /// the recovery already delivered.
    Duplicate,
    /// A v2 recovery is running; the event waits for the durable interval.
    Held,
    /// A v2 recovery gave up. The terminal domain is reset with `reason`, `emit`
    /// says whether the event that caused the abort still goes out live, and
    /// `held` are the live events the recovery was holding when it gave up.
    ///
    /// `held` exists because those events were LIVE: the client has not seen
    /// them, and a durable log the recovery never finished reading is not
    /// evidence that it has. Discarding them leaves a browser whose "opened" row
    /// never arrives while its terminal replica — admitted off the same feed,
    /// independently — keeps painting: a client showing an empty session list
    /// beside a live terminal, with no way for the reader to tell them apart.
    Abort {
        /// The `domain_reset` reason v2 sends.
        reason: &'static str,
        /// Whether the event that caused the abort is delivered anyway.
        emit: bool,
        /// The live events the abandoned recovery was holding, in id order.
        held: Vec<SessionBusMessage>,
    },
}

/// One socket's durable-replay bookkeeping.
#[derive(Debug)]
pub struct SessionReplay {
    cutoff: u64,
    collecting_boundary: bool,
    boundary: BTreeSet<u64>,
    held: BTreeMap<u64, (SessionBusMessage, usize)>,
    held_bytes: usize,
    recovering: bool,
    aborted: bool,
}

impl SessionReplay {
    /// A socket resuming from `since` (zero for a fresh one). A v1 socket with
    /// a cursor delivers live events at once and remembers them as the
    /// boundary; a v2 one holds them until its recovery finishes
    /// (`sync-feed.ts:92-98`).
    #[must_use]
    pub fn new(since: u64, v2: bool) -> Self {
        Self {
            cutoff: since,
            collecting_boundary: !v2 && since > 0,
            boundary: BTreeSet::new(),
            held: BTreeMap::new(),
            held_bytes: 0,
            recovering: v2 && since > 0,
            aborted: false,
        }
    }

    /// Decide one live session event (`sync-feed.ts:123-165`).
    pub fn admit_live(&mut self, message: &SessionBusMessage) -> LiveVerdict {
        if !self.recovering {
            return self.live_now(message.event_id.unwrap_or(0));
        }
        let Some(event_id) = message.event_id.filter(|id| *id > 0) else {
            let held = self.abort();
            return LiveVerdict::Abort {
                reason: "unstamped_session_event",
                emit: false,
                held,
            };
        };
        let estimated = estimated_bytes(message);
        let previous = self.held.get(&event_id).map(|(_, bytes)| *bytes);
        let next_bytes = self.held_bytes - previous.unwrap_or(0) + estimated;
        let full = previous.is_none() && self.held.len() >= RECOVERY_HOLD_MAX_EVENTS;
        if full || next_bytes > RECOVERY_HOLD_MAX_BYTES {
            let held = self.abort();
            let emit = self.live_now(event_id) == LiveVerdict::Emit;
            return LiveVerdict::Abort {
                reason: "recovery_live_overflow",
                emit,
                held,
            };
        }
        self.held.insert(event_id, (message.clone(), estimated));
        self.held_bytes = next_bytes;
        LiveVerdict::Held
    }

    /// Whether a durable row above the cursor is delivered: `false` when it is
    /// at or below the cutoff or a live boundary event already carried it
    /// (`sync-feed.ts:130-135`). Either way the cutoff advances to it.
    pub fn admit_recovered(&mut self, event_id: u64) -> bool {
        if event_id <= self.cutoff {
            return false;
        }
        let already_live = self.boundary.remove(&event_id);
        self.cutoff = event_id;
        !already_live
    }

    /// Whether a v2 recovery gave up; its backfill stops at once.
    #[must_use]
    pub fn is_aborted(&self) -> bool {
        self.aborted
    }

    /// Stop recovering and HAND BACK what was held, for a recovery that failed
    /// or a live event that overflowed it.
    ///
    /// The events are live traffic this client has not seen, so the caller
    /// emits them. Returning rather than clearing is the whole fix: a client
    /// that connects mid-recovery and then loses it would otherwise never learn
    /// that its session opened, while its terminal — admitted off the same feed —
    /// keeps painting. What the durable log already covers stays dropped, because
    /// `finish_recovery` and `live_now` are what decide that, and neither runs
    /// here.
    pub fn abort(&mut self) -> Vec<SessionBusMessage> {
        let held = std::mem::take(&mut self.held);
        tracing::warn!(
            event = "sync-ws",
            action = "probe_held_released",
            held = held.len(),
            "an abandoned recovery released the live events it was holding"
        );
        self.aborted = true;
        self.recovering = false;
        self.held_bytes = 0;
        held.into_values().map(|(message, _)| message).collect()
    }

    /// The v2 recovery reached `cutoff`: hand back the held events above it,
    /// in id order, remembering each as the boundary so a repeat of it is
    /// dropped (`sync-feed.ts:334-345`).
    pub fn finish_recovery(&mut self, cutoff: u64) -> Vec<SessionBusMessage> {
        self.cutoff = cutoff;
        let held = std::mem::take(&mut self.held);
        self.held_bytes = 0;
        self.recovering = false;
        held.into_iter()
            .filter(|(event_id, _)| *event_id > cutoff)
            .map(|(event_id, (message, _))| {
                self.boundary.insert(event_id);
                message
            })
            .collect()
    }

    /// The v1 backfill is over: later live events are no longer remembered
    /// (`sync-feed.ts:372`).
    pub fn finish_backfill(&mut self) {
        self.collecting_boundary = false;
    }

    fn live_now(&mut self, event_id: u64) -> LiveVerdict {
        if event_id > 0 {
            if event_id <= self.cutoff || self.boundary.contains(&event_id) {
                tracing::warn!(
                    event = "sync-ws",
                    action = "probe_live_duplicate",
                    event_id,
                    cutoff = self.cutoff,
                    boundary = self.boundary.contains(&event_id),
                    "live event dropped as duplicate: the client already has it"
                );
                return LiveVerdict::Duplicate;
            }
            if self.collecting_boundary {
                self.boundary.insert(event_id);
            }
        }
        LiveVerdict::Emit
    }
}

/// v2's `JSON.stringify(event).length`: the event's JSON text, which is what
/// the hold is bounded by.
fn estimated_bytes(message: &SessionBusMessage) -> usize {
    serde_json::to_string(&message.event).map_or(0, |text| text.len())
}
