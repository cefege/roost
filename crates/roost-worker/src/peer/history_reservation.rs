//! The direct-history pre-read quota and its atomic handoff to the history
//! queue: one read owns the worst-case reservation, and only that read's
//! explicitly transferred final response replaces it with exact queue bytes.
//! Owned by `peer::packet_port` (its history queue's quota and
//! `reserve_history_read`). Ports v2
//! `apps/worker/src/terminal/peer/terminal-peer-history-reservation.ts`.

use std::sync::{Arc, Mutex, MutexGuard};

use roost_protocol::terminal_peer::packets::TerminalPeerPacketQuota;

use super::packet_budget::lock;
use super::peer_budget::{PeerBudgetHold, TerminalPeerPacketPeerBudget, TerminalPeerQuota};

#[derive(Debug, Default)]
struct ReservationState {
    bytes: usize,
    reservation_id: u64,
    transfer_id: Option<u64>,
    hold: Option<PeerBudgetHold>,
}

/// v2 `TerminalPeerHistoryReservationOwner`. Clone shares it: the history
/// queue's quota and the port both reach one state.
#[derive(Debug, Clone)]
pub(crate) struct HistoryReservations {
    base: TerminalPeerQuota,
    peer_budget: TerminalPeerPacketPeerBudget,
    state: Arc<Mutex<ReservationState>>,
}

impl HistoryReservations {
    pub(crate) fn new(base: TerminalPeerQuota, peer_budget: TerminalPeerPacketPeerBudget) -> Self {
        Self {
            base,
            peer_budget,
            state: Arc::new(Mutex::new(ReservationState::default())),
        }
    }

    /// The history queue's quota (v2 `queueQuota`).
    pub(crate) fn queue_quota(&self) -> HistoryQueueQuota {
        HistoryQueueQuota {
            owner: self.clone(),
        }
    }

    /// The reservation id of a new read, or `None` while another read holds
    /// the ceiling or the budget refuses it.
    pub(crate) fn reserve(&self, bytes: usize) -> Option<u64> {
        let mut state = self.state();
        if state.bytes != 0 || !self.base.clone().reserve(bytes) {
            return None;
        }
        let Some(hold) = self.peer_budget.hold() else {
            self.base.clone().release(bytes);
            return None;
        };
        state.reservation_id += 1;
        state.bytes = bytes;
        state.hold = Some(hold);
        Some(state.reservation_id)
    }

    /// The read's final response is about to be queued: the next history
    /// admission may spend the reservation.
    pub(crate) fn transfer(&self, reservation_id: u64) {
        let mut state = self.state();
        if state.reservation_id == reservation_id && state.bytes != 0 {
            state.transfer_id = Some(reservation_id);
        }
    }

    /// The read ended: whatever it still reserves goes back, and so does its hold.
    pub(crate) fn release(&self, reservation_id: u64) {
        let mut state = self.state();
        if state.reservation_id != reservation_id {
            return;
        }
        let reserved = std::mem::take(&mut state.bytes);
        state.transfer_id = None;
        let hold = state.hold.take();
        drop(state);
        if reserved != 0 {
            self.base.clone().release(reserved);
        }
        drop(hold);
    }

    /// v2 `cancelForPressure`: true when an outstanding read was cancelled.
    pub(crate) fn cancel_for_pressure(&self) -> bool {
        let mut state = self.state();
        if state.bytes == 0 {
            return false;
        }
        let reserved = std::mem::take(&mut state.bytes);
        state.transfer_id = None;
        state.reservation_id += 1;
        let hold = state.hold.take();
        drop(state);
        self.base.clone().release(reserved);
        drop(hold);
        true
    }

    fn reserve_queued(&self, bytes: usize) -> bool {
        let mut state = self.state();
        if state.bytes != 0 && state.transfer_id == Some(state.reservation_id) {
            let reserved = std::mem::take(&mut state.bytes);
            state.transfer_id = None;
            drop(state);
            self.base.clone().release(reserved);
        } else {
            drop(state);
        }
        self.base.clone().reserve(bytes)
    }

    fn state(&self) -> MutexGuard<'_, ReservationState> {
        lock(&self.state)
    }
}

/// The history queue's quota: a transferred reservation is released just
/// before the queue reserves the response's exact bytes.
#[derive(Debug, Clone)]
pub(crate) struct HistoryQueueQuota {
    owner: HistoryReservations,
}

impl TerminalPeerPacketQuota for HistoryQueueQuota {
    fn reserve(&mut self, bytes: usize) -> bool {
        self.owner.reserve_queued(bytes)
    }

    fn release(&mut self, bytes: usize) {
        self.owner.base.clone().release(bytes);
    }
}
