//! The lane half of the terminal peer packet port: admission into a lane
//! queue, the priority flush that fragments queued messages onto the native
//! channels, control-lane reassembly into the socket owner's ingress, and the
//! per-lane stall and low-water handling. Called through
//! [`super::packet_port::TerminalPeerPacketPort`]'s entry points and by
//! `peer::connection`'s native event pump. Ports the send, receive and flush
//! paths of v2 `apps/worker/src/terminal/peer/terminal-peer-packet-port.ts`.

use std::time::Duration;

use roost_protocol::terminal_peer::packets::{TerminalPeerPacketLane, parse_terminal_peer_packet};
use roost_protocol::terminal_peer::peer::{
    TERMINAL_PEER_LANE_PRIORITY, TERMINAL_PEER_MAX_FLUSH_BYTES_PER_TURN,
    TERMINAL_PEER_PACKET_STALL_MS, TERMINAL_PEER_UNAUTHENTICATED_CONTROL_MAX_BYTES,
    TerminalPeerChannelWatermarks,
};

use super::native::NativePeerEvent;
use super::packet_port::{PortCore, TerminalPeerPacketPort};
use crate::local_terminal::PacketSendResult;

impl TerminalPeerPacketPort {
    /// One native channel event for lane `channel` (connection-level events
    /// are the connection's). v2 `installChannelCallbacks`.
    pub fn on_channel_event(&self, event: NativePeerEvent) {
        let _turn = self.budget.turn();
        let mut core = self.lock_core();
        if core.closed {
            return;
        }
        match event {
            NativePeerEvent::ChannelOpen(_) => self.flush(&mut core),
            NativePeerEvent::BufferedAmountLow(channel) => {
                if let Some(backpressured) = core.backpressured.get_mut(channel) {
                    *backpressured = false;
                }
                self.flush(&mut core);
            }
            NativePeerEvent::ChannelMessage {
                channel,
                binary,
                data,
            } => self.receive(&mut core, channel, binary, data),
            NativePeerEvent::ChannelError(_) => self.fail_in_turn(&mut core, "data_channel_error"),
            NativePeerEvent::ChannelClosed(_) => {
                self.fail_in_turn(&mut core, "data_channel_closed")
            }
            NativePeerEvent::Connected
            | NativePeerEvent::Failed
            | NativePeerEvent::Closed
            | NativePeerEvent::UnsolicitedChannel => {}
        }
    }

    pub(super) fn send_in_turn(
        &self,
        core: &mut PortCore,
        bytes: Vec<u8>,
        lane: TerminalPeerPacketLane,
    ) -> PacketSendResult {
        if core.closed {
            return PacketSendResult::Refused;
        }
        let index = lane as usize;
        core.backpressured[index] = false;
        let queued_before = core.queues[index].queued_bytes();
        if !matches!(core.queues[index].enqueue(bytes), Ok(true)) {
            return PacketSendResult::Refused;
        }
        let was_backpressured =
            !self.native.is_open(index) || self.is_flow_controlled(lane) || queued_before > 0;
        self.flush(core);
        if core.closed {
            return PacketSendResult::Refused;
        }
        let result =
            if was_backpressured || core.backpressured[index] || self.is_flow_controlled(lane) {
                PacketSendResult::Backpressured
            } else {
                PacketSendResult::Accepted
            };
        core.backpressured[index] = false;
        result
    }

    fn receive(&self, core: &mut PortCore, channel: usize, binary: bool, data: Vec<u8>) {
        let Some(ingress) = core.ingress.clone() else {
            self.fail_in_turn(core, "unexpected_client_data");
            return;
        };
        if channel != TerminalPeerPacketLane::Control as usize || !binary {
            self.fail_in_turn(core, "unexpected_client_data");
            return;
        }
        let lane = TerminalPeerPacketLane::Control;
        let header = match parse_terminal_peer_packet(lane, &data) {
            Ok(packet) => packet.header,
            Err(_) => {
                self.fail_in_turn(core, "packet_rejected");
                return;
            }
        };
        if !core.authenticated
            && header.total_bytes as usize > TERMINAL_PEER_UNAUTHENTICATED_CONTROL_MAX_BYTES
        {
            self.fail_in_turn(core, "unauthenticated_frame_too_large");
            return;
        }
        let complete = match core.assemblers[lane as usize].push(&data, self.now_ms()) {
            Ok(complete) => complete,
            Err(_) => {
                self.fail_in_turn(core, "packet_rejected");
                return;
            }
        };
        self.arm_partial_deadline(core, lane);
        if let Some(message) = complete {
            self.budget
                .defer(Box::new(move || ingress.on_message(&message)));
        }
    }

