//! One peer port's two-direction byte reservations, charged to the worker
//! budget: the queues and assemblers of `peer::packet_port` each hold a lane
//! quota from here, and a port's close keeps the worker charged while a hold
//! (an in-flight history read) outlives it. Created by
//! [`super::packet_budget::TerminalPeerPacketBudget::create_peer_budget`]. Ports
//! `TerminalPeerPacketPeerBudget` of v2 `apps/worker/src/terminal/peer/terminal-peer-packet-budget.ts`.

use std::sync::{Arc, Mutex, MutexGuard, Weak};

use roost_protocol::terminal_peer::packets::{TerminalPeerPacketLane, TerminalPeerPacketQuota};
use roost_protocol::terminal_peer::peer::{
    TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES, TERMINAL_PEER_CONTROL_QUEUE_MAX_BYTES,
};

use super::packet_budget::{
    HistoryPressure, PacketBudgetSnapshot, PacketDirection, TerminalPeerPacketBudget, lock,
};

const DIRECTIONS: [PacketDirection; 2] = [PacketDirection::Incoming, PacketDirection::Outgoing];

#[derive(Debug, Default)]
struct PeerBudgetState {
    retained: [[usize; 3]; 2],
    dispose_requested: bool,
    disposed: bool,
    holds: usize,
    pressure_id: Option<u64>,
}

#[derive(Debug)]
struct PeerBudgetInner {
    worker: TerminalPeerPacketBudget,
    state: Mutex<PeerBudgetState>,
}

/// v2 `TerminalPeerPacketPeerBudget`. Clone shares it.
#[derive(Debug, Clone)]
pub struct TerminalPeerPacketPeerBudget {
    inner: Arc<PeerBudgetInner>,
}

/// One lane of one direction, as the protocol's queue and assembler reserve
/// against it (v2 `quota(direction, lane)`).
#[derive(Debug, Clone)]
pub struct TerminalPeerQuota {
    budget: TerminalPeerPacketPeerBudget,
    direction: PacketDirection,
    lane: TerminalPeerPacketLane,
}

impl TerminalPeerPacketQuota for TerminalPeerQuota {
    fn reserve(&mut self, bytes: usize) -> bool {
        self.budget.reserve(self.direction, self.lane, bytes)
    }

    fn release(&mut self, bytes: usize) {
        self.budget.release(self.direction, self.lane, bytes);
    }
}

/// Keeps the worker charged while async work outlives a closing port
/// (v2 `hold()`'s release function, run on drop).
#[derive(Debug)]
pub struct PeerBudgetHold {
    budget: TerminalPeerPacketPeerBudget,
}

impl Drop for PeerBudgetHold {
    fn drop(&mut self) {
        let mut state = self.budget.state();
        state.holds = state.holds.saturating_sub(1);
        drop(state);
        self.budget.finish_dispose();
    }
}

impl TerminalPeerPacketPeerBudget {
    pub(super) fn new(worker: TerminalPeerPacketBudget) -> Self {
        Self {
            inner: Arc::new(PeerBudgetInner {
                worker,
                state: Mutex::new(PeerBudgetState::default()),
            }),
        }
    }

    pub fn quota(&self, direction: PacketDirection, lane: TerminalPeerPacketLane) -> TerminalPeerQuota {
        TerminalPeerQuota {
            budget: self.clone(),
            direction,
            lane,
        }
    }

    /// Registers this peer's port as a history holder the worker may retire,
    /// and excludes it from relieving its own reservations.
    pub(crate) fn register_history_pressure(&self, handler: Weak<dyn HistoryPressure>) -> u64 {
        let id = self.inner.worker.register_pressure(handler);
        self.state().pressure_id = Some(id);
        id
    }

    pub(crate) fn unregister_history_pressure(&self, id: u64) {
        self.inner.worker.unregister_pressure(id);
        let mut state = self.state();
        if state.pressure_id == Some(id) {
            state.pressure_id = None;
        }
    }

