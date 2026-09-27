//! The coordinator link's own state: who this worker is, the writer's mirror of
//! the durable queue, and the authorisation slot. Owned by `serve` through
//! [`LinkLoop::run`], and reached by [`super::link_serve`] and
//! [`super::link_drain`], which is why the fields are `pub(super)`.
//!
//! It composes delivered pieces and owns none of their rules: the ladder is
//! [`crate::backoff`], the ordering is [`crate::link_barrier::Pump`], the lanes
//! are [`crate::outbox::Outbox`]. What is here is what those three cannot
//! express between them, and it is split along that seam — `durable.rs` owns the
//! rows, `volatile.rs` the two superseding producers, `cell_sink.rs` the
//! coordinator's cell receiver, and `reconnect_loop.rs` the loop over dials.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use roost_protocol::wire::WorkerFp;

use crate::backoff::LinkHealth;
use crate::link_barrier::{Barrier, Pump};
use crate::event_store::Journal;
use crate::link_dial::CoordinatorEndpoint;
use crate::outbox::{AdmitError, Admitted, Lane, Outbox, PENDING_BYTES_CAP};

use super::credential::CredentialSource;
use super::link_wire::LinkWire;
use super::reconnect::{Escalation, ReconnectPolicy};
use super::snapshot_source::SnapshotSource;
pub mod browser;
pub mod cell_sink;
pub mod durable;
pub mod reconnect_loop;
pub mod volatile;

pub use browser::BrowserLink;
pub use cell_sink::CoordinatorCellSink;

/// How long one dial may take before it counts as a non-open dial.
///
/// The handshake is a network round trip. Ten seconds is far past any healthy
/// answer and far short of an operator deciding the coordinator is gone, and it
/// is the bound the keeper's own connection retry uses, so both halves of a boot
/// give up on the same schedule.
pub const DIAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How often the outbox is swept while a link is open.
///
/// One pass is a walk of the outbox plus at most one `send` per frame. This is
/// far below a terminal frame interval and far above the cost of an empty pass,
/// so it is invisible on the hot path, and it bounds a lost wakeup to one tick
/// rather than a stall.
pub const DRAIN_TICK: std::time::Duration = std::time::Duration::from_millis(25);

/// How many durable events may sit between the pump and the writer.
///
/// The pump holds exactly one in flight by contract; this is the ceiling on the
/// rest. Without it, the move out of the pump's queue — which the pump does not
/// bound — into the writer's copy would be unbounded.
pub const DURABLE_MIRROR_CAP: usize = 256;

/// How long the barrier may sit at the snapshot stage before the link is torn
/// down so the condition becomes visible.
///
/// Thirty seconds is longer than any healthy barrier takes, because the
/// snapshot is a local read of the worker's own session set, and short enough
/// that a wedged link is not mistaken for a working one for a whole backoff
/// window.
pub const SNAPSHOT_STARVATION: std::time::Duration = std::time::Duration::from_secs(30);

/// What a browser command is answered with while nothing can execute one.
pub const NO_SESSION_LAYER_REFUSAL: &str =
    "this worker build has no session layer, so it cannot execute browser commands";



/// Who this worker is to the coordinator, in the fields the hello carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerIdentity {
    /// The registry fingerprint this worker dials as.
    pub worker_fp: WorkerFp,
    /// The version reported in the hello.
    pub version: String,
    /// The activation identity, sent with the hello so a coordinator can tell
    /// a restarted worker from the one it was talking to.
    pub process_epoch: String,
}

/// A durable event the writer holds for the barrier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DurableWrite {
    pub(super) bytes: Vec<u8>,
    /// The pump sequence this event was released under, once it has been.
    pub(super) seq: Option<u64>,
}

/// A write the barrier has authorised and that is not yet on the wire.
#[derive(Debug)]
pub(super) enum Authorised {
    /// One durable event, at the head of the mirror.
    Durable(u64),
    /// The authoritative snapshot. The bytes are held here rather than requested
    /// at send time, because a snapshot request can fail and a failed request
    /// must not consume the authorisation.
    Snapshot(Vec<u8>),
}

/// The coordinator link, and everything it owns while it runs.
pub struct LinkLoop {
    pub(super) endpoint: CoordinatorEndpoint,
    pub(super) identity: WorkerIdentity,
    pub(super) wire: Arc<dyn LinkWire>,
    pub(super) snapshot: Arc<dyn SnapshotSource>,
    pub(super) credential: Arc<dyn CredentialSource>,
    /// Where a browser command goes, and where its answers come back.
    pub(super) browser: BrowserLink,
    pub(super) outbox: Outbox,
    pub(super) pump: Pump,
    pub(super) policy: ReconnectPolicy,
    /// The writer's copy of the durable queue, in the pump's order.
    pub(super) durable: VecDeque<DurableWrite>,
    pub(super) durable_bytes: usize,
    pub(super) authorised: Option<Authorised>,
    pub(super) wake: Arc<tokio::sync::Notify>,
    /// When the barrier entered `snapshot`, for the starvation bound.
    pub(super) snapshot_since: Option<Instant>,
    /// The durable outbox this link replays from. `None` until one is attached,
    /// and `durable.rs` owns everything that depends on which it is.
    pub(super) durable_rows: Option<Arc<Journal>>,
    /// Acknowledgements the frame handler recorded and the drain has not yet
    /// retired. `durable.rs` owns the rule that decides what happens to them.
    pub(super) pending_acks: Vec<u64>,
    /// The coordinator's cell receiver, which is `&self` and so cannot be the
    /// link's own lane. `cell_sink.rs` owns it and the link only drains it.
    pub(super) cell_sink: Option<Arc<CoordinatorCellSink>>,
    pub(super) links_opened: u64,
    pub(super) redisials: u64,
}

