//! Retained-byte accounting for this worker's terminal peer packet ports: each
//! direction has independent per-peer and worker ceilings, so slow receive
//! assembly cannot spend the outgoing budget or starve control replies. Built
//! once by `peer::owner`; each `peer::packet_port` takes one peer budget.
//! Ports v2 `apps/worker/src/terminal/peer/terminal-peer-packet-budget.ts`.
//!
//! THE TURN. v2 ran every port, budget and pressure handler on one thread, and
//! a pressure handler retiring another peer ran inside the reservation that
//! needed its bytes. Here every port entry point takes this budget's turn
//! first, so that re-entrant sequence runs with nothing interleaved; what
//! leaves a port (ingress, owner callbacks) is deferred to the turn's end.

use std::mem;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use roost_protocol::terminal_peer::packets::TerminalPeerPacketLane;
use roost_protocol::terminal_peer::peer::{
    TERMINAL_PEER_WORKER_APPLICATION_QUEUE_MAX_BYTES, TERMINAL_PEER_WORKER_CONTROL_QUEUE_MAX_BYTES,
    TERMINAL_PEER_WORKER_QUEUE_MAX_BYTES,
};

use super::peer_budget::TerminalPeerPacketPeerBudget;

/// v2 `TerminalPeerPacketDirection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketDirection {
    Incoming,
    Outgoing,
}

impl PacketDirection {
    pub(super) fn index(self) -> usize {
        match self {
            Self::Incoming => 0,
            Self::Outgoing => 1,
        }
    }
}

/// v2 `TerminalPeerPacketBudgetSnapshot`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PacketBudgetSnapshot {
    pub application_bytes: usize,
    pub control_bytes: usize,
    pub retained_bytes: usize,
}

/// A port that can give back application bytes when the worker runs out
/// (v2 `TerminalPeerHistoryPressureHandler`). True when it released some.
pub(crate) trait HistoryPressure: Send + Sync {
    fn relieve_history_pressure(&self) -> bool;
}

/// Work that leaves the turn: run after it is released, in queue order.
pub(crate) type DeferredEffect = Box<dyn FnOnce() + Send>;

#[derive(Debug, Default)]
struct DirectionBudget {
    application_bytes: usize,
    control_bytes: usize,
}

impl DirectionBudget {
    fn reserve(&mut self, lane: TerminalPeerPacketLane, bytes: usize) -> bool {
        if bytes == 0 {
            return false;
        }
        let over_lane = if lane == TerminalPeerPacketLane::Control {
            self.control_bytes + bytes > TERMINAL_PEER_WORKER_CONTROL_QUEUE_MAX_BYTES
        } else {
            self.application_bytes + bytes > TERMINAL_PEER_WORKER_APPLICATION_QUEUE_MAX_BYTES
        };
        if over_lane
            || self.application_bytes + self.control_bytes + bytes
                > TERMINAL_PEER_WORKER_QUEUE_MAX_BYTES
        {
            return false;
        }
        *self.lane_bytes(lane) += bytes;
        true
    }

    fn release(&mut self, lane: TerminalPeerPacketLane, bytes: usize) {
        let retained = self.lane_bytes(lane);
        if bytes == 0 || bytes > *retained {
            // v2 throws here; the release path has no caller to throw to, so
            // the accounting error is logged and the lane saturates.
            tracing::error!(
                lane = lane.as_str(),
                bytes,
                retained = *retained,
                "terminal peer worker budget underflow"
            );
        }
        *retained = retained.saturating_sub(bytes);
    }

    fn lane_bytes(&mut self, lane: TerminalPeerPacketLane) -> &mut usize {
        if lane == TerminalPeerPacketLane::Control {
            &mut self.control_bytes
        } else {
            &mut self.application_bytes
        }
    }

    fn snapshot(&self) -> PacketBudgetSnapshot {
        PacketBudgetSnapshot {
            application_bytes: self.application_bytes,
            control_bytes: self.control_bytes,
            retained_bytes: self.application_bytes + self.control_bytes,
        }
    }
}

#[derive(Default)]
struct WorkerBudgetState {
    directions: [DirectionBudget; 2],
    pressure: Vec<(u64, Weak<dyn HistoryPressure>)>,
    next_pressure_id: u64,
    disposed: bool,
}

