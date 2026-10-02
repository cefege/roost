//! The smoke harness's reach into live terminal peers: a malformed control
//! packet fed through a port's real reassembly boundary, and the history lane
//! held out of the flush. Called by `smoke_faults` through
//! [`TerminalPeerOwner`]; an ordinary worker has no caller. Ports
//! `injectMalformedPacketForTest` / `setHistoryDeliveryPausedForTest` of v2
//! `terminal-peer-owner.ts`, `terminal-peer-connection.ts` and
//! `terminal-peer-packet-port.ts`.

use std::sync::Arc;

use roost_protocol::terminal_peer::packets::TerminalPeerPacketLane;

use super::connection::TerminalPeerConnection;
use super::faults::MalformedPacket;
use super::owner::TerminalPeerOwner;
use super::packet_port::TerminalPeerPacketPort;

impl TerminalPeerPacketPort {
    /// Feed `packet` to this port's control reassembly as if the peer sent it;
    /// the parser refuses it and only this port closes. `false` when the port
    /// is closed or not yet authenticated.
    pub fn inject_malformed_control(&self, packet: MalformedPacket) -> bool {
        let _turn = self.budget.turn();
        let mut core = self.lock_core();
        if core.closed || !core.authenticated {
            return false;
        }
        let control = TerminalPeerPacketLane::Control as usize;
        self.receive(&mut core, control, true, packet.control_packet());
        true
    }

    /// Hold the history lane out of the flush; releasing it flushes at once.
    /// Control and terminal lanes are untouched.
    pub fn set_history_paused(&self, paused: bool) {
        let _turn = self.budget.turn();
        let mut core = self.lock_core();
        if core.closed {
            return;
        }
        core.history_paused = paused;
        if !paused {
            self.flush(&mut core);
        }
    }
}

impl TerminalPeerOwner {
    /// Inject into the first active peer that accepts it.
    pub fn inject_malformed_packet(&self, packet: MalformedPacket) -> bool {
        self.active_connections()
            .iter()
            .any(|connection| connection.port().inject_malformed_control(packet))
    }

    /// Pause or resume history delivery on every active peer.
    pub fn set_history_paused(&self, paused: bool) {
        for connection in self.active_connections() {
            connection.port().set_history_paused(paused);
        }
    }

    // Collected first: a port that closes under a fault reports back to this
    // owner, which must not still hold its own state lock.
    fn active_connections(&self) -> Vec<Arc<TerminalPeerConnection>> {
        self.state()
            .active
            .values()
            .map(|active| Arc::clone(&active.connection))
            .collect()
    }
}
