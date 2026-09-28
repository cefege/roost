//! The outgoing half of an attachment peer port and its carrier contract:
//! fragments leave the per-lane queues only while the native channel is open,
//! a drain-then-close waits for both queues and native buffers, and a send
//! reports backpressure the way the loopback carrier does. Called by
//! `direct_sockets` through `AttachmentTransferPort`. Ports `send`, `flush`,
//! `closeAfterDrain` and `closeIfDrained` of
//! `apps/worker/src/attachments/attachment-peer-packet-port.ts`.

use std::time::Duration;

use roost_protocol::attachment_transfer::{ACK_DEADLINE_MS, PeerChannelLane};

use super::peer_packet_port::{AttachmentPeerPacketPort, LANES, PortState, lane_index};
use super::transfer_port::{AttachmentTransferPort, PortKind, SendResult};

impl AttachmentPeerPacketPort {
    /// A native `false` is buffered acceptance: the fragment is committed once
    /// either way, so a buffered send is never duplicated.
    pub(super) fn flush(&self, state: &mut PortState) {
        for lane in LANES {
            let index = lane_index(lane);
            while !state.closed && self.native.is_open(index) {
                let fragment = match state.lanes[index].queue.next_fragment() {
                    Ok(Some(fragment)) => fragment,
                    Ok(None) => break,
                    Err(_) => {
                        self.close_locked(state, "packet_rejected");
                        return;
                    }
                };
                if self.native.send(index, fragment.bytes()).is_err() {
                    self.close_locked(state, "native_send_failed");
                    return;
                }
                fragment.commit();
            }
        }
        self.close_if_drained(state);
    }

    fn close_if_drained(&self, state: &mut PortState) {
        let drained = state
            .lanes
            .iter()
            .all(|lane| lane.queue.message_count() == 0)
            && LANES
                .iter()
                .all(|lane| self.native.buffered_amount(lane_index(*lane)) == 0);
        if state.closed || !drained {
            return;
        }
        if let Some(reason) = state.close_when_drained.clone() {
            self.close_locked(state, &reason);
        }
    }
}

impl AttachmentTransferPort for AttachmentPeerPacketPort {
    fn socket_id(&self) -> &str {
        &self.socket_id
    }

    fn kind(&self) -> PortKind {
        PortKind::Webrtc
    }

    fn is_open(&self) -> bool {
        !self.lock().closed && self.native.is_open(lane_index(PeerChannelLane::Control))
    }

    fn send(&self, bytes: Vec<u8>, lane: PeerChannelLane) -> SendResult {
        let mut state = self.lock();
        if state.closed {
            return SendResult::Refused;
        }
        let index = lane_index(lane);
        let queued_before = state.lanes[index].queue.queued_bytes();
        if !matches!(state.lanes[index].queue.enqueue(bytes), Ok(true)) {
            return SendResult::Refused;
        }
        let was_backpressured = queued_before > 0
            || !self.native.is_open(index)
            || self.native.buffered_amount(index) > 0;
        self.flush(&mut state);
        if state.closed {
            SendResult::Refused
        } else if was_backpressured {
            SendResult::Backpressured
        } else {
            SendResult::Accepted
        }
    }

    fn close(&self, _code: Option<u16>, reason: &str) {
        let mut state = self.lock();
        self.close_locked(&mut state, reason);
    }

    fn close_after_drain(&self, reason: &str) {
        let mut state = self.lock();
        if state.closed {
            return;
        }
        state.close_when_drained = Some(reason.to_owned());
        self.flush(&mut state);
        if state.closed || state.drain_timer.is_some() {
            return;
        }
        let this = self.this.clone();
        state.drain_timer = Some(
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(ACK_DEADLINE_MS)).await;
                if let Some(port) = this.upgrade() {
                    let mut state = port.lock();
                    state.drain_timer = None;
                    port.close_locked(&mut state, "ack_timeout");
                }
            })
            .abort_handle(),
        );
    }

    fn mark_authenticated(&self) {
        let mut state = self.lock();
        if state.closed || state.ingress.is_none() {
            return;
        }
        state.authenticated = true;
        if let Some(timer) = state.hello_timer.take() {
            timer.abort();
        }
    }
}
