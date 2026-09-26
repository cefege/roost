//! The Sync firehose adapters: one bus message in, one `FirehoseFrame` and the
//! lane metadata that orders it out. Nothing else lives here -- the seed and the
//! socket shell are other modules' work.
//!
//! `FeedFrame` is the type the whole directory exists to produce. It is a frame
//! AND the `SyncFrameMeta` that describes where the frame belongs, held
//! together, and its only constructor is private to this module. Contract
//! §12.8 is the reason: a queued frame that lost its meta is invisible to the
//! egress path that is supposed to advance its cursor, so a snapshot stops
//! after part one and nothing says why. Building the pair in one value makes
//! that unrepresentable rather than merely discouraged.
//!
//! ONE FIELD ON `CoordServices`, reached as `core.services.feed`. `new()` takes
//! nothing and must keep taking nothing: the retention bounds and the clock are
//! read at call time or supplied by the caller.
//!
//! Ported from `apps/coord/src/sync/sync-feed-frames.ts` (the typed bus
//! adapters it exports), `sync-feed-ui.ts`, `sync/presence-hub.ts` and
//! `sync/last-activity-hub.ts`. The subscription ENGINE that installs these on
//! the live buses, seeds a fresh subscriber and narrows each frame to the
//! sockets that may see it belongs to the socket shell
//! (`sync_ws::socket` / `sync_ws::driver`), not here: nothing in this directory
//! subscribes to a bus except the one lifecycle subscription `last_activity`
//! owns, because that one is the hub's own retained state and not a fan-out.

pub mod frames;
pub mod last_activity;
pub mod presence;
pub mod ui;
pub mod worker_frames;

use roost_proto::FirehoseFrame;
use roost_protocol::ProtocolError;

use crate::sync_ws::admission::EnqueueOutcome;
use crate::sync_ws::frame_meta::{SyncFrameMeta, frame_meta_for};
use crate::sync_ws::session::SyncV2Session;
use crate::sync_ws::terminal::snapshot::TerminalSnapshotHub;

pub use last_activity::LastActivityHub;

/// The firehose state one coordinator process holds.
#[derive(Debug, Default)]
pub struct FeedRuntime {
    last_activity: LastActivityHub,
}

impl FeedRuntime {
    /// A feed over the real clock, retaining no last-activity observation yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            last_activity: LastActivityHub::new(),
        }
    }

    /// The retained last-activity observations, owned by this coordinator and
    /// seeded to every fresh Sync subscriber by the socket shell.
    #[must_use]
    pub fn last_activity(&self) -> &LastActivityHub {
        &self.last_activity
    }
}

/// One bus message as one Sync frame, with the metadata the per-socket
/// scheduler orders it by.
///
/// The two fields are private and the constructor is module-private, so the only
/// way to obtain one is through an adapter in this directory -- and every
/// adapter goes through [`FeedFrame::of`], which classifies the frame with the
/// same [`frame_meta_for`] the queue would use. An adapter therefore cannot
/// declare a lane the queue would disagree with, and cannot hand a caller a
/// frame whose routing was thrown away.
#[derive(Debug, Clone, PartialEq)]
pub struct FeedFrame {
    frame: FirehoseFrame,
    meta: SyncFrameMeta,
}

impl FeedFrame {
    /// Classify `frame` and bind it to that classification.
    ///
    /// The classification is read back off the finished frame rather than
    /// declared by the adapter, which is v2's `push(frame, meta = frameMeta(
    /// frame))` default (`sync-feed.ts:85`) made explicit: a session frame's
    /// `announces`/`closes` are facts about the encoded event, so a feed that
    /// computed them from the wire value would have two sources for one answer.
    pub(in crate::sync_ws::feed) fn of(frame: FirehoseFrame) -> Self {
        let meta = frame
            .frame
            .as_ref()
            .map_or_else(SyncFrameMeta::control, frame_meta_for);
        Self { frame, meta }
    }

    /// The same frame under routing the caller supplies.
    ///
    /// Only the RETAINED SEED path uses this, and only to say a sample must
    /// precede the live segment that buffered while the client was hydrating.
    /// A live bus message never overrides what [`Self::of`] read off the frame.
    #[must_use]
    pub fn with_meta(mut self, meta: SyncFrameMeta) -> Self {
        self.meta = meta;
        self
    }

