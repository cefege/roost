//! One attempt's carrier: the three lanes, the identity the worker's `Ready`
//! earned, and the document's own connections.
//!
//! Owned by `platform::carriers`, held by `pump` and reached by
//! `pump::peer_lane`. Target-independent on purpose: every fence below — the
//! tuple check, the lane bytes, the generation a frame is stamped on, the
//! deadlines — is a value a native test can decide, and only the browser's writes
//! and reads are left to the host.
//!
//! OWNED PER ATTEMPT, and that is the whole point. One document holds several
//! attempts at once — a second worker, a replacement epoch — and a peer table
//! keyed by anything but the attempt id would let one worker's fault retire
//! another's carrier. Every map here is keyed by `attempt_id`, and the only
//! other key is the generation a token presents, which is what makes a frame
//! from a retired attempt un-routable rather than merely late.
//!
//! The lanes are opened WITH the attempt rather than at the `Ready`, because the
//! control lane carries the `Hello` and a carrier that had no lanes until it had
//! already authenticated could not say anything at all.

use std::collections::BTreeMap;

use roost_client_core::TerminalToken;
use roost_client_core::client::carriers::{PeerAttempt, PeerLane, ReadyTuple};
use roost_client_core::terminal::routes::DirectCarrier;

use super::life::PeerLife;
use super::{LaneFault, PeerLanes, drain_order};
use crate::platform::carriers::route::ConnectionKey;

/// One attempt's whole host-side life, from opening its lanes to retiring them.
#[derive(Debug)]
pub struct PeerCarrier {
    attempt: PeerAttempt,
    lanes: PeerLanes,
    life: PeerLife,
    /// `None` until the worker proves its tuple: a carrier that has not
    /// authenticated presents nothing, so there is no generation to register and
    /// no route that could name it.
    carrier: Option<DirectCarrier>,
}

impl PeerCarrier {
    /// The record for an attempt whose transport has just opened.
    pub fn opened(attempt: PeerAttempt, now_ms: u64) -> Self {
        let lanes = PeerLanes::new(attempt.attempt_id);
        let life = PeerLife::opened(attempt.worker_fp.clone(), now_ms);
        Self {
            attempt,
            lanes,
            life,
            carrier: None,
        }
    }

    /// Which attempt this carrier is.
    pub fn attempt_id(&self) -> u64 {
        self.attempt.attempt_id
    }

    /// The attempt as the negotiation opened it.
    pub fn attempt(&self) -> &PeerAttempt {
        &self.attempt
    }

    /// This attempt's clock, for the tick.
    pub fn life(&self) -> &PeerLife {
        &self.life
    }

    /// This attempt's clock, for the tick to advance.
    pub fn life_mut(&mut self) -> &mut PeerLife {
        &mut self.life
    }

    /// The worker this carrier reaches.
    pub fn worker_fp(&self) -> &str {
        &self.attempt.worker_fp
    }

    /// Whether the worker has proved its tuple, which is what makes spending the
    /// credential on the control lane legitimate and what makes a cell frame a
    /// frame rather than a claim about a tag.
    pub fn is_authenticated(&self) -> bool {
        self.carrier.is_some()
    }

    /// Admit a proved tuple, or refuse it.
    ///
    /// `None` is the refusal and it is the CORE's rule (`ReadyTuple::admits`):
    /// the worker, its process epoch, this negotiation's peer id, a non-zero
    /// socket generation, a socket id to fence against, and a session scope
    /// inside the attempt's own. A host that assembled the carrier anyway would
    /// hand the registry a generation minted from a message that was never this
    /// attempt's.
    pub fn authenticate(
        &mut self,
        ready: &ReadyTuple,
        connection_id: String,
    ) -> Option<DirectCarrier> {
        if self.carrier.is_some() || !ready.admits(&self.attempt) {
            return None;
        }
        let carrier = ready.carrier(&self.attempt, connection_id);
        self.life.authenticated();
        self.carrier = Some(carrier.clone());
        Some(carrier)
    }

    /// The exact generation this carrier presents, once it has one.
    pub fn token(&self) -> Option<&TerminalToken> {
        self.carrier.as_ref().map(|carrier| &carrier.token)
    }

    /// The connection this carrier announces to the route registry.
    pub fn connection_id(&self) -> Option<&str> {
        self.carrier
            .as_ref()
            .map(|carrier| carrier.connection_id.as_str())
    }

    /// Queue one logical message on a lane. `Ok(false)` is backpressure, and the
    /// message was NOT retained.
    pub fn enqueue(&mut self, lane: PeerLane, message: Vec<u8>) -> Result<bool, LaneFault> {
        self.lanes.enqueue(lane, message)
    }

    /// Feed one arrived packet to its lane's assembler. `Ok(None)` means the
    /// fragment was retained and its message is still incomplete.
    pub fn push(
        &mut self,
        lane: PeerLane,
        now_ms: u64,
        packet: &[u8],
    ) -> Result<Option<Vec<u8>>, LaneFault> {
        self.lanes.push(lane, now_ms, packet)
    }

    /// The complete logical bytes one lane still holds, for the diagnostic that
    /// reports what a carrier is holding and cannot write yet.
    pub fn queued_bytes(&self, lane: PeerLane) -> usize {
        self.lanes.queued_bytes(lane)
    }

    /// Lanes this carrier may write next, in the order the protocol fixes.
    pub fn write_order() -> [PeerLane; 3] {
        drain_order()
    }

