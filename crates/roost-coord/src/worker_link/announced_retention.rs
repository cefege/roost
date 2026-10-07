//! One compact terminal-metadata record per worker channel, held outside the
//! announced-cell buffer: an early fact waiting up to 3 s for its channel's
//! announcement, or the latest fact parked after a cell-loss drop, which this
//! owner keeps up to 30 s until the exact durable route commits.
//!
//! Owned by `worker_link::announced_barrier`. Ports
//! `apps/coord/src/events/announced-channel-semantic-retention.ts`. It accepts
//! no PTY bytes and charges the socket's `RetainedWorkBudget`.
//!
//! NO DRAIN INTERLEAVING. v2 awaits each recovery delivery, so later facts can
//! arrive mid-drain; its `drainingRecoveryChannels` set and the session a
//! pre-announced record carries exist only for that. Here delivery is a
//! synchronous callback on the socket's one task, so nothing reaches this owner
//! during a delivery and those two states cannot occur.

use std::collections::HashMap;

use roost_protocol::proto_adapters::coord_worker_proto::encode_upstream;
use roost_protocol::wire::coord_worker::{CoordWorkerUpstream, TerminalMetadata};
use tokio::time::Instant;

use crate::worker_link::announced_types::{
    SEMANTIC_METADATA_MAX_BYTES, SEMANTIC_METADATA_MAX_CHANNELS, SEMANTIC_METADATA_PREANNOUNCE_MAX,
    SEMANTIC_METADATA_RECOVERY_MAX,
};
use crate::worker_link::retained_budget::{RetainOutcome, RetainedWorkBudget};

/// One compact metadata fact and the budget charge it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetainedMetadata {
    /// The fact itself.
    pub metadata: TerminalMetadata,
    /// Its encoded size, which is what the budget was charged.
    pub encoded_bytes: u64,
}

/// Whether an encoded metadata frame is small enough to retain.
#[must_use]
pub fn is_compact_terminal_metadata(encoded_bytes: u64) -> bool {
    encoded_bytes > 0 && encoded_bytes <= SEMANTIC_METADATA_MAX_BYTES
}

/// Coalesce two facts for one channel: each changed field keeps its newest
/// value, and a field either fact changed stays changed. A pending clipboard
/// write or command completion survives later title/activity-only
/// observations. `None` only when the merged frame does not encode, which the
/// caller treats as not compact.
#[must_use]
pub fn merge_terminal_metadata(
    previous: &TerminalMetadata,
    incoming: &TerminalMetadata,
) -> Option<RetainedMetadata> {
    let newest_title = if incoming.title_changed {
        incoming
    } else {
        previous
    };
    let newest_activity = if incoming.activity_changed {
        incoming
    } else {
        previous
    };
    let newest_clipboard = if incoming.clipboard_changed {
        incoming
    } else {
        previous
    };
    let newest_command = if incoming.command_finished {
        incoming
    } else {
        previous
    };
    let metadata = TerminalMetadata {
        channel_id: incoming.channel_id,
        title_changed: previous.title_changed || incoming.title_changed,
        title: newest_title.title.clone(),
        activity_changed: previous.activity_changed || incoming.activity_changed,
        activity_ts_ms: newest_activity.activity_ts_ms,
        clipboard_changed: previous.clipboard_changed || incoming.clipboard_changed,
        clipboard: newest_clipboard.clipboard.clone(),
        command_finished: previous.command_finished || incoming.command_finished,
        command_exit_code: newest_command.command_exit_code,
        command_duration_ms: newest_command.command_duration_ms,
        bell: previous.bell || incoming.bell,
    };
    let frame = CoordWorkerUpstream::TerminalMetadata(metadata);
    let encoded_bytes = u64::try_from(encode_upstream(&frame).ok()?.len()).ok()?;
    let CoordWorkerUpstream::TerminalMetadata(metadata) = frame else {
        return None;
    };
    Some(RetainedMetadata {
        metadata,
        encoded_bytes,
    })
}

/// A retained fact with its expiry, and the session a recovery belongs to.
#[derive(Debug)]
struct Parked {
    fact: RetainedMetadata,
    session_id: Option<String>,
    deadline: Instant,
}

/// Which of the two record sets an operation addresses.
#[derive(Debug, Clone, Copy)]
enum Shelf {
    PreAnnounced,
    Recovery,
}

