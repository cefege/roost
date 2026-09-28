//! The bounded bookkeeping of terminal-peer signaling: admissions claimed
//! before authorization, reserved offers awaiting a typed worker answer, and
//! the per-device and per-worker counts both are charged against.
//! Owned by `peer_negotiations` behind its lock. Ports the admission and
//! pending-map half of `apps/coord/src/terminal/direct/terminal-peer-negotiations.ts`
//! (`claimAdmission`, `releaseAdmission`, `reserve`, `removePending`).

use std::collections::HashMap;
use std::sync::Arc;

use connectrpc::ConnectError;
use roost_proto::SessionsNegotiateLocalTerminalPeerResponse;
use roost_protocol::terminal_peer::peer::{
    TERMINAL_PEER_MAX_NEGOTIATIONS_PER_WORKER, TERMINAL_PEER_MAX_PENDING_NEGOTIATIONS,
    TERMINAL_PEER_MAX_PENDING_NEGOTIATIONS_PER_DEVICE,
};
use tokio::sync::oneshot;

use crate::coord_core::ids::{draw, render_v4};
use crate::coord_core::worker_handle::WorkerHandle;
use crate::terminal_direct::peer_state::{
    PeerKey, terminal_peer_already_exists, terminal_peer_exhausted, terminal_peer_invalid,
};

/// How a reserved offer settles for the negotiation waiting on it.
pub(crate) type PeerOutcome = Result<SessionsNegotiateLocalTerminalPeerResponse, ConnectError>;

/// One negotiation admitted but still authorizing; it holds its capacity.
#[derive(Debug, Clone)]
pub(crate) struct TerminalPeerAdmission {
    pub(crate) peer_id: String,
    pub(crate) offer_digest: String,
    pub(crate) device_fingerprint: String,
    pub(crate) worker_fp: String,
}

/// One offer sent (or about to be) to an exact worker generation. Retains the
/// offer's digest, never the SDP itself.
#[derive(Debug)]
pub(crate) struct PendingTerminalPeerNegotiation {
    pub(crate) request_id: String,
    pub(crate) owner_key: String,
    pub(crate) device_fingerprint: String,
    pub(crate) tab_id: String,
    pub(crate) worker_fp: String,
    pub(crate) grant_id: String,
    pub(crate) peer_id: String,
    pub(crate) worker: Arc<WorkerHandle>,
    pub(crate) connection_generation: String,
    pub(crate) worker_epoch: String,
    pub(crate) offer_digest: String,
    pub(crate) settle: oneshot::Sender<PeerOutcome>,
}

/// Every admission and pending offer, and the counts they are charged to.
#[derive(Debug, Default)]
pub(crate) struct NegotiationTable {
    pending: HashMap<String, PendingTerminalPeerNegotiation>,
    pending_by_key: HashMap<PeerKey, String>,
    admitting: HashMap<PeerKey, TerminalPeerAdmission>,
    by_device: HashMap<String, usize>,
    by_worker: HashMap<String, usize>,
    pub(crate) disposed: bool,
}

impl NegotiationTable {
    /// Charge one admission, refusing a duplicate document/worker tuple, a
    /// conflicting offer under the same peer id, or any exhausted bound.
    pub(crate) fn claim_admission(
        &mut self,
        key: PeerKey,
        admission: TerminalPeerAdmission,
    ) -> Result<(), ConnectError> {
        let existing = self
            .admitting
            .get(&key)
            .map(|held| (held.peer_id.as_str(), held.offer_digest.as_str()))
            .or_else(|| {
                let request_id = self.pending_by_key.get(&key)?;
                let pending = self.pending.get(request_id)?;
                Some((pending.peer_id.as_str(), pending.offer_digest.as_str()))
            });
        if let Some((peer_id, offer_digest)) = existing {
            if peer_id == admission.peer_id && offer_digest != admission.offer_digest {
                return Err(terminal_peer_invalid(
                    "terminal peer peer_id conflicts with an in-flight offer",
                ));
            }
            return Err(terminal_peer_already_exists(
                "terminal peer negotiation is already pending for this document and worker",
            ));
        }
        if self.pending.len() + self.admitting.len() >= TERMINAL_PEER_MAX_PENDING_NEGOTIATIONS {
            return Err(terminal_peer_exhausted(
                "terminal peer negotiation capacity is exhausted",
            ));
        }
        if count(&self.by_device, &admission.device_fingerprint)
            >= TERMINAL_PEER_MAX_PENDING_NEGOTIATIONS_PER_DEVICE
        {
            return Err(terminal_peer_exhausted(
                "terminal peer device negotiation capacity is exhausted",
            ));
        }
        if count(&self.by_worker, &admission.worker_fp) >= TERMINAL_PEER_MAX_NEGOTIATIONS_PER_WORKER
        {
            return Err(terminal_peer_exhausted(
                "terminal peer worker negotiation capacity is exhausted",
            ));
        }
        increment(&mut self.by_device, &admission.device_fingerprint);
        increment(&mut self.by_worker, &admission.worker_fp);
        self.admitting.insert(key, admission);
        Ok(())
    }

