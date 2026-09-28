//! One Sync socket's shared link: the state the bus listeners and the socket
//! task both mutate, the outbound buffer between them, and the flush turn that
//! turns queued application frames into bytes.
//!
//! Owned by `sync_ws::socket`, which builds one per socket and is its only
//! writer to the wire; `sync_ws::live_feed` delivers bus frames into it and
//! `sync_ws::ingress` applies client frames to it. Ports the send half of
//! `apps/coord/src/sync/sync-ws-v2-egress.ts` (`flushV2`, `enqueueV2Frame`,
//! `resetV2Domain`), `sync-ws-v2-control.ts`, the close paths of
//! `sync-ws-v1-delivery.ts`, and the per-socket record of
//! `sync-ws-v2-state.ts` / `sync-ws-v2-state-types.ts` that the pure session
//! does not already hold.
//!
//! THE OUTBOX IS THE PORT OF BUN'S SEND BUFFER, NOT A SECOND QUEUE. v2 called
//! `ws.send` from inside a bus listener and Bun buffered the bytes; its
//! high-water check read that buffer after every send. A tokio socket is
//! written by one task, so a listener appends encoded bytes here and wakes that
//! task -- and the same high-water mark closes `1013` when the bytes pile up.
//! Application frames never reach it until a flush turn releases them, so the
//! ACK window still bounds what is in flight, exactly as v2's did.

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard, PoisonError};

use roost_proto::FirehoseFrame;
use roost_proto::buffa::Message;
use tokio::sync::Notify;

use crate::sync_ws::ack_window::{
    ACK_TIMEOUT_MS, BACKPRESSURE_CLOSE_CODE, BACKPRESSURE_REASON, BackpressureReason,
    INVALID_ACK_CLOSE_CODE, INVALID_ACK_REASON, WindowStats,
};
use crate::sync_ws::admission::EnqueueOutcome;
use crate::sync_ws::commands::ClientContext;
use crate::sync_ws::control_frames::{control_frame, keepalive_frame};
use crate::sync_ws::domain_table::FLUSH_BATCH_FRAMES;
use crate::sync_ws::egress::{FlushStep, frame_kind};
use crate::sync_ws::feed::FeedFrame;
use crate::sync_ws::resource_index::SyncResourceIndex;
use crate::sync_ws::session::{SessionClose, SyncV2Session};
use crate::sync_ws::terminal::snapshot::NoTerminalSnapshotHub;
use crate::sync_ws::v1_delivery::V1Delivery;

/// Encoded bytes the outbox may hold before the socket closes `high_water`
/// (`sync-ws-handler.ts:61`, `BACKPRESSURE_LIMIT_BYTES`).
pub const SOCKET_HIGH_WATER_BYTES: u64 = 8 * 1024 * 1024;

/// How long one write may block before the socket closes `timeout`
/// (`sync-ws-handler.ts:62`, `BACKPRESSURE_TIMEOUT_MS`).
pub const SOCKET_WRITE_TIMEOUT_MS: u64 = 10_000;

/// A close the socket must perform: the first one decided wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkClose {
    /// The WebSocket close code.
    pub code: u16,
    /// The close reason, exactly as v2 spells it.
    pub reason: &'static str,
}

impl LinkClose {
    /// `1013 sync backpressure`, every backpressure path's close.
    pub const BACKPRESSURE: Self = Self {
        code: BACKPRESSURE_CLOSE_CODE,
        reason: BACKPRESSURE_REASON,
    };
    /// `1008 invalid sync ack`, the policy-violation close.
    pub const INVALID_ACK: Self = Self {
        code: INVALID_ACK_CLOSE_CODE,
        reason: INVALID_ACK_REASON,
    };
}

/// How this socket delivers: the v1 live path, or the v2 session.
#[derive(Debug)]
pub enum Delivery {
    /// No `sync_v=2`: every frame is sent at once, window-checked.
    V1(V1Delivery),
    /// `flow=1&sync_v=2`: domain queues, generations and the flush turn.
    V2(Box<SyncV2Session>),
}

/// Encoded frames waiting for the socket task to write them.
#[derive(Debug, Default)]
pub struct Outbox {
    frames: VecDeque<Vec<u8>>,
    bytes: u64,
}

impl Outbox {
    /// Append one encoded frame. `false` once the buffer is past its
    /// high-water mark, which the caller turns into a `1013` close.
    pub fn push(&mut self, encoded: Vec<u8>) -> bool {
        self.bytes = self.bytes.saturating_add(encoded.len() as u64);
        self.frames.push_back(encoded);
        self.bytes <= SOCKET_HIGH_WATER_BYTES
    }

    /// Everything buffered, in order, for one write pass.
    pub fn take(&mut self) -> Vec<Vec<u8>> {
        self.bytes = 0;
        self.frames.drain(..).collect()
    }
}