/// The per-socket semantic metadata owner.
#[derive(Debug, Default)]
pub struct SemanticRetention {
    pre_announced: HashMap<u32, Parked>,
    recovery: HashMap<u32, Parked>,
}

/// Retention's share of the barrier counters: frames, bytes, early, recovery.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticStats {
    pub frames: usize,
    pub bytes: u64,
    pub pre_announced: usize,
    pub recovery: usize,
}

impl SemanticRetention {
    /// Retain a fact for a channel nothing announced: into the channel's
    /// recovery when one is parked, else as an early fact.
    pub fn retain_unannounced(
        &mut self,
        channel_id: u32,
        metadata: &TerminalMetadata,
        encoded_bytes: u64,
        now: Instant,
        budget: &mut RetainedWorkBudget,
    ) -> bool {
        let recovery_session = self
            .recovery
            .get(&channel_id)
            .map(|parked| parked.session_id.clone());
        let (shelf, session_id, max_age) = match recovery_session {
            Some(session_id) => (Shelf::Recovery, session_id, SEMANTIC_METADATA_RECOVERY_MAX),
            None => (Shelf::PreAnnounced, None, SEMANTIC_METADATA_PREANNOUNCE_MAX),
        };
        if !is_compact_terminal_metadata(encoded_bytes) {
            return false;
        }
        let compact = match self.shelf(shelf).get(&channel_id) {
            Some(previous) => match merge_terminal_metadata(&previous.fact.metadata, metadata) {
                Some(merged) => merged,
                None => return false,
            },
            None => RetainedMetadata {
                metadata: metadata.clone(),
                encoded_bytes,
            },
        };
        if !is_compact_terminal_metadata(compact.encoded_bytes) {
            return false;
        }
        let had_previous = self.shelf(shelf).contains_key(&channel_id);
        if !had_previous && self.record_count() >= SEMANTIC_METADATA_MAX_CHANNELS {
            return false;
        }
        if had_previous {
            self.discard(shelf, channel_id, budget);
        }
        if budget.retain(compact.encoded_bytes) != RetainOutcome::Retained {
            return false;
        }
        self.install(shelf, channel_id, session_id, compact, now + max_age);
        true
    }

    /// Hand an early fact to a new announcement; its charge moves with it.
    pub fn take_pre_announced(&mut self, channel_id: u32) -> Option<RetainedMetadata> {
        self.pre_announced
            .remove(&channel_id)
            .map(|parked| parked.fact)
    }

    /// Park a dropped channel's latest fact until its exact route commits.
    /// The fact's charge moves in; a refusal releases it.
    pub fn park_recovery(
        &mut self,
        channel_id: u32,
        session_id: &str,
        fact: RetainedMetadata,
        now: Instant,
        budget: &mut RetainedWorkBudget,
    ) -> bool {
        self.discard(Shelf::PreAnnounced, channel_id, budget);
        let same_session = self
            .recovery
            .get(&channel_id)
            .map(|parked| parked.session_id.as_deref() == Some(session_id));
        if same_session.is_none() && self.record_count() >= SEMANTIC_METADATA_MAX_CHANNELS {
            budget.release(fact.encoded_bytes);
            return false;
        }
        let mut recovery_fact = fact;
        match same_session {
            Some(true) => {
                let Some(previous) = self.recovery.remove(&channel_id) else {
                    return false;
                };
                let merged =
                    merge_terminal_metadata(&previous.fact.metadata, &recovery_fact.metadata);
                budget.release(previous.fact.encoded_bytes);
                budget.release(recovery_fact.encoded_bytes);
                let Some(merged) = merged else {
                    return false;
                };
                if !is_compact_terminal_metadata(merged.encoded_bytes)
                    || budget.retain(merged.encoded_bytes) != RetainOutcome::Retained
                {
                    return false;
                }
                recovery_fact = merged;
            }
            Some(false) => self.discard(Shelf::Recovery, channel_id, budget),
            None => {}
        }
        let deadline = now + SEMANTIC_METADATA_RECOVERY_MAX;
        let session_id = Some(session_id.to_owned());
        self.install(
            Shelf::Recovery,
            channel_id,
            session_id,
            recovery_fact,
            deadline,
        );
        true
    }

    /// A re-announcement for the same session carries its recovery forward; a
    /// recovery for another session is discarded.
    pub fn take_recovery_for_session(
        &mut self,
        channel_id: u32,
        session_id: &str,
        budget: &mut RetainedWorkBudget,
    ) -> Option<RetainedMetadata> {
        let matches = self.recovery.get(&channel_id)?.session_id.as_deref() == Some(session_id);
        if !matches {
            self.discard(Shelf::Recovery, channel_id, budget);
            return None;
        }
        self.recovery.remove(&channel_id).map(|parked| parked.fact)
    }

