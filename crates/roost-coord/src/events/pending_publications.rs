//! Bounded, same-process recovery for a committed event whose live publication
//! lost a connection-generation race.
//!
//! Owned by the coordinator. One store per process, constructed at boot and
//! injected (`apps/coord/src/main.ts:93`) -- never a lazy static, because the
//! key-revocation path reaches into it from a handler and a test needs to hold a
//! store of its own.
//!
//! **THIS IS NOT THE WORKER'S RESERVATION MODEL.** Worker-side durable capacity
//! (`roost-worker`'s `event_store.rs`) reserves room for an event that has not
//! been written yet. This reserves a *publication* for an event that already
//! committed. Same shape, opposite end of the transaction, and conflating them
//! looks correct until the two lifecycles diverge -- which they do on the first
//! crash. `docs/phase3-coord-contract.md` §3 has the side-by-side table.
//!
//! THE RACE. The durable row commits, then the socket-generation fence is
//! consulted. If the fence says this connection is stale, or the publish throws,
//! the event is durable and nobody was told. Without this store the browser
//! learns of it only on reconnect; with it, the worker's retry -- which is
//! guaranteed, because it never got an ACK -- claims the effect and publishes it
//! now.
//!
//! PROCESS MEMORY ONLY, AND THAT IS CORRECT. A crash discards every retained
//! the entry unacked, replays the same `client_seq`, the INSERT deduplicates, and
//! the worker gets its ACK. The browser recovers the event from the durable
//! `events` table through the Sync feed. The only cross-process state in this
//! whole mechanism is the SQLite unique index.

use std::collections::HashMap;

use roost_protocol::wire::WorkspaceId;

use crate::events::visibility::kind_is_public;

/// Committed publications a process may hold.
pub const MAX_ENTRIES: usize = 256;

/// What a publication slot is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotState {
    /// Claimed before the transaction opened, to serialize concurrent duplicates
    /// of one `client_seq`.
    Reserved,
    /// A committed effect is parked because its publication was lost.
    Retained,
    /// A retry consumed the retained effect.
    Claimed,
}

/// One worker's slot for one `client_seq`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationSlot {
    /// The worker whose event this is.
    pub worker_fp: String,
    /// The worker's monotonic outbox sequence.
    pub client_seq: u64,
    /// Where the slot is in its lifecycle.
    pub state: SlotState,
    /// The committed effect, present only while `Retained`.
    pub effect: Option<RetainedPublication>,
}

/// A committed event awaiting publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetainedPublication {
    /// The event's kind discriminator, kept so a private event is never
    /// published even if a bug routed one here.
    pub event_kind: String,
    /// The durable `events.id`, stamped onto the bus message as the replay
    /// order. It is internal only and never a wire field.
    pub event_id: i64,
    /// The exact JSON the transaction persisted, kept for the byte-for-byte
    /// comparison a dedupe replay performs.
    pub event_json: String,
    /// The dashboard this event was scoped to.
    pub dashboard_id: String,
    /// Workspaces a `closed` orphaned, republished when the claim publishes.
    ///
    /// v2's `CommittedEventPublication` carries these
    /// (`pending-event-publications.ts:24-30`) and the claim path republishes
    /// them. Without them a claim would publish the session event and silently
    /// drop the workspace deletion, and a browser that missed the live delta
    /// would keep a workspace in its sidebar that no longer exists.
    pub cascade_orphan_ids: Vec<WorkspaceId>,
    /// Sessions a snapshot found force-closed, reaped when the claim publishes.
    pub snapshot_reap_ids: Vec<String>,
}

impl RetainedPublication {
    /// Whether this event may reach a browser.
    ///
    /// The same predicate the publisher and the durable readers use
    /// (`docs/phase3-coord-contract.md` §3.7); a private event is durable and
    /// recoverable by its worker and invisible to every dashboard.
    #[must_use]
    pub fn is_publishable(&self) -> bool {
        kind_is_public(&self.event_kind)
    }
}

/// What a claim attempt found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// The retained effect was handed over for publication, and it is handed over
    /// **whole**: the claimer publishes from this value rather than re-deriving
    /// one, so what reaches the bus is the bytes the first delivery committed.
    Claimed(RetainedPublication),
    /// There is nothing to publish: a dedupe with no retained effect, which is
    /// the ordinary path after a crash.
    Nothing,
    /// A retry carried a different payload for the same `client_seq`. A
    /// **protocol violation**: the worker is not replaying what it sent, so the
    /// socket is closed rather than the two payloads being merged.
    PayloadMismatch,
}