/// Everything one socket's listeners and task share.
#[derive(Debug)]
pub struct LinkState {
    /// The opaque socket id; empty on a v1 socket, which has none.
    pub socket_id: String,
    /// The verified caller's fingerprint, for every close signal.
    pub caller_fp: String,
    /// The delivery path this socket negotiated.
    pub delivery: Delivery,
    /// What the socket may do, handed to the command gate.
    pub context: ClientContext,
    /// What the socket may observe, kept current by the live feed.
    pub index: SyncResourceIndex,
    /// A worker socket's sessions, retained after close so a late `closed`
    /// still reaches it (`sync-feed.ts:96-114`); `None` for a browser.
    pub owned_session_ids: Option<std::collections::BTreeSet<String>>,
    /// Live session events at or below this durable id were already held by
    /// the client (`sync-feed.ts:92`, `replayedSessionCutoff`).
    pub replayed_cutoff: u64,
    /// Bytes for the socket task to write.
    pub outbox: Outbox,
    /// The close this socket will perform, once decided.
    pub close: Option<LinkClose>,
    /// The terminal screen hub a lane asks for a rebaseline. A socket with no
    /// screen replica wired asks nobody (`terminal::snapshot`).
    pub screen: NoTerminalSnapshotHub,
}

impl LinkState {
    /// Decide this socket's close, once. Every queue is released at the same
    /// moment so a teardown in flight cannot admit another frame.
    pub fn decide_close(&mut self, close: LinkClose, cause: &str, frame: &str, now_ms: u64) {
        if self.close.is_some() {
            return;
        }
        let stats = self.window_stats(now_ms);
        self.close = Some(close);
        if let Delivery::V2(session) = &mut self.delivery {
            session.retire();
        }
        tracing::warn!(
            event = "sync-ws",
            action = "close_decided",
            caller_fp = %self.caller_fp,
            socket_id = %self.socket_id,
            code = close.code,
            reason = cause,
            frame,
            unacked_frames = stats.unacked_frames,
            unacked_bytes = stats.unacked_bytes,
            oldest_age_ms = stats.oldest_age_ms,
            "sync socket closing"
        );
    }

    /// Close for a fault the session decided.
    pub fn close_for_session(&mut self, close: SessionClose, frame: &str, now_ms: u64) {
        match close {
            SessionClose::Backpressure { reason, .. } => {
                self.decide_close(LinkClose::BACKPRESSURE, &reason.to_string(), frame, now_ms);
            }
            SessionClose::InvalidAck => {
                self.decide_close(LinkClose::INVALID_ACK, "invalid_ack", frame, now_ms);
            }
        }
    }

    /// Admit one feed frame on this socket's delivery path.
    pub fn deliver(&mut self, frame: FeedFrame, now_ms: u64) {
        if self.close.is_some() {
            return;
        }
        match &mut self.delivery {
            Delivery::V1(v1) => {
                let outcome = v1.send_guarded(frame.into_frame(), now_ms, &mut self.outbox);
                if let Err((reason, kind)) = outcome {
                    self.close_for_backpressure(reason, kind, now_ms);
                }
            }
            Delivery::V2(session) => match frame.enqueue_into(session, now_ms, &mut self.screen) {
                EnqueueOutcome::Queued | EnqueueOutcome::Dropped => {}
                EnqueueOutcome::Control(control) => self.send_control(&control, now_ms),
                EnqueueOutcome::Reset(notice) => {
                    tracing::info!(
                        event = "sync-ws",
                        action = "domain_reset",
                        socket_id = %self.socket_id,
                        domain = ?notice.domain,
                        generation = notice.generation,
                        reason = notice.reason,
                        "a Sync domain overflowed and was reset"
                    );
                    self.send_control(&notice.to_frame(), now_ms);
                }
                EnqueueOutcome::Fault(close) => {
                    self.close_for_session(close, frame_kind(frame.frame()), now_ms);
                }
            },
        }
    }

    /// Send one frame on the control lane: unsequenced, unqueued, and never
    /// charged to the window (`sync-ws-v2-control.ts`). A v1 socket has no
    /// control lane, so a control there is an ordinary guarded send.
    pub fn send_control(&mut self, frame: &FirehoseFrame, now_ms: u64) {
        if self.close.is_some() {
            return;
        }
        if let Delivery::V1(v1) = &mut self.delivery {
            if let Err((reason, kind)) = v1.send_guarded(frame.clone(), now_ms, &mut self.outbox) {
                self.close_for_backpressure(reason, kind, now_ms);
            }
            return;
        }
        let stamped = control_frame(frame.clone());
        let kind = frame_kind(&stamped);
        match stamped.try_encode_to_vec() {
            Ok(encoded) => {
                if !self.outbox.push(encoded) {
                    self.close_for_backpressure(BackpressureReason::HighWater, kind, now_ms);
                }
            }
            // A control that cannot be encoded is an answer the client will
            // never get, which v2 treats as an ambiguous delivery.
            Err(error) => {
                tracing::warn!(event = "sync-ws", action = "control_send_failed", frame = kind, error = %error);
                self.decide_close(LinkClose::BACKPRESSURE, "frame_dropped", kind, now_ms);
            }
        }
    }

