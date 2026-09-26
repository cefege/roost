//! The authoritative state the link's snapshot stage publishes, and the honest
//! absence of one. Called by the link loop exactly once per dial, at the
//! barrier's `snapshot` stage.
//!
//! The snapshot is what a reconnecting worker cannot describe incrementally: it
//! is the coordinator's proof that it knows every session this worker holds.
//! A worker that skips it has a link the coordinator cannot trust, which is why
//! `link_barrier::Barrier::allows_live_traffic` is `Live` and nothing else.
//!
//! So a worker with no session layer has no snapshot, and the honest thing is
//! to say so rather than publish an empty one. An empty snapshot is not a
//! smaller lie, it is a different one: it tells the coordinator this worker
//! holds nothing, and it would then close every session the keeper still has.

/// Why no snapshot could be published.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotError {
    #[error("this worker has no snapshot to publish: {reason}")]
    Unavailable { reason: String },
    #[error("the snapshot could not be encoded: {reason}")]
    Unencodable { reason: String },
}

/// Publishes the worker's complete session set, encoded as the frame the
/// coordinator acknowledges.
pub trait SnapshotSource: Send + Sync {
    /// Whether a snapshot can be produced at all.
    ///
    /// Separate from asking for one so the link loop can say once, at boot,
    /// that the barrier will never be released — rather than discovering it
    /// once per dial from an error nobody can act on.
    fn is_active(&self) -> bool;
    /// The encoded snapshot frame.
    fn snapshot(&self) -> Result<Vec<u8>, SnapshotError>;
}

/// The source the service installs, which has no session layer to describe.
///
/// TODO(roost-phase2): implement this over the session manager, producing what
/// `apps/worker/src/snapshot.ts` produces today. It must be the worker's
/// COMPLETE open-session set, read after boot reconciliation has reserved every
/// durable session, and not a filtered view: a snapshot that omits a session is
/// the coordinator closing one that is still running.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSnapshot;

impl SnapshotSource for NoSnapshot {
    fn is_active(&self) -> bool {
        false
    }

    fn snapshot(&self) -> Result<Vec<u8>, SnapshotError> {
        Err(SnapshotError::Unavailable {
            reason: "the worker has no session manager, so it has no session set to publish"
                .to_string(),
        })
    }
}
