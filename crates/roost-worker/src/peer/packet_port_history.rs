//! The history half of the terminal peer packet port: the per-read pre-read
//! reservation a sliced history reader holds, and the worker-wide pressure
//! hook that retires a port holding application bytes. Built by
//! `packet_port`'s `reserve_history_read`; the pressure hook is registered by
//! `TerminalPeerPacketPort::new`. Ports the reservation and
//! `relieveHistoryPressure` half of v2
//! `apps/worker/src/terminal/peer/terminal-peer-packet-port.ts`.

use std::sync::Arc;

use roost_protocol::terminal_peer::packets::TerminalPeerPacketLane;

use super::packet_budget::HistoryPressure;
use super::packet_port::TerminalPeerPacketPort;
use crate::local_terminal::HistoryReadReservation;

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
pub(super) struct PortHistoryReservation {
    pub(super) port: Arc<TerminalPeerPacketPort>,
    pub(super) reservation_id: u64,
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
