//! The WebRTC implementation of the direct terminal packet port: framed FIFO
//! queues per lane, reassembly of the control lane, retained-byte quotas and
//! the port's own close. `LocalTerminalSockets` stays the only frame owner.
//! Built by `peer::connection`; driven by `crate::local_terminal` through
//! [`PeerTerminalPacketPort`]. Ports v2
//! `apps/worker/src/terminal/peer/terminal-peer-packet-port.ts`.
//!
//! Every entry point takes the worker budget's turn (see
//! `peer::packet_budget`), so a pressure handler may retire this port in the
//! middle of another port's reservation exactly as v2 did.

use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Instant;

use roost_protocol::terminal_peer::packet_queue::TerminalPeerPacketQueue;
use roost_protocol::terminal_peer::packets::{
    TerminalPeerPacketAssembler, TerminalPeerPacketLane, TerminalPeerPacketQuota,
};
use roost_protocol::terminal_peer::peer::TerminalPeerChannelWatermarks;
use tokio::runtime::Handle;
use tokio::sync::oneshot;
use tokio::task::AbortHandle;

use super::history_reservation::{HistoryQueueQuota, HistoryReservations};
use super::native::NativePeer;
use super::packet_budget::{HistoryPressure, PacketDirection, TerminalPeerPacketBudget, lock};
use super::peer_budget::{TerminalPeerPacketPeerBudget, TerminalPeerQuota};
use crate::local_terminal::{
    HistoryReadReservation, PacketSendResult, PeerIngress, PeerTerminalPacketPort,
    TerminalPacketPort,
};
use crate::uplink::OwnerFuture;

/// A port-level callback, run after the turn that raised it.
pub type PortHook = Arc<dyn Fn(&str) + Send + Sync>;

/// Where reassembled control messages go (v2 `TerminalPeerPacketIngress`).
/// Called only after the port's turn, so it may send on the port.
pub trait TerminalPeerPacketIngress: Send + Sync {
    fn on_message(&self, bytes: &[u8]);
    fn on_close(&self);
}

impl TerminalPeerPacketIngress for PeerIngress {
    fn on_message(&self, bytes: &[u8]) {
        PeerIngress::on_message(self, bytes);
    }

    fn on_close(&self) {
        PeerIngress::on_close(self);
    }
}

/// What one port is built over (v2 `TerminalPeerPacketPortDeps`).
pub struct PacketPortDeps {
    pub socket_id: String,
    /// The connection's native peer; channel `i` carries lane `i`.
    pub native: Arc<dyn NativePeer>,
    pub budget: TerminalPeerPacketBudget,
    pub peer_budget: TerminalPeerPacketPeerBudget,
    /// The port closed, with its reason (v2 `onClosed`).
    pub on_closed: Option<PortHook>,
    /// The port failed, before it closes (v2 `onFatal`).
    pub on_fatal: Option<PortHook>,
    pub runtime: Handle,
}

impl std::fmt::Debug for PacketPortDeps {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PacketPortDeps")
            .field("socket_id", &self.socket_id)
            .finish_non_exhaustive()
    }
}

/// A lane queue's quota: history spends its pre-read reservation first.
#[derive(Debug, Clone)]
pub(super) enum LaneQuota {
    Plain(TerminalPeerQuota),
    History(HistoryQueueQuota),
}

impl TerminalPeerPacketQuota for LaneQuota {
    fn reserve(&mut self, bytes: usize) -> bool {
        match self {
            Self::Plain(quota) => quota.reserve(bytes),
            Self::History(quota) => quota.reserve(bytes),
        }
    }

    fn release(&mut self, bytes: usize) {
        match self {
            Self::Plain(quota) => quota.release(bytes),
            Self::History(quota) => quota.release(bytes),
        }
    }
}

/// Everything a port operation mutates, reached only under the budget's turn.
pub(super) struct PortCore {
    pub(super) queues: [TerminalPeerPacketQueue<LaneQuota>; 3],
    pub(super) assemblers: [TerminalPeerPacketAssembler<TerminalPeerQuota>; 3],
    pub(super) history: HistoryReservations,
    pub(super) ingress: Option<Arc<dyn TerminalPeerPacketIngress>>,
    pub(super) pressure_id: Option<u64>,
    pub(super) authenticated: bool,
    pub(super) closed: bool,
    pub(super) flushing: bool,
    pub(super) flush_scheduled: bool,
    pub(super) backpressured: [bool; 3],
    pub(super) drain_waiters: [Vec<oneshot::Sender<()>>; 3],
    pub(super) setup_timer: Option<AbortHandle>,
    pub(super) partial_timers: [Option<AbortHandle>; 3],
}