    fn arm_partial_deadline(&self, core: &mut PortCore, lane: TerminalPeerPacketLane) {
        let index = lane as usize;
        if let Some(timer) = core.partial_timers[index].take() {
            timer.abort();
        }
        if !core.assemblers[index].has_partial_message() {
            return;
        }
        let port = self.this.clone();
        let timer = self.runtime.spawn(async move {
            tokio::time::sleep(Duration::from_millis(TERMINAL_PEER_PACKET_STALL_MS)).await;
            let Some(port) = port.upgrade() else {
                return;
            };
            let _turn = port.budget.turn();
            let mut core = port.lock_core();
            core.partial_timers[index] = None;
            if core.assemblers[index].expire(port.now_ms()) {
                port.fail_in_turn(&mut core, "packet_stalled");
            }
        });
        core.partial_timers[index] = Some(timer.abort_handle());
    }

    /// Sends queued fragments in lane priority until every lane is empty,
    /// flow-controlled or closed, yielding after one turn's byte budget.
    pub(super) fn flush(&self, core: &mut PortCore) {
        if core.closed || core.flushing {
            return;
        }
        core.flushing = true;
        let mut sent_bytes = 0usize;
        let mut should_yield = false;
        loop {
            let mut progressed = false;
            for lane in TERMINAL_PEER_LANE_PRIORITY {
                let index = lane as usize;
                if self.is_flow_controlled(lane) || !self.native.is_open(index) {
                    continue;
                }
                let fragment = match core.queues[index].next_fragment() {
                    Ok(Some(fragment)) => fragment,
                    Ok(None) => continue,
                    Err(_) => {
                        core.flushing = false;
                        self.fail_in_turn(core, "packet_queue_failed");
                        return;
                    }
                };
                let length = fragment.bytes().len();
                if sent_bytes > 0 && sent_bytes + length > TERMINAL_PEER_MAX_FLUSH_BYTES_PER_TURN {
                    should_yield = true;
                    break;
                }
                // Ok(false) is buffered acceptance, never a refusal: the
                // fragment is the transport's now.
                let sent = self.native.send(index, fragment.bytes());
                let Ok(sent_now) = sent else {
                    drop(fragment);
                    core.flushing = false;
                    self.fail_in_turn(core, "native_send_failed");
                    return;
                };
                fragment.commit();
                core.backpressured[index] = !sent_now;
                resolve_drain_waiters(core, index);
                sent_bytes += length;
                progressed = true;
                if sent_bytes >= TERMINAL_PEER_MAX_FLUSH_BYTES_PER_TURN {
                    should_yield = true;
                    break;
                }
            }
            if !progressed || should_yield || core.closed {
                break;
            }
        }
        core.flushing = false;
        if should_yield && !core.closed {
            self.schedule_flush(core);
        }
    }

    /// v2 `setTimeout(flush, 0)`: the rest of the queue goes on a later turn.
    fn schedule_flush(&self, core: &mut PortCore) {
        let queued = core.queues.iter().any(|queue| queue.message_count() > 0);
        if core.flush_scheduled || !queued {
            return;
        }
        core.flush_scheduled = true;
        let port = self.this.clone();
        self.runtime.spawn(async move {
            tokio::task::yield_now().await;
            let Some(port) = port.upgrade() else {
                return;
            };
            let _turn = port.budget.turn();
            let mut core = port.lock_core();
            core.flush_scheduled = false;
            port.flush(&mut core);
        });
    }

    fn is_flow_controlled(&self, lane: TerminalPeerPacketLane) -> bool {
        let (high_bytes, _) = TerminalPeerChannelWatermarks::for_lane(lane);
        self.native.buffered_amount(lane as usize) >= high_bytes
    }
}

fn resolve_drain_waiters(core: &mut PortCore, index: usize) {
    if core.queues[index].message_count() != 0 {
        return;
    }
    for waiter in core.drain_waiters[index].drain(..) {
        let _ = waiter.send(());
    }
}