    /// Give back an admission that never became a pending offer.
    pub(crate) fn release_admission(&mut self, key: &PeerKey) {
        if let Some(admission) = self.admitting.remove(key) {
            decrement(&mut self.by_device, &admission.device_fingerprint);
            decrement(&mut self.by_worker, &admission.worker_fp);
        }
    }

    /// Give back every admission; the negotiations are shutting down.
    pub(crate) fn release_all_admissions(&mut self) {
        let keys: Vec<PeerKey> = self.admitting.keys().cloned().collect();
        for key in &keys {
            self.release_admission(key);
        }
    }

    /// A fresh correlation id no pending offer already uses.
    pub(crate) fn allocate_request_id(&self) -> Result<String, ConnectError> {
        for _ in 0..8 {
            let Ok(bytes) = draw::<16>() else { break };
            let request_id = render_v4(bytes);
            if !self.pending.contains_key(&request_id) {
                return Ok(request_id);
            }
        }
        Err(terminal_peer_exhausted(
            "terminal peer request capacity is exhausted",
        ))
    }

    /// Turn the tuple's admission into a pending offer. The admission's
    /// capacity charge moves with it rather than being released and re-taken.
    pub(crate) fn reserve(&mut self, key: &PeerKey, pending: PendingTerminalPeerNegotiation) {
        self.admitting.remove(key);
        self.pending_by_key
            .insert(key.clone(), pending.request_id.clone());
        self.pending.insert(pending.request_id.clone(), pending);
    }

    /// The pending offer under a correlation id.
    pub(crate) fn get(&self, request_id: &str) -> Option<&PendingTerminalPeerNegotiation> {
        self.pending.get(request_id)
    }

    /// Remove a pending offer and release its capacity; `None` when another
    /// path already settled it.
    pub(crate) fn remove(&mut self, request_id: &str) -> Option<PendingTerminalPeerNegotiation> {
        let pending = self.pending.remove(request_id)?;
        let key = (
            pending.owner_key.clone(),
            pending.tab_id.clone(),
            pending.worker_fp.clone(),
        );
        if self.pending_by_key.get(&key) == Some(&pending.request_id) {
            self.pending_by_key.remove(&key);
        }
        decrement(&mut self.by_device, &pending.device_fingerprint);
        decrement(&mut self.by_worker, &pending.worker_fp);
        Some(pending)
    }

    /// The correlation ids of every pending offer `selected` picks.
    pub(crate) fn request_ids_where(
        &self,
        selected: impl Fn(&PendingTerminalPeerNegotiation) -> bool,
    ) -> Vec<String> {
        self.pending
            .values()
            .filter(|pending| selected(pending))
            .map(|pending| pending.request_id.clone())
            .collect()
    }

    /// How many offers await an answer.
    pub(crate) fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

fn count(counts: &HashMap<String, usize>, key: &str) -> usize {
    counts.get(key).copied().unwrap_or_default()
}

fn increment(counts: &mut HashMap<String, usize>, key: &str) {
    *counts.entry(key.to_owned()).or_default() += 1;
}

fn decrement(counts: &mut HashMap<String, usize>, key: &str) {
    match counts.get_mut(key) {
        Some(count) if *count > 1 => *count -= 1,
        _ => {
            counts.remove(key);
        }
    }
}
