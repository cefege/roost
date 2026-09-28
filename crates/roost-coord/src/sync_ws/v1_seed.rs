//! The Sync v1 retained seed, paced by the client's acknowledgements: one
//! retained frame on the wire at a time, the live frames that arrive meanwhile
//! held in a bounded FIFO, and that FIFO drained the same way before live
//! delivery resumes.
//!
//! Owned by `sync_ws::v1_delivery::V1Delivery` (a `flow=1` socket's `seed`);
//! `sync_ws::driver::LinkState::flush_turn` advances it on every socket turn.
//! Ports `apps/coord/src/sync/sync-feed-v1-seed.ts` and `pushPacedSeed` /
//! `waitForDeliveryChange` of `sync-ws-v1-delivery.ts`.
//!
//! ACK-COUPLED, NOT CHUNKED. A seed frame goes out only when everything sent
//! before it was acknowledged, and the next waits for its own acknowledgement,
//! so an arbitrarily large seed cannot outrun the 512-frame / 4 MiB window while
//! live frames wait behind it (`sync-ws-v1-delivery.ts:245-248`).

use std::collections::VecDeque;

use roost_proto::FirehoseFrame;
use roost_proto::buffa::Message;
use tokio::sync::oneshot;

use crate::sync_ws::ack_window::{BackpressureReason, MAX_UNACKED_BYTES, MAX_UNACKED_FRAMES};
use crate::sync_ws::driver::{Delivery, LinkClose, LinkState};
use crate::sync_ws::egress::frame_kind;
use crate::sync_ws::v1_delivery::V1SendRefusal;

/// Why the paced seed stopped its socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedHalt {
    /// The window or the outbox refused the frame.
    Refused(V1SendRefusal),
    /// The frame could not be put on the wire at all; v2 closes `1013` for a
    /// seed frame its guarded send did not carry (`sync-ws-v1-delivery.ts:254-261`).
    Unsent(&'static str),
}

/// What the seed does next.
#[derive(Debug)]
pub enum SeedStep {
    /// Something sent earlier is still unacknowledged.
    Wait,
    /// Send this frame; `live_bytes` is its queued size when it is the live
    /// FIFO's head, which stays charged until it is acknowledged.
    Send {
        /// The frame to send.
        frame: FirehoseFrame,
        /// The live FIFO charge it carries, `None` for a retained frame.
        live_bytes: Option<u64>,
    },
    /// Every retained and queued frame was acknowledged.
    Done,
}

/// One `flow=1` v1 socket's seed in progress.
#[derive(Debug)]
pub struct V1PacedSeed {
    retained: VecDeque<FirehoseFrame>,
    live: VecDeque<(FirehoseFrame, u64)>,
    live_bytes: u64,
    in_flight: Option<(u64, Option<u64>)>,
    seeded: Option<oneshot::Sender<()>>,
}

impl V1PacedSeed {
    /// A seed over `retained`, and the signal its backfill waits on: it fires
    /// when the seed is done and is dropped unfired when the socket is.
    #[must_use]
    pub fn new(retained: Vec<FirehoseFrame>) -> (Self, oneshot::Receiver<()>) {
        let (sender, receiver) = oneshot::channel();
        let seed = Self {
            retained: retained.into(),
            live: VecDeque::new(),
            live_bytes: 0,
            in_flight: None,
            seeded: Some(sender),
        };
        (seed, receiver)
    }

    /// Hold one live frame behind the seed, or refuse it: the FIFO is bounded
    /// by the same 512 frames / 4 MiB as the window, counting the head that is
    /// on the wire (`sync-feed-v1-seed.ts:43-58`).
    pub fn queue_live(&mut self, frame: FirehoseFrame) -> Result<(), V1SendRefusal> {
        let kind = frame_kind(&frame);
        let in_flight_live = usize::from(matches!(self.in_flight, Some((_, Some(_)))));
        if self.live.len() + in_flight_live >= MAX_UNACKED_FRAMES {
            return Err((BackpressureReason::FrameLimit, kind));
        }
        let bytes = frame.try_encoded_len().map_or(u64::MAX, u64::from);
        if bytes > MAX_UNACKED_BYTES.saturating_sub(self.live_bytes) {
            return Err((BackpressureReason::ByteLimit, kind));
        }
        self.live_bytes += bytes;
        self.live.push_back((frame, bytes));
        Ok(())
    }

    /// The next step, given the window's cumulative acknowledgement and how
    /// many frames it still holds unacknowledged.
    pub fn next_step(&mut self, acknowledged: u64, unacked_frames: usize) -> SeedStep {
        if let Some((sequence, live_bytes)) = self.in_flight {
            if acknowledged < sequence {
                return SeedStep::Wait;
            }
            self.in_flight = None;
            self.live_bytes -= live_bytes.unwrap_or(0);
        }
        if self.retained.is_empty() && self.live.is_empty() {
            return SeedStep::Done;
        }
        if unacked_frames > 0 {
            return SeedStep::Wait;
        }
        match self.retained.pop_front() {
            Some(frame) => SeedStep::Send {
                frame,
                live_bytes: None,
            },
            None => match self.live.pop_front() {
                Some((frame, bytes)) => SeedStep::Send {
                    frame,
                    live_bytes: Some(bytes),
                },
                None => SeedStep::Done,
            },
        }
    }

    /// Record the frame just sent under `sequence`.
    pub fn mark_sent(&mut self, sequence: u64, live_bytes: Option<u64>) {
        self.in_flight = Some((sequence, live_bytes));
    }

    /// The seed is over: release the backfill waiting behind it.
    pub fn finish(mut self) {
        if let Some(seeded) = self.seeded.take() {
            let _ = seeded.send(());
        }
    }
}

impl LinkState {
    /// Advance a v1 socket's paced seed as far as its acknowledgements allow,
    /// closing `1013` if a seed frame cannot go out.
    pub(in crate::sync_ws) fn pump_v1_seed(&mut self, now_ms: u64) {
        let Delivery::V1(v1) = &mut self.delivery else {
            return;
        };
        if self.close.is_some() {
            return;
        }
        let halt = match v1.pump_seed(now_ms, &mut self.outbox) {
            Ok(()) => return,
            Err(halt) => halt,
        };
        let (cause, frame) = match halt {
            SeedHalt::Refused((reason, frame)) => (reason.to_string(), frame),
            SeedHalt::Unsent(frame) => ("seed_unsent".to_owned(), frame),
        };
        self.decide_close(LinkClose::BACKPRESSURE, &cause, frame, now_ms);
    }
}