    /// `None` once the peer is closing (v2 throws "terminal peer packet budget is closing").
    pub fn hold(&self) -> Option<PeerBudgetHold> {
        let mut state = self.state();
        if state.dispose_requested || state.disposed {
            return None;
        }
        state.holds += 1;
        Some(PeerBudgetHold {
            budget: self.clone(),
        })
    }

    pub fn retained_bytes(&self, direction: PacketDirection) -> usize {
        self.state().retained[direction.index()].iter().sum()
    }

    pub fn snapshot(&self, direction: PacketDirection) -> PacketBudgetSnapshot {
        let retained = self.state().retained[direction.index()];
        let application_bytes = retained[TerminalPeerPacketLane::Terminal as usize]
            + retained[TerminalPeerPacketLane::History as usize];
        let control_bytes = retained[TerminalPeerPacketLane::Control as usize];
        PacketBudgetSnapshot {
            application_bytes,
            control_bytes,
            retained_bytes: application_bytes + control_bytes,
        }
    }

    pub fn dispose(&self) {
        let mut state = self.state();
        if state.dispose_requested || state.disposed {
            return;
        }
        state.dispose_requested = true;
        drop(state);
        self.finish_dispose();
    }

    fn reserve(&self, direction: PacketDirection, lane: TerminalPeerPacketLane, bytes: usize) -> bool {
        let excluded = {
            let state = self.state();
            if state.dispose_requested || state.disposed || bytes == 0 {
                return false;
            }
            let retained = &state.retained[direction.index()];
            let control = retained[TerminalPeerPacketLane::Control as usize];
            let application = retained[TerminalPeerPacketLane::Terminal as usize]
                + retained[TerminalPeerPacketLane::History as usize];
            let over_lane = if lane == TerminalPeerPacketLane::Control {
                control + bytes > TERMINAL_PEER_CONTROL_QUEUE_MAX_BYTES
            } else {
                application + bytes > TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES
            };
            if over_lane
                || application + control + bytes
                    > TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES + TERMINAL_PEER_CONTROL_QUEUE_MAX_BYTES
            {
                return false;
            }
            state.pressure_id
        };
        if !self.inner.worker.reserve(direction, lane, bytes, excluded) {
            return false;
        }
        let mut state = self.state();
        if state.dispose_requested || state.disposed {
            drop(state);
            self.inner.worker.release(direction, lane, bytes);
            return false;
        }
        state.retained[direction.index()][lane as usize] += bytes;
        true
    }

    fn release(&self, direction: PacketDirection, lane: TerminalPeerPacketLane, bytes: usize) {
        let mut state = self.state();
        if state.disposed {
            return;
        }
        let retained = &mut state.retained[direction.index()][lane as usize];
        if bytes == 0 || bytes > *retained {
            // v2 throws "terminal peer packet peer budget underflow".
            tracing::error!(lane = lane.as_str(), bytes, retained = *retained, "terminal peer budget underflow");
            return;
        }
        *retained -= bytes;
        drop(state);
        self.inner.worker.release(direction, lane, bytes);
    }

    /// Returns every retained byte to the worker once disposal was asked for
    /// and no hold remains.
    fn finish_dispose(&self) {
        let mut state = self.state();
        if !state.dispose_requested || state.disposed || state.holds != 0 {
            return;
        }
        state.disposed = true;
        let retained = std::mem::take(&mut state.retained);
        drop(state);
        for direction in DIRECTIONS {
            for lane in TerminalPeerPacketLane::ALL {
                let bytes = retained[direction.index()][lane as usize];
                if bytes != 0 {
                    self.inner.worker.release(direction, lane, bytes);
                }
            }
        }
        tracing::debug!("a terminal peer budget returned its bytes to the worker");
    }

    fn state(&self) -> MutexGuard<'_, PeerBudgetState> {
        lock(&self.inner.state)
    }
}