impl std::fmt::Debug for LinkLoop {
    /// The three collaborators are interfaces, and their debug output is not
    /// this file's to choose. Everything with a real diagnostic value is here.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LinkLoop")
            .field("endpoint", &self.endpoint)
            .field("identity", &self.identity)
            .field("barrier", &self.pump.barrier())
            .field("durable_mirror", &self.durable.len())
            .field("durable_bytes", &self.durable_bytes)
            .field("outbox_frames", &self.outbox.frame_count())
            .field("health", &self.policy.health())
            .field("links_opened", &self.links_opened)
            .field("redisials", &self.redisials)
            .finish()
    }
}

impl LinkLoop {
    pub fn new(
        endpoint: CoordinatorEndpoint,
        identity: WorkerIdentity,
        wire: Arc<dyn LinkWire>,
        snapshot: Arc<dyn SnapshotSource>,
        credential: Arc<dyn CredentialSource>,
        browser: BrowserLink,
    ) -> Self {
        Self {
            endpoint,
            identity,
            wire,
            snapshot,
            browser,
            credential,
            outbox: Outbox::default(),
            pump: Pump::new(),
            policy: ReconnectPolicy::new(),
            durable: VecDeque::new(),
            durable_bytes: 0,
            authorised: None,
            wake: Arc::new(tokio::sync::Notify::new()),
            snapshot_since: None,
            links_opened: 0,
            redisials: 0,
            durable_rows: None,
            pending_acks: Vec::new(),
            cell_sink: None,
        }
    }

    pub fn barrier(&self) -> Barrier {
        self.pump.barrier()
    }

    pub fn durable_pending(&self) -> usize {
        self.durable.len()
    }

    pub fn outbox_frames(&self) -> usize {
        self.outbox.frame_count()
    }

    pub fn health(&self) -> LinkHealth {
        self.policy.health()
    }

    pub fn links_opened(&self) -> u64 {
        self.links_opened
    }

    pub fn redisials(&self) -> u64 {
        self.redisials
    }

    /// Offer an already-encoded frame to a control, terminal or raw lane.
    ///
    /// The durable lane is refused rather than accepted. A durable event goes
    /// through [`LinkLoop::enqueue_durable`], which is the only path into the
    /// pump, and a second one would put a durable event on the wire beside the
    /// ones the pump is holding instead of after them.
    pub fn admit(
        &mut self,
        lane: Lane,
        bytes: Vec<u8>,
        label: impl Into<String>,
    ) -> Result<Admitted, AdmitRefusal> {
        if lane == Lane::Durable {
            return Err(AdmitRefusal::DurableHasItsOwnPath);
        }
        self.outbox
            .admit(lane, bytes, label, Instant::now())
            .map_err(AdmitRefusal::Outbox)
    }

    /// Drop everything waiting in a lane, oldest first. What a superseded stream
    /// generation calls for: its pending cells describe a grid that is gone.
    pub fn discard_pending(&mut self, lane: Lane) -> usize {
        self.outbox.discard(lane)
    }

    /// Offer a durable event.
    ///
    /// It goes to the pump, never to a lane: the pump is what makes "one in
    /// flight" and "the coordinator acknowledges this exact sequence" true.
    pub fn enqueue_durable(&mut self, bytes: Vec<u8>) -> Result<(), DurableRefusal> {
        if let Some(refusal) = self.mirror_refusal(bytes.len()) {
            return Err(refusal);
        }
        self.durable_bytes += bytes.len();
        // Copied once: the pump keeps its own copy and never hands it back, so
        // the writer needs a second one to send. A durable event is a session
        // record rather than a screen, so this is one copy of a few hundred
        // bytes on a path that runs at session-lifecycle frequency.
        self.durable.push_back(DurableWrite {
            bytes: bytes.clone(),
            seq: None,
        });
        let action = self.pump.enqueue_durable(bytes);
        super::link_drain::apply_to(self, action);
        Ok(())
    }

    pub(super) fn mirror_refusal(&self, incoming: usize) -> Option<DurableRefusal> {
        if self.durable.len() >= DURABLE_MIRROR_CAP {
            return Some(DurableRefusal::TooManyPending {
                pending: self.durable.len(),
                cap: DURABLE_MIRROR_CAP,
            });
        }
        if self.durable_bytes + incoming > PENDING_BYTES_CAP {
            return Some(DurableRefusal::TooManyBytes {
                bytes: self.durable_bytes,
                cap: PENDING_BYTES_CAP,
            });
        }
        None
    }

    /// Tell the loop that a producer has something for it.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

}

/// Why a frame was not offered to the outbox.
#[derive(Debug, thiserror::Error)]
pub enum AdmitRefusal {
    #[error("a durable event goes through the pump, not through the outbox's durable lane")]
    DurableHasItsOwnPath,
    #[error(transparent)]
    Outbox(#[from] AdmitError),

    #[error("an unidentified agent status names no session or occupant, so no reader could place it")]
    UnidentifiedAgentStatus,
    #[error("a {label} frame did not encode: {reason}")]
    Unencodable { label: String, reason: String },
}

/// Why a durable event was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DurableRefusal {
    #[error("the durable mirror holds {pending} events against a cap of {cap}")]
    TooManyPending { pending: usize, cap: usize },
    #[error("the durable mirror holds {bytes} bytes against a cap of {cap}")]
    TooManyBytes { bytes: usize, cap: usize },
}

pub(super) fn report_escalation(escalation: Escalation) {
    tracing::warn!(
        streak = escalation.streak,
        cap_ms = u64::try_from(escalation.cap.as_millis()).unwrap_or(u64::MAX),
        has_opened = escalation.has_opened,
        "the coordinator backoff escalated: this worker is probably carrying something the \
         coordinator refuses"
    );
}