/// One native peer's framed direct terminal carrier.
pub struct TerminalPeerPacketPort {
    pub(super) socket_id: String,
    pub(super) native: Arc<dyn NativePeer>,
    pub(super) budget: TerminalPeerPacketBudget,
    pub(super) peer_budget: TerminalPeerPacketPeerBudget,
    pub(super) on_closed: Option<PortHook>,
    pub(super) on_fatal: Option<PortHook>,
    pub(super) runtime: Handle,
    pub(super) origin: Instant,
    pub(super) this: Weak<TerminalPeerPacketPort>,
    pub(super) core: Mutex<PortCore>,
}

impl std::fmt::Debug for TerminalPeerPacketPort {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TerminalPeerPacketPort")
            .field("socket_id", &self.socket_id)
            .finish_non_exhaustive()
    }
}

impl TerminalPeerPacketPort {
    pub fn new(deps: PacketPortDeps) -> Arc<Self> {
        let quota = |direction, lane| deps.peer_budget.quota(direction, lane);
        let history = HistoryReservations::new(
            quota(PacketDirection::Outgoing, TerminalPeerPacketLane::History),
            deps.peer_budget.clone(),
        );
        let queues = TerminalPeerPacketLane::ALL.map(|lane| {
            let lane_quota = if lane == TerminalPeerPacketLane::History {
                LaneQuota::History(history.queue_quota())
            } else {
                LaneQuota::Plain(quota(PacketDirection::Outgoing, lane))
            };
            TerminalPeerPacketQueue::new(lane, lane_quota)
        });
        let assemblers = TerminalPeerPacketLane::ALL.map(|lane| {
            TerminalPeerPacketAssembler::new(lane, quota(PacketDirection::Incoming, lane))
        });
        for lane in TerminalPeerPacketLane::ALL {
            let (_, low_bytes) = TerminalPeerChannelWatermarks::for_lane(lane);
            deps.native
                .set_buffered_amount_low_threshold(lane as usize, low_bytes);
        }
        let port = Arc::new_cyclic(|this: &Weak<Self>| Self {
            socket_id: deps.socket_id,
            native: deps.native,
            budget: deps.budget,
            peer_budget: deps.peer_budget.clone(),
            on_closed: deps.on_closed,
            on_fatal: deps.on_fatal,
            runtime: deps.runtime,
            origin: Instant::now(),
            this: this.clone(),
            core: Mutex::new(PortCore {
                queues,
                assemblers,
                history,
                ingress: None,
                pressure_id: None,
                authenticated: false,
                closed: false,
                flushing: false,
                flush_scheduled: false,
                backpressured: [false; 3],
                drain_waiters: [Vec::new(), Vec::new(), Vec::new()],
                setup_timer: None,
                partial_timers: [None, None, None],
            }),
        });
        let pressure: Weak<dyn HistoryPressure> = port.this.clone();
        let pressure_id = deps.peer_budget.register_history_pressure(pressure);
        let setup_timer = port.arm_setup_deadline();
        {
            let mut core = port.lock_core();
            core.pressure_id = Some(pressure_id);
            core.setup_timer = Some(setup_timer);
        }
        tracing::debug!(socket = %port.socket_id, "a terminal peer packet port opened");
        port
    }

    /// The socket owner's ingress, bound before remote SDP can deliver a frame.
    pub fn attach_ingress(&self, ingress: Arc<dyn TerminalPeerPacketIngress>) -> bool {
        let _turn = self.budget.turn();
        let mut core = self.lock_core();
        if core.closed || core.ingress.is_some() {
            return false;
        }
        core.ingress = Some(ingress);
        true
    }

    pub(super) fn lock_core(&self) -> MutexGuard<'_, PortCore> {
        lock(&self.core)
    }

    pub(super) fn now_ms(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// v2 `fail`: the fatal hook, then the close, both under this turn.
    pub(super) fn fail_in_turn(&self, core: &mut PortCore, reason: &str) {
        if core.closed {
            return;
        }
        tracing::info!(socket = %self.socket_id, reason, "a terminal peer packet port failed");
        if let Some(on_fatal) = &self.on_fatal {
            let on_fatal = Arc::clone(on_fatal);
            let reason = reason.to_owned();
            self.budget.defer(Box::new(move || on_fatal(&reason)));
        }
        self.close_in_turn(core, reason);
    }

    /// v2 `close`: every queue, assembler, timer and waiter is released, the
    /// channels close, and the socket owner and connection hear of it after
    /// the turn.
    pub(super) fn close_in_turn(&self, core: &mut PortCore, reason: &str) {
        if core.closed {
            return;
        }
        core.closed = true;
        if let Some(id) = core.pressure_id.take() {
            self.peer_budget.unregister_history_pressure(id);
        }
        if let Some(timer) = core.setup_timer.take() {
            timer.abort();
        }
        for lane in TerminalPeerPacketLane::ALL {
            let index = lane as usize;
            if let Some(timer) = core.partial_timers[index].take() {
                timer.abort();
            }
            core.queues[index].clear();
            core.assemblers[index].reset();
            self.native.close_channel(index);
            // v2 rejects the waiters; every caller swallows it, so they resolve.
            for waiter in core.drain_waiters[index].drain(..) {
                let _ = waiter.send(());
            }
        }
        self.peer_budget.dispose();
        tracing::info!(socket = %self.socket_id, reason, "a terminal peer packet port closed");
        if let Some(ingress) = core.ingress.clone() {
            self.budget.defer(Box::new(move || ingress.on_close()));
        }
        if let Some(on_closed) = &self.on_closed {
            let on_closed = Arc::clone(on_closed);
            let reason = reason.to_owned();
            self.budget.defer(Box::new(move || on_closed(&reason)));
        }
    }

    fn arm_setup_deadline(&self) -> AbortHandle {
        let port = self.this.clone();
        let deadline = std::time::Duration::from_millis(
            roost_protocol::terminal_peer::peer::TERMINAL_PEER_NEGOTIATION_DEADLINE_MS,
        );
        self.runtime
            .spawn(async move {
                tokio::time::sleep(deadline).await;
                if let Some(port) = port.upgrade() {
                    let _turn = port.budget.turn();
                    let mut core = port.lock_core();
                    if !core.authenticated {
                        port.fail_in_turn(&mut core, "setup_timeout");
                    }
                }
            })
            .abort_handle()
    }
}

