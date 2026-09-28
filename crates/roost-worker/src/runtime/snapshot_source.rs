//! The authoritative state the link's snapshot stage publishes, and the refusal
//! a worker gives when it cannot produce one. Called by the link loop exactly
//! once per dial, at the barrier's `snapshot` stage.
//!
//! The snapshot is what a reconnecting worker cannot describe incrementally: it
//! is the coordinator's proof that it knows every session this worker holds.
//! A worker that skips it has a link the coordinator cannot trust, which is why
//! `link_barrier::Barrier::allows_live_traffic` is `Live` and nothing else.
//!
//! WHAT A SNAPSHOT IS NOT IS A SUMMARY, and the difference is the whole reason
//! this refuses rather than publishing an empty set. An empty snapshot is not a
//! smaller lie, it is a different one: it tells the coordinator this worker holds
//! nothing, and the coordinator then closes every session the keeper still has.
//! So [`SnapshotSource::is_active`] exists as a separate question from
//! [`SnapshotSource::snapshot`]: the link asks once, at boot, whether the barrier
//! will ever be released, rather than discovering the answer once per dial from
//! an error nobody can act on.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

use roost_protocol::wire::brand::WorkerFp;
use roost_protocol::wire::event::SessionEvent;
use roost_protocol::wire::session::{Session, SessionKind, SessionStatus};

use crate::session::lifecycle::SessionTable;
use crate::session::types::SessionRecord;

/// Why no snapshot could be published.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotError {
    #[error("this worker has no snapshot to publish: {reason}")]
    Unavailable { reason: String },
}

/// Publishes the worker's complete session set. The link frames it as v2's
/// `Event{snapshot, client_seq}` under a sequence the durable outbox draws
/// (`runtime::link_loop::durable_sync`).
pub trait SnapshotSource: Send + Sync {
    /// Whether a snapshot can be produced at all.
    ///
    /// Separate from asking for one so the link loop can say once, at boot,
    /// that the barrier will never be released — rather than discovering it
    /// once per dial from an error nobody can act on.
    fn is_active(&self) -> bool;
    /// The snapshot event, unframed.
    fn snapshot(&self) -> Result<SessionEvent, SnapshotError>;
}

/// v2 `activateSnapshotProvider`: boot holds the link's snapshot until its first
/// reconcile pass has produced the complete local session set, while the link
/// already dials and replays the durable outbox (v2 `main.ts:296-303`). Made by
/// `LinkLoop::hold_snapshot_until_activated`, released by
/// `runtime::boot_admission`, read by the link's snapshot stage. Every clone is
/// the same hold.
#[derive(Debug, Clone)]
pub struct SnapshotActivation {
    active: Arc<AtomicBool>,
    wake: Arc<Notify>,
}

impl SnapshotActivation {
    /// A hold not yet released; `wake` is what [`SnapshotActivation::activate`]
    /// rings.
    pub fn held(wake: Arc<Notify>) -> Self {
        Self {
            active: Arc::new(AtomicBool::new(false)),
            wake,
        }
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    /// Release the hold and wake the link, so a barrier already waiting at the
    /// snapshot stage publishes now (v2 pumps when its phase is `snapshot`).
    pub fn activate(&self) {
        if !self.active.swap(true, Ordering::AcqRel) {
            tracing::info!("the snapshot provider is active: the link may publish the worker snapshot");
            self.wake.notify_one();
        }
    }
}

/// The worker's own session set, as the snapshot barrier publishes it.
///
/// Ported from `apps/worker/src/snapshot.ts`, which builds the frame from ONE
/// membership copy of the session manager rather than reading the table once
/// per field: a snapshot assembled from several reads describes a set that never
/// existed, and the coordinator closes every session the difference contains.
pub struct SessionSnapshot {
    worker_fp: WorkerFp,
    sessions: Arc<SessionTable>,
    now_ms: i64,
}

impl std::fmt::Debug for SessionSnapshot {
    /// The session COUNT and not the sessions: this type is reached from the
    /// link loop's log line, and printing every record's bytes would put a
    /// machine's whole scrollback into a log.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionSnapshot")
            .field("worker_fp", &self.worker_fp)
            .field("live_sessions", &self.sessions.live().len())
            .field("ts", &self.now_ms)
            .finish()
    }
}

impl SessionSnapshot {
    /// The snapshot this worker publishes, stamped once per activation.
    ///
    /// `now_ms` is taken here rather than per call because the barrier asks on
    /// every dial, and a timestamp that moved between two dials of the same
    /// process is a fact about the clock rather than about the session set.
    pub fn new(worker_fp: WorkerFp, sessions: Arc<SessionTable>, now_ms: i64) -> Self {
        Self {
            worker_fp,
            sessions,
            now_ms,
        }
    }

    /// One membership copy, and the frame built from it.
    fn build(&self) -> Result<SessionEvent, SnapshotError> {
        let rows = self
            .sessions
            .live()
            .into_iter()
            .map(|(_session_id, channel_id)| {
                self.sessions
                    .with_channel_record(channel_id, |record| row(&self.worker_fp, record))
            })
            .collect::<Option<Vec<Session>>>();
        // A session that left the table between the two reads is NOT a failure:
        // it is a session that closed, and a closed session belongs in nobody's
        // snapshot. A member the table cannot produce a record for, though,
        // would be a row the coordinator never learns about, so that refuses.
        let sessions = rows.ok_or_else(|| SnapshotError::Unavailable {
            reason: "a session left the table between the membership copy and the record read"
                .to_string(),
        })?;
        Ok(SessionEvent::Snapshot {
            worker_fp: self.worker_fp.clone(),
            sessions,
            ts: self.now_ms,
            trace_id: None,
        })
    }
}

impl SnapshotSource for SessionSnapshot {
    /// Always true: this source exists only once a session table does, and a
    /// table that exists can be described even when it is empty. An empty set
    /// is a claim, and this worker is entitled to make it.
    fn is_active(&self) -> bool {
        true
    }

    fn snapshot(&self) -> Result<SessionEvent, SnapshotError> {
        self.build()
    }
}

/// One record as the coordinator's row.
///
/// The three-state fields (`git_branch`, `git_remote`, `pr`) map straight
/// across, and they are kept distinct rather than flattened: a client renders
/// "not a repository" and "not looked yet" differently, and a snapshot that
/// collapses them into `null` makes the first look like the second after every
/// reconnect.
fn row(worker_fp: &WorkerFp, record: &SessionRecord) -> Session {
    let pr = record.pr.as_ref().and_then(|pr| pr.as_ref());
    Session {
        id: record.session_id().clone(),
        worker_fp: worker_fp.clone(),
        channel: record.channel_id(),
        kind: SessionKind::Shell,
        cwd: record.identity.cwd.clone(),
        spawn_cwd: Some(record.identity.shell_spec.cwd.clone()),
        workspace_id: None,
        status: SessionStatus::Open,
        created_at: record.identity.spawned_at_ms,
        closed_at: None,
        // The worker does not track a rename, so the field is left absent and
        // the coordinator's snapshot fold preserves the prior value. Writing
        // `null` here would clear a user's title on every reconnect.
        custom_title: None,
        git_branch: record.git_branch.clone().flatten(),
        git_remote: record.git_remote.clone(),
        pr_number: pr.map(|pr| i64::from(pr.number)),
        pr_state: pr.map(|pr| pr.state),
        pr_checks: pr.map(|pr| pr.checks),
        pr_url: pr.map(|pr| pr.url.clone()),
        ports: record
            .ports
            .as_ref()
            .map(|ports| ports.iter().map(|port| i64::from(*port)).collect()),
    }
}