    /// The keepalive every long-lived socket sends on its cadence.
    pub fn send_keepalive(&mut self, now_ms: u64) {
        self.send_control(&keepalive_frame(now_ms), now_ms);
    }

    /// One flush turn: up to [`FLUSH_BATCH_FRAMES`] application frames from the
    /// session's queues into the outbox. Returns whether more are eligible,
    /// in which case the caller writes this batch and runs another turn
    /// rather than waiting (`sync-ws-v2-egress.ts:355-359`).
    ///
    /// `take_flush_request` is the trigger: an admission, an ACK and a
    /// `domain_ready` all raise it, and a turn that finds it down sends
    /// nothing, because nothing has changed that could make a frame eligible.
    pub fn flush_turn(&mut self, now_ms: u64) -> bool {
        let Delivery::V2(session) = &mut self.delivery else {
            return false;
        };
        if self.close.is_some() || !session.take_flush_request() {
            return false;
        }
        let mut sent = 0;
        while sent < FLUSH_BATCH_FRAMES {
            let sendable = match session.take_next_sendable(now_ms, &mut self.screen) {
                FlushStep::Send(sendable) => sendable,
                FlushStep::Idle | FlushStep::Stalled => break,
            };
            sent += 1;
            if let Some(chunk) = &sendable.chunk_transfer {
                tracing::debug!(
                    event = "cell.chunk_fanout",
                    session_id = %chunk.session_id,
                    snapshot_id = %chunk.snapshot_id,
                    chunk_index = chunk.chunk_index,
                    chunk_count = chunk.chunk_count,
                    coord_transfer_ms = chunk.transfer_ms,
                );
            }
            // The turn already dequeued this frame and advanced its lane, so a
            // frame that cannot go out retires the socket: there is no
            // position left to retry it from.
            let accepted = match sendable.frame.try_encode_to_vec() {
                Ok(encoded) => self.outbox.push(encoded),
                Err(_) => false,
            };
            if !accepted {
                self.decide_close(LinkClose::BACKPRESSURE, "high_water", sendable.kind, now_ms);
                return false;
            }
        }
        if let Some(close) = session.take_close_fault() {
            self.close_for_session(close, "flush", now_ms);
            return false;
        }
        let ack_seq = session.acknowledged_sequence();
        if sent == FLUSH_BATCH_FRAMES && session.has_sendable_work(now_ms, ack_seq) {
            session.request_flush();
            return true;
        }
        false
    }

    /// How long until the oldest unacknowledged frame reaches the ACK
    /// deadline, or `None` with nothing in flight.
    #[must_use]
    pub fn ack_deadline_in_ms(&self, now_ms: u64) -> Option<u64> {
        let stats = self.window_stats(now_ms);
        (self.close.is_none() && stats.unacked_frames > 0)
            .then(|| ACK_TIMEOUT_MS.saturating_sub(stats.oldest_age_ms))
    }

    /// Close `age_limit` when the oldest unacknowledged frame waited out the
    /// ACK deadline (`sync-ws-v1-delivery.ts:119-143`).
    pub fn enforce_ack_deadline(&mut self, now_ms: u64) {
        if self.ack_deadline_in_ms(now_ms) == Some(0) {
            self.decide_close(LinkClose::BACKPRESSURE, "age_limit", "age_deadline", now_ms);
        }
    }

    fn window_stats(&self, now_ms: u64) -> WindowStats {
        match &self.delivery {
            Delivery::V1(v1) => v1.window_stats(now_ms),
            Delivery::V2(session) => session.window_stats(now_ms),
        }
    }

    fn close_for_backpressure(&mut self, reason: BackpressureReason, frame: &str, now_ms: u64) {
        self.decide_close(LinkClose::BACKPRESSURE, &reason.to_string(), frame, now_ms);
    }
}

/// The link one socket's listeners and task share.
#[derive(Debug)]
pub struct SyncLink {
    state: Mutex<LinkState>,
    wake: Notify,
}

impl SyncLink {
    /// A link over freshly built state.
    #[must_use]
    pub fn new(state: LinkState) -> Self {
        Self {
            state: Mutex::new(state),
            wake: Notify::new(),
        }
    }

    /// The shared state. Never held across an await, and never held while
    /// publishing to a bus: this socket's own listeners take the same lock.
    pub fn lock(&self) -> MutexGuard<'_, LinkState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Route one bus message under the lock, deliver the frame it produced,
    /// and wake the socket task.
    pub fn deliver_with(&self, route: impl FnOnce(&mut LinkState) -> Option<FeedFrame>) {
        {
            let mut state = self.lock();
            if state.close.is_some() {
                return;
            }
            if let Some(frame) = route(&mut state) {
                state.deliver(frame, now_ms());
            }
        }
        self.wake.notify_one();
    }

    /// Resolve when a listener has handed this socket work. A wake that
    /// arrives while the task is writing is kept, not lost.
    pub async fn woken(&self) {
        self.wake.notified().await;
    }
}

/// The wall clock as the unsigned milliseconds the session counts in.
#[must_use]
pub fn now_ms() -> u64 {
    u64::try_from(crate::serve::now_ms()).unwrap_or(0)
}