#[derive(Default)]
struct WorkerBudget {
    turn: Mutex<()>,
    state: Mutex<WorkerBudgetState>,
    deferred: Mutex<Vec<DeferredEffect>>,
}

/// Worker-owned aggregate accounting (v2 `TerminalPeerPacketBudget`). Clone
/// shares it; create exactly one peer budget per peer port.
#[derive(Clone, Default)]
pub struct TerminalPeerPacketBudget {
    inner: Arc<WorkerBudget>,
}

impl std::fmt::Debug for TerminalPeerPacketBudget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state();
        formatter
            .debug_struct("TerminalPeerPacketBudget")
            .field("incoming", &state.directions[0].snapshot())
            .field("outgoing", &state.directions[1].snapshot())
            .field("disposed", &state.disposed)
            .finish()
    }
}

/// One port operation's exclusive turn. Dropping it releases the turn, then
/// runs the effects the operation deferred.
pub(crate) struct PacketTurn<'budget> {
    budget: &'budget WorkerBudget,
    guard: Option<MutexGuard<'budget, ()>>,
}

impl Drop for PacketTurn<'_> {
    fn drop(&mut self) {
        let effects = mem::take(&mut *lock(&self.budget.deferred));
        self.guard.take();
        for effect in effects {
            effect();
        }
    }
}

impl TerminalPeerPacketBudget {
    pub fn new() -> Self {
        Self::default()
    }

    /// `None` once disposed (v2 throws "terminal peer packet budget is disposed").
    pub fn create_peer_budget(&self) -> Option<TerminalPeerPacketPeerBudget> {
        if self.state().disposed {
            return None;
        }
        Some(TerminalPeerPacketPeerBudget::new(self.clone()))
    }

    pub fn snapshot(&self, direction: PacketDirection) -> PacketBudgetSnapshot {
        self.state().directions[direction.index()].snapshot()
    }

    pub fn dispose(&self) {
        let mut state = self.state();
        state.disposed = true;
        state.pressure.clear();
        tracing::debug!("the terminal peer packet budget was disposed");
    }

    pub(crate) fn turn(&self) -> PacketTurn<'_> {
        PacketTurn {
            budget: &self.inner,
            guard: Some(lock(&self.inner.turn)),
        }
    }

    /// Queues work for the end of the current turn.
    pub(crate) fn defer(&self, effect: DeferredEffect) {
        lock(&self.inner.deferred).push(effect);
    }

    pub(super) fn register_pressure(&self, handler: Weak<dyn HistoryPressure>) -> u64 {
        let mut state = self.state();
        state.next_pressure_id += 1;
        let id = state.next_pressure_id;
        state.pressure.push((id, handler));
        id
    }

    pub(super) fn unregister_pressure(&self, id: u64) {
        self.state()
            .pressure
            .retain(|(registered, _)| *registered != id);
    }

    /// v2 `reserve`: an outgoing terminal reservation the worker cannot take
    /// asks every other port's history holder to give its bytes back first.
    pub(super) fn reserve(
        &self,
        direction: PacketDirection,
        lane: TerminalPeerPacketLane,
        bytes: usize,
        excluded: Option<u64>,
    ) -> bool {
        let handlers = {
            let mut state = self.state();
            if state.disposed {
                return false;
            }
            if state.directions[direction.index()].reserve(lane, bytes) {
                return true;
            }
            if direction != PacketDirection::Outgoing || lane != TerminalPeerPacketLane::Terminal {
                return false;
            }
            state.pressure.clone()
        };
        for (id, handler) in handlers {
            if Some(id) == excluded {
                continue;
            }
            let Some(handler) = handler.upgrade() else {
                continue;
            };
            if handler.relieve_history_pressure()
                && self.state().directions[direction.index()].reserve(lane, bytes)
            {
                return true;
            }
        }
        false
    }

    pub(super) fn release(
        &self,
        direction: PacketDirection,
        lane: TerminalPeerPacketLane,
        bytes: usize,
    ) {
        self.state().directions[direction.index()].release(lane, bytes);
    }

    fn state(&self) -> MutexGuard<'_, WorkerBudgetState> {
        lock(&self.inner.state)
    }
}

pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