    /// The next packet this lane may write, committed here.
    ///
    /// The bytes are copied out of the fragment before it is committed, because
    /// the fragment borrows the lane's queue and the host writes them to a
    /// browser object the pump does not own. Committing is what makes the write
    /// happen once: a queue that produced the same bytes twice would duplicate a
    /// frame on an ORDERED lane.
    pub fn next_packet(&mut self, lane: PeerLane) -> Result<Option<Vec<u8>>, LaneFault> {
        let Some(fragment) = self.lanes.next_fragment(lane)? else {
            return Ok(None);
        };
        let bytes = fragment.bytes().to_vec();
        fragment.commit();
        Ok(Some(bytes))
    }

    /// The lanes whose fragments have not completed a message for longer than
    /// the protocol's packet stall allows.
    pub fn stalled_lanes(&mut self, now_ms: u64) -> Vec<PeerLane> {
        self.lanes.expired_lanes(now_ms)
    }
}

/// Every peer carrier this document holds, by attempt id and by the generation
/// each presents.
///
/// The two indexes answer the two questions the host actually asks, and neither
/// is derivable from the other cheaply: the drain is handed an attempt id by a
/// browser callback that knows nothing about a generation, and the send path is
/// handed a token that names no attempt.
#[derive(Debug, Default)]
pub struct PeerCarriers {
    by_attempt: BTreeMap<u64, PeerCarrier>,
    by_key: BTreeMap<ConnectionKey, u64>,
}

impl PeerCarriers {
    /// A document holding no peer carrier.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a newly opened attempt, and name the attempt it displaced.
    ///
    /// The attempt id is never reused by the core, so a displacement here can
    /// only be the same worker re-opening; the displaced record is handed back so
    /// the caller closes it rather than leaving its lanes queued behind a peer
    /// nobody holds.
    pub fn open(&mut self, carrier: PeerCarrier) -> Option<PeerCarrier> {
        self.by_attempt.insert(carrier.attempt_id(), carrier)
    }

    /// Admit a proved tuple under this document's own connection id, and name the
    /// attempt whose generation it displaced.
    pub fn authenticate(
        &mut self,
        attempt_id: u64,
        ready: &ReadyTuple,
        connection_id: String,
    ) -> Option<(DirectCarrier, Option<PeerCarrier>)> {
        let admitted = self
            .by_attempt
            .get_mut(&attempt_id)?
            .authenticate(ready, connection_id)?;
        let key = ConnectionKey::of(&admitted.token)?;
        let displaced = self
            .by_key
            .insert(key, attempt_id)
            .filter(|previous| *previous != attempt_id)
            .and_then(|previous| self.retire_attempt(previous));
        Some((admitted, displaced))
    }

    /// Open an attempt and admit the tuple it proved, in one step.
    ///
    /// The two halves the negotiation does apart — `open` when the transport
    /// comes up, `authenticate` when the worker's `Ready` comes back — are one
    /// call here because a caller that opened one attempt and then authenticated
    /// a different one would register a generation under a lane set belonging to
    /// another negotiation.
    pub fn authenticate_after_open(
        &mut self,
        attempt: &PeerAttempt,
        ready: &ReadyTuple,
        connection_id: String,
    ) -> Option<(DirectCarrier, Option<PeerCarrier>)> {
        self.open(PeerCarrier::opened(attempt.clone(), 0));
        self.authenticate(attempt.attempt_id, ready, connection_id)
    }

    /// The carrier one attempt holds, which is what a browser callback names.
    pub fn attempt(&self, attempt_id: u64) -> Option<&PeerCarrier> {
        self.by_attempt.get(&attempt_id)
    }

    /// The carrier one attempt holds, for the drain.
    pub fn attempt_mut(&mut self, attempt_id: u64) -> Option<&mut PeerCarrier> {
        self.by_attempt.get_mut(&attempt_id)
    }

    /// The carrier presenting exactly this generation, which is what a
    /// `SendDirect` names.
    pub fn for_token(&self, token: &TerminalToken) -> Option<&PeerCarrier> {
        let attempt_id = *self.by_key.get(&ConnectionKey::of(token)?)?;
        let carrier = self.by_attempt.get(&attempt_id)?;
        (carrier.token() == Some(token)).then_some(carrier)
    }

    /// Drop one attempt, and hand its carrier back so the host can close it and
    /// retire the exact routes it was serving.
    pub fn retire_attempt(&mut self, attempt_id: u64) -> Option<PeerCarrier> {
        let carrier = self.by_attempt.remove(&attempt_id)?;
        self.by_key.retain(|_, held| *held != attempt_id);
        Some(carrier)
    }

    /// Drop every attempt a worker held, for a retirement or a restart.
    pub fn retire_worker(&mut self, worker_fp: &str) -> Vec<PeerCarrier> {
        let doomed: Vec<u64> = self
            .by_attempt
            .values()
            .filter(|carrier| carrier.worker_fp() == worker_fp)
            .map(PeerCarrier::attempt_id)
            .collect();
        doomed
            .into_iter()
            .filter_map(|attempt_id| self.retire_attempt(attempt_id))
            .collect()
    }

    /// The attempts this document holds, in id order.
    pub fn attempt_ids(&self) -> Vec<u64> {
        self.by_attempt.keys().copied().collect()
    }

    /// How many peer carriers this document holds.
    pub fn len(&self) -> usize {
        self.by_attempt.len()
    }

    /// Whether this document holds no peer carrier.
    pub fn is_empty(&self) -> bool {
        self.by_attempt.is_empty()
    }
}