/// The store.
#[derive(Debug)]
pub struct PendingPublicationStore {
    by_worker: HashMap<String, HashMap<u64, PublicationSlot>>,
}

impl Default for PendingPublicationStore {
    fn default() -> Self {
        Self::new()
    }
}

impl PendingPublicationStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            by_worker: HashMap::new(),
        }
    }

    /// How many slots are held, across every worker.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_worker
            .values()
            .map(|slots| slots.len())
            .sum::<usize>()
    }

    /// Whether the store holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_worker.is_empty()
    }

    /// Claim a slot for `(worker_fp, client_seq)` before the transaction opens.
    ///
    /// Reserving **before** commit is what makes two concurrent deliveries of the
    /// same sequence serialize instead of double-publishing. Exceeding
    /// [`MAX_ENTRIES`] is refused here, before the transaction, so a full store
    /// costs the caller nothing else.
    pub fn reserve(&mut self, worker_fp: &str, client_seq: u64) -> Result<(), CapacityError> {
        if self.len() >= MAX_ENTRIES && !self.contains(worker_fp, client_seq) {
            return Err(CapacityError);
        }
        let slots = self.by_worker.entry(worker_fp.to_string()).or_default();
        if slots.contains_key(&client_seq) {
            // A concurrent duplicate of one sequence. It does not get its own
            // slot; it waits for the first to settle, and the caller re-reserves.
            return Ok(());
        }
        slots.insert(
            client_seq,
            PublicationSlot {
                worker_fp: worker_fp.to_string(),
                client_seq,
                state: SlotState::Reserved,
                effect: None,
            },
        );
        Ok(())
    }

    /// Park a committed effect whose publication was lost.
    pub fn retain(&mut self, worker_fp: &str, client_seq: u64, effect: RetainedPublication) {
        if let Some(slot) = self
            .by_worker
            .get_mut(worker_fp)
            .and_then(|slots| slots.get_mut(&client_seq))
        {
            slot.state = SlotState::Retained;
            slot.effect = Some(effect);
        }
    }

    /// Hand a retained effect to a retry, comparing the payload byte for byte.
    ///
    /// The comparison is the whole reason this store holds the JSON rather than
    /// just the event: two payloads for one `client_seq` means the worker is not
    /// replaying what it sent, and merging them would produce an event that
    /// neither delivery described.
    pub fn claim(
        &mut self,
        worker_fp: &str,
        client_seq: u64,
        replay_event_json: &str,
    ) -> ClaimOutcome {
        let Some(slot) = self
            .by_worker
            .get_mut(worker_fp)
            .and_then(|slots| slots.get_mut(&client_seq))
        else {
            return ClaimOutcome::Nothing;
        };
        match slot.state {
            SlotState::Retained => {}
            _ => return ClaimOutcome::Nothing,
        }
        let Some(effect) = slot.effect.as_ref() else {
            return ClaimOutcome::Nothing;
        };
        if effect.event_json != replay_event_json {
            return ClaimOutcome::PayloadMismatch;
        }
        let claimed = effect.clone();
        slot.state = SlotState::Claimed;
        ClaimOutcome::Claimed(claimed)
    }

    /// Release a slot after a successful publish, or when there is nothing to
    /// publish.
    pub fn release(&mut self, worker_fp: &str, client_seq: u64) {
        if let Some(slots) = self.by_worker.get_mut(worker_fp) {
            slots.remove(&client_seq);
            if slots.is_empty() {
                self.by_worker.remove(worker_fp);
            }
        }
    }

    /// Drop every slot for a fingerprint.
    ///
    /// Called on key revocation (`apps/coord/src/main.ts:103-106`) and on worker
    /// delete (`apps/coord/src/workers/handlers-workers.ts:242-243`): a revoked
    /// worker's retained effect must not be publishable by anything, including a
    /// retry that was already in flight.
    pub fn clear_worker(&mut self, worker_fp: &str) {
        self.by_worker.remove(worker_fp);
    }

    /// Whether a slot exists for a key, without revealing the effect.
    #[must_use]
    pub fn contains(&self, worker_fp: &str, client_seq: u64) -> bool {
        self.by_worker
            .get(worker_fp)
            .is_some_and(|slots| slots.contains_key(&client_seq))
    }
}

/// The store is full. Refused before the transaction opens, so a full store costs
/// the caller nothing but the retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("pending event publication capacity exceeded")]
pub struct CapacityError;