impl TerminalPacketPort for TerminalPeerPacketPort {
    fn socket_id(&self) -> &str {
        &self.socket_id
    }

    fn is_open(&self) -> bool {
        let _turn = self.budget.turn();
        !self.lock_core().closed
            && self
                .native
                .is_open(TerminalPeerPacketLane::Control as usize)
    }

    fn send(&self, bytes: Vec<u8>, lane: TerminalPeerPacketLane) -> PacketSendResult {
        let _turn = self.budget.turn();
        let mut core = self.lock_core();
        self.send_in_turn(&mut core, bytes, lane)
    }

    fn close(&self, _code: u16, reason: &str) {
        let _turn = self.budget.turn();
        let mut core = self.lock_core();
        self.close_in_turn(&mut core, reason);
    }
}

impl PeerTerminalPacketPort for TerminalPeerPacketPort {
    /// Concrete-only authentication seam: the Hello matched the offer.
    fn mark_authenticated(&self) {
        let _turn = self.budget.turn();
        let mut core = self.lock_core();
        if core.closed || core.ingress.is_none() {
            return;
        }
        core.authenticated = true;
        if let Some(timer) = core.setup_timer.take() {
            timer.abort();
        }
        tracing::debug!(socket = %self.socket_id, "a terminal peer packet port authenticated");
    }

    /// Reserves the complete direct-history ceiling before the sliced reader
    /// allocates rows.
    fn reserve_history_read(&self, bytes: usize) -> Option<Box<dyn HistoryReadReservation>> {
        let _turn = self.budget.turn();
        let history = self.lock_core().history.clone();
        let reservation_id = history.reserve(bytes)?;
        let port = self.this.upgrade()?;
        Some(Box::new(PortHistoryReservation {
            port,
            reservation_id,
        }))
    }

    /// Resolves once this port no longer owns a complete queued message on the
    /// lane, or once it closed.
    fn wait_for_lane_drain(&self, lane: TerminalPeerPacketLane) -> OwnerFuture<()> {
        let _turn = self.budget.turn();
        let mut core = self.lock_core();
        if core.closed || core.queues[lane as usize].message_count() == 0 {
            return Box::pin(async {});
        }
        let (waiter, drained) = oneshot::channel();
        core.drain_waiters[lane as usize].push(waiter);
        Box::pin(async move {
            let _ = drained.await;
        })
    }
}

impl HistoryPressure for TerminalPeerPacketPort {
    /// v2 `relieveHistoryPressure`: a port holding application bytes retires.
    /// Runs inside another port's turn, never this port's.
    fn relieve_history_pressure(&self) -> bool {
        let mut core = self.lock_core();
        let held = core.history.cancel_for_pressure()
            || core.queues[TerminalPeerPacketLane::Terminal as usize].message_count() > 0
            || core.queues[TerminalPeerPacketLane::History as usize].message_count() > 0;
        if !held {
            return false;
        }
        self.fail_in_turn(&mut core, "application_pressure");
        true
    }
}

/// One read's pre-read reservation (v2 `TerminalPeerHistoryReadReservation`).
struct PortHistoryReservation {
    port: Arc<TerminalPeerPacketPort>,
    reservation_id: u64,
}

impl HistoryReadReservation for PortHistoryReservation {
    fn transfer(&mut self) {
        let _turn = self.port.budget.turn();
        let history = self.port.lock_core().history.clone();
        history.transfer(self.reservation_id);
    }
}

impl Drop for PortHistoryReservation {
    fn drop(&mut self) {
        let _turn = self.port.budget.turn();
        let history = self.port.lock_core().history.clone();
        history.release(self.reservation_id);
    }
}