    /// The frame, for encoding and for the close reason a caller logs.
    #[must_use]
    pub fn frame(&self) -> &FirehoseFrame {
        &self.frame
    }

    /// The metadata the queue places, fences and ages this frame by.
    #[must_use]
    pub fn meta(&self) -> &SyncFrameMeta {
        &self.meta
    }

    /// The one hand-off from the feed into a socket's queues.
    ///
    /// THE META GOES WITH THE FRAME OR NEITHER GOES. Passing the metadata as a
    /// separate argument to `enqueue_frame` is what let §12.8 happen; this
    /// method has no signature that can express it, and it is the only way a
    /// `FeedFrame` becomes a queued frame.
    pub fn enqueue_into(
        &self,
        session: &mut SyncV2Session,
        now_ms: u64,
        hub: &mut dyn TerminalSnapshotHub,
    ) -> EnqueueOutcome {
        session.enqueue_frame(&self.frame, Some(&self.meta), now_ms, hub)
    }
}

/// Why a bus message could not become a frame.
#[derive(Debug, Clone, thiserror::Error)]
pub enum FeedRefusal {
    /// The event is durable and recoverable by the worker that owns it, and
    /// must never enter a browser lane. v2 threw at frame construction; here it
    /// is a value, so a fan-out over a batch of events drops the private one and
    /// keeps the rest.
    #[error("private session event {kind} cannot enter a browser frame")]
    PrivateSessionEvent {
        /// The durable discriminator that was refused.
        kind: &'static str,
    },
    /// The value did not survive the wire boundary.
    #[error("feed frame: {0}")]
    Unencodable(#[from] ProtocolError),
}

/// Every bus in `events::bus_domains::Buses` and the adapter that turns one of
/// its messages into a frame: `(bus field, adapter path)`.
///
/// This is the audit list, and it is a `const` rather than prose because a bus
/// added without an adapter is a message published into the void, which is
/// indistinguishable from a coordinator with no browsers connected.
/// `tests/sync_feed_bus_coverage.rs` drives every row of it.
pub const BUS_FRAME_ADAPTERS: &[(&str, &str)] = &[
    ("session_bus", "sync_ws::feed::frames::session_message_frame"),
    ("workspace_bus", "sync_ws::feed::frames::workspace_frame"),
    ("task_bus", "sync_ws::feed::frames::task_frame"),
    ("mcp_bus", "sync_ws::feed::frames::mcp_frame"),
    (
        "agent_status_bus",
        "sync_ws::feed::frames::agent_status_frame",
    ),
    ("pair_bus", "sync_ws::feed::frames::pair_frame"),
    ("audit_bus", "sync_ws::feed::frames::audit_frame"),
    ("title_bus", "sync_ws::feed::frames::session_title_frame"),
    (
        "presence_bus",
        "sync_ws::feed::worker_frames::worker_presence_frame",
    ),
    (
        "worker_routable_bus",
        "sync_ws::feed::worker_frames::worker_routable_frame",
    ),
    (
        "global_presence_bus",
        "sync_ws::feed::presence::session_presence_frame",
    ),
    (
        "last_activity_bus",
        "sync_ws::feed::last_activity::last_activity_frame",
    ),
    ("ui_bus", "sync_ws::feed::ui::ui_bus_frame"),
];

/// A stored millisecond stamp as the `uint64` the wire carries.
///
/// Saturating, and for the same reason `workers::projection` saturates: a
/// negative stamp is a corrupt value, `0` is the honest reading, and a wrapped
/// `u64` would date a row to the year 584 billion. Spelled once here because
/// every adapter below this module has the same cast to make.
pub(in crate::sync_ws::feed) fn as_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// A stored count as the `uint32` the wire carries.
pub(in crate::sync_ws::feed) fn as_u32(value: i64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// An epoch-millisecond stamp as the `double` the wire carries.
///
/// Exact well past any real timestamp, which is what a client that renders an
/// age needs, and `i64::MAX as f64` is a value no reader can act on anyway.
pub(in crate::sync_ws::feed) fn as_f64(value: i64) -> f64 {
    value as f64
}
