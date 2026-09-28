//! The Sync v1 live path: a socket that did not negotiate `sync_v=2` gets every
//! frame at once, stamped with the next delivery sequence when it asked for
//! `flow=1`, and checked against the same cumulative-ACK window v2 uses.
//!
//! Owned by `sync_ws::driver` (the `Delivery::V1` arm); `sync_ws::ingress`
//! applies a v1 client's acknowledgements here. Ports the LIVE half of
//! `apps/coord/src/sync/sync-ws-v1-delivery.ts` (`sendGuarded`,
//! `applyCumulativeAck`, `closeForInvalidAck`), its seed pacing
//! (`pushPacedSeed`, whose queue is `sync_ws::v1_seed`), and the v1 branch of
//! `sync-ws-client-ingress.ts`.
//!
//! WHY UNQUEUED. v1 has no domains, no generations and no flush turn: the
//! client (the CLI's headless sync among them) folds frames in arrival order,
//! so there is nothing to reorder and a frame that would pass the window goes
//! straight to the outbox. A frame that would not pass it closes `1013` rather
//! than waiting, because a v1 client has no reset to recover a held frame from.

use roost_proto::buffa::Message;
use roost_proto::{FirehoseFrame, SyncClientFrame};
use tokio::sync::oneshot;

use crate::sync_ws::ack_window::{AckWindow, BackpressureReason, WindowClose, WindowStats};
use crate::sync_ws::driver::Outbox;
use crate::sync_ws::egress::frame_kind;
use crate::sync_ws::v1_seed::{SeedHalt, SeedStep, V1PacedSeed};

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
    /// The retained seed still being paced, on a `flow=1` socket until its
    /// last frame is acknowledged.
    seed: Option<V1PacedSeed>,
}

impl V1Delivery {
    /// A v1 socket, sequenced only when it asked for `flow=1`.
    #[must_use]
    pub fn new(flow_control: bool) -> Self {
        Self {
            window: AckWindow::new(flow_control),
            seed: None,
        }
    }

    /// A `flow=1` socket whose `retained` seed is paced by acknowledgements
    /// before any live frame is sent, and the signal fired when it is done
    /// (`sync-feed.ts:76-84`).
    #[must_use]
    pub fn with_paced_seed(retained: Vec<FirehoseFrame>) -> (Self, oneshot::Receiver<()>) {
        let (seed, seeded) = V1PacedSeed::new(retained);
        tracing::info!(
            event = "sync-ws",
            action = "v1_seed_started",
            "a paced v1 retained seed started"
        );
        let delivery = Self {
            window: AckWindow::new(true),
            seed: Some(seed),
        };
        (delivery, seeded)
    }

    /// Send one live feed frame, or hold it behind a seed in progress.
    pub fn push_live(
        &mut self,
        frame: FirehoseFrame,
        now_ms: u64,
        outbox: &mut Outbox,
    ) -> Result<(), V1SendRefusal> {
        match &mut self.seed {
            Some(seed) => seed.queue_live(frame),
            None => self.send_guarded(frame, now_ms, outbox).map(|_| ()),
        }
    }

    /// Put the seed's next frames on the wire as far as the acknowledgements
    /// allow; the seed ends once its last frame is acknowledged.
    pub fn pump_seed(&mut self, now_ms: u64, outbox: &mut Outbox) -> Result<(), SeedHalt> {
        loop {
            let unacked = self.window.stats(now_ms).unacked_frames;
            let acknowledged = self.window.acknowledged();
            let Some(seed) = self.seed.as_mut() else {
                return Ok(());
            };
            let (frame, live_bytes) = match seed.next_step(acknowledged, unacked) {
                SeedStep::Wait => return Ok(()),
                SeedStep::Done => {
                    if let Some(seed) = self.seed.take() {
                        seed.finish();
                    }
                    tracing::info!(
                        event = "sync-ws",
                        action = "v1_seeded",
                        "the paced v1 retained seed is acknowledged"
                    );
                    return Ok(());
                }
                SeedStep::Send { frame, live_bytes } => (frame, live_bytes),
            };
            let kind = frame_kind(&frame);
            let sequence = self.window.next_sequence();
            if !self
                .send_guarded(frame, now_ms, outbox)
                .map_err(SeedHalt::Refused)?
            {
                return Err(SeedHalt::Unsent(kind));
            }
            if let Some(seed) = self.seed.as_mut() {
                seed.mark_sent(sequence, live_bytes);
            }
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

    /// Send one frame now, or say why the socket must close. `Ok(false)` is a
    /// frame that was not sent.
    ///
    /// The sequence is stamped BEFORE the frame is measured, because the
    /// window is charged the bytes the wire carries and the varint is part of
    /// them. A frame that cannot be encoded is logged and not sent, as v2's
    /// `encode_failed` path: nothing reached the socket, so the window is not
    /// charged and a live frame closes nothing.
    pub fn send_guarded(
        &mut self,
        mut frame: FirehoseFrame,
        now_ms: u64,
        outbox: &mut Outbox,
    ) -> Result<bool, V1SendRefusal> {
        let kind = frame_kind(&frame);
        if self.window.is_enabled() {
            frame.delivery_seq = self.window.next_sequence();
        }
        let encoded = match frame.try_encode_to_vec() {
            Ok(encoded) => encoded,
            Err(error) => {
                tracing::warn!(event = "sync-ws", action = "encode_failed", frame = kind, error = %error);
                return Ok(false);
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
        Ok(true)
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