    /// A channel the durable index already maps: whether its fact still
    /// belongs behind a same-session recovery. A replaced route must not let
    /// its predecessor's recovery or early fact absorb a new fact.
    pub fn reconcile_mapped_route(
        &mut self,
        channel_id: u32,
        session_id: &str,
        budget: &mut RetainedWorkBudget,
    ) -> bool {
        if let Some(parked) = self.recovery.get(&channel_id) {
            if parked.session_id.as_deref() == Some(session_id) {
                return true;
            }
            self.discard(Shelf::Recovery, channel_id, budget);
        }
        self.discard(Shelf::PreAnnounced, channel_id, budget);
        false
    }

    /// Forget a channel's recovery: its durable append failed.
    pub fn discard_recovery(&mut self, channel_id: u32, budget: &mut RetainedWorkBudget) {
        self.discard(Shelf::Recovery, channel_id, budget);
    }

    /// The route committed for a channel with no live announcement: deliver
    /// its parked fact when it belongs to this session and the durable index
    /// bound it. `Ok(true)` only when the fact was delivered.
    pub fn commit_recovery<E>(
        &mut self,
        channel_id: u32,
        session_id: &str,
        mapping_matches: bool,
        budget: &mut RetainedWorkBudget,
        deliver: &mut dyn FnMut(CoordWorkerUpstream) -> Result<(), E>,
    ) -> Result<bool, E> {
        let belongs = self
            .recovery
            .get(&channel_id)
            .is_some_and(|parked| parked.session_id.as_deref() == Some(session_id));
        if !belongs {
            return Ok(false);
        }
        if !mapping_matches {
            self.discard(Shelf::Recovery, channel_id, budget);
            return Ok(false);
        }
        let Some(parked) = self.recovery.remove(&channel_id) else {
            return Ok(false);
        };
        let delivered = deliver(CoordWorkerUpstream::TerminalMetadata(parked.fact.metadata));
        budget.release(parked.fact.encoded_bytes);
        delivered.map(|()| true)
    }

    /// Discard every record whose wait has run out.
    pub fn expire(&mut self, now: Instant, budget: &mut RetainedWorkBudget) {
        for shelf in [Shelf::PreAnnounced, Shelf::Recovery] {
            let expired: Vec<u32> = self
                .shelf(shelf)
                .iter()
                .filter(|(_, parked)| parked.deadline <= now)
                .map(|(channel_id, _)| *channel_id)
                .collect();
            for channel_id in expired {
                tracing::debug!(channel_id, ?shelf, "worker link: retained metadata expired");
                self.discard(shelf, channel_id, budget);
            }
        }
    }

    /// The earliest record expiry, for the read loop's timer.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        self.pre_announced
            .values()
            .chain(self.recovery.values())
            .map(|parked| parked.deadline)
            .min()
    }

    /// Records and bytes held.
    #[must_use]
    pub fn stats(&self) -> SemanticStats {
        let bytes = self
            .pre_announced
            .values()
            .chain(self.recovery.values())
            .map(|parked| parked.fact.encoded_bytes)
            .sum();
        SemanticStats {
            frames: self.record_count(),
            bytes,
            pre_announced: self.pre_announced.len(),
            recovery: self.recovery.len(),
        }
    }

    fn shelf(&mut self, shelf: Shelf) -> &mut HashMap<u32, Parked> {
        match shelf {
            Shelf::PreAnnounced => &mut self.pre_announced,
            Shelf::Recovery => &mut self.recovery,
        }
    }

    fn install(
        &mut self,
        shelf: Shelf,
        channel_id: u32,
        session_id: Option<String>,
        fact: RetainedMetadata,
        deadline: Instant,
    ) {
        let parked = Parked {
            fact,
            session_id,
            deadline,
        };
        self.shelf(shelf).insert(channel_id, parked);
    }

    fn discard(&mut self, shelf: Shelf, channel_id: u32, budget: &mut RetainedWorkBudget) {
        if let Some(parked) = self.shelf(shelf).remove(&channel_id) {
            budget.release(parked.fact.encoded_bytes);
        }
    }

    fn record_count(&self) -> usize {
        self.pre_announced.len() + self.recovery.len()
    }
}
