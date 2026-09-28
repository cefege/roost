//! The Sync v1 live path: a socket that did not negotiate `sync_v=2` gets every
//! frame at once, stamped with the next delivery sequence when it asked for
//! `flow=1`, and checked against the same cumulative-ACK window v2 uses.
//!
//! Owned by `sync_ws::driver` (the `Delivery::V1` arm); `sync_ws::ingress`
//! applies a v1 client's acknowledgements here. Ports the LIVE half of
//! `apps/coord/src/sync/sync-ws-v1-delivery.ts` (`sendGuarded`,
//! `applyCumulativeAck`, `closeForInvalidAck`) and the v1 branch of
//! `sync-ws-client-ingress.ts`. The retained-seed pacing in that file
//! (`pushPacedSeed`, `waitForDeliveryChange`) is the seed's, not the live path's.
//!
//! WHY UNQUEUED. v1 has no domains, no generations and no flush turn: the
//! client (the CLI's headless sync among them) folds frames in arrival order,
//! so there is nothing to reorder and a frame that would pass the window goes
//! straight to the outbox. A frame that would not pass it closes `1013` rather
//! than waiting, because a v1 client has no reset to recover a held frame from.

use roost_proto::buffa::Message;
use roost_proto::{FirehoseFrame, SyncClientFrame};

use crate::sync_ws::ack_window::{AckWindow, BackpressureReason, WindowClose, WindowStats};
use crate::sync_ws::driver::Outbox;
use crate::sync_ws::egress::frame_kind;

/// Why a v1 send closed the socket: the backpressure reason and the frame
/// kind the close is attributed to.
pub type V1SendRefusal = (BackpressureReason, &'static str);

/// A v1 client frame that is not a bare, in-range cumulative ACK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V1AckViolation;

/// One v1 socket's delivery state.
#[derive(Debug)]
pub struct V1Delivery {
    window: AckWindow,
}

impl V1Delivery {
    /// A v1 socket, sequenced only when it asked for `flow=1`.
    #[must_use]
    pub fn new(flow_control: bool) -> Self {
        Self {
            window: AckWindow::new(flow_control),
        }
    }

    /// Whether this socket negotiated the ACK window. A socket that did not
    /// sends no client frames the coordinator reads.
    #[must_use]
    pub fn flow_control(&self) -> bool {
        self.window.is_enabled()
    }

    /// The window's counters, for a close signal and the ACK deadline.
    #[must_use]
    pub fn window_stats(&self, now_ms: u64) -> WindowStats {
        self.window.stats(now_ms)
    }

    /// Send one frame now, or say why the socket must close.
    ///
    /// The sequence is stamped BEFORE the frame is measured, because the
    /// window is charged the bytes the wire carries and the varint is part of
    /// them. A frame that cannot be encoded is logged and not sent, as v2's
    /// `encode_failed` path: nothing reached the socket, so the window is not
    /// charged and nothing closes.
    pub fn send_guarded(
        &mut self,
        mut frame: FirehoseFrame,
        now_ms: u64,
        outbox: &mut Outbox,
    ) -> Result<(), V1SendRefusal> {
        let kind = frame_kind(&frame);
        if self.window.is_enabled() {
            frame.delivery_seq = self.window.next_sequence();
        }
        let encoded = match frame.try_encode_to_vec() {
            Ok(encoded) => encoded,
            Err(error) => {
                tracing::warn!(event = "sync-ws", action = "encode_failed", frame = kind, error = %error);
                return Ok(());
            }
        };
        let encoded_bytes = encoded.len() as u64;
        if let Err(WindowClose::Backpressure(reason, _)) =
            self.window.may_send(encoded_bytes, now_ms)
        {
            return Err((reason, kind));
        }
        self.window.record_sent(encoded_bytes, now_ms);
        if !outbox.push(encoded) {
            return Err((BackpressureReason::HighWater, kind));
        }
        Ok(())
    }

    /// Apply one v1 client frame, which may only be a bare cumulative ACK.
    ///
    /// `Err` is a protocol violation and closes `1008`: a v1 frame carrying a
    /// command or a socket id is a v2 frame on a socket that never negotiated
    /// v2, and an ACK above the last sent sequence acknowledges something that
    /// was never sent (`sync-ws-client-ingress.ts:54-66`).
    pub fn accept_ack(
        &mut self,
        frame: &SyncClientFrame,
        now_ms: u64,
    ) -> Result<u64, V1AckViolation> {
        let ack = frame.ack_delivery_seq.unwrap_or_default();
        if ack == 0 || frame.command.is_some() || !frame.socket_id.is_empty() {
            return Err(V1AckViolation);
        }
        self.window
            .apply_ack(ack, now_ms)
            .map_err(|_| V1AckViolation)
    }
}
