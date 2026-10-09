//! A fresh shell session: the record, the PTY, and the two durable claims that
//! bracket them. `session::lifecycle` calls it for a spawn and for a respawn;
//! `keeper_pool` implements the seam it opens the PTY through. Depends on
//! `roost_term` for the core, `event_store` for the claims and `shell_spec` for
//! the launch contract — and on nothing that calls back into it.
//!
//! THE ORDER IS THE CONTRACT. Both claims are taken by the caller BEFORE this
//! runs, because a session that is open has not written its `closed` event yet
//! and must be able to. The record is registered before the keeper is asked
//! for a PTY, so bytes the shell prints immediately after its `SpawnAck` have
//! terminal state to receive them; and on ANY failure after the keeper was
//! touched, the PTY is killed, both claims are released and no record is
//! returned. A leaked claim is how a store eventually refuses every write, and
//! an orphan PTY is how a keeper fills up with terminals nobody can see.
//! Ports v2 `apps/worker/src/session/session-spawn.ts`, `apps/worker/src/util/path.ts`.

use std::sync::Arc;

use roost_protocol::viewport::{TerminalGeometry, is_terminal_geometry};
use roost_protocol::wire::brand::{ChannelId, SessionId, TraceId, WorkerFp};
use roost_protocol::wire::event::SessionEvent;
use roost_protocol::wire::session::SessionKind;
use roost_term::{CellEmitState, RioCore, TerminalCore};

use crate::channel_fsm::ChannelEvent;
use crate::event_store::{DurableEventKind, Reservation};
use crate::session::ids::{MintError, mint_trace_id, mint_uuid};
use crate::session::ring::ScrollbackRing;
use crate::session::session_cwd::canonical_session_cwd;
use crate::session::sinks::{ChannelBinding, SessionEventError, SessionEventSink};
use crate::session::types::{SessionIdentity, SessionRecord};
use crate::shell_spec::ShellSpec;
use crate::terminal_core_capacity::{
    TerminalCoreAllocationKind, TerminalCoreCapacity, TerminalCoreCapacityError,
};

/// The geometry a spawn with none stated gets.
pub const DEFAULT_SPAWN_COLS: u16 = 80;
pub const DEFAULT_SPAWN_ROWS: u16 = 24;

/// Everything one spawn needs, and nothing it may decide for itself.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    /// Allocated by the caller's `ChannelAllocator`, never here: one counter in
    /// the worker, and it lives with the stray reaper that has to advance it
    /// past the keeper's own maximum.
    pub channel_id: ChannelId,
    /// The folder the record reports as its `cwd`, and the event's `cwd`.
    pub folder: String,
    /// Zero means the default, not zero: a PTY cannot be opened at no size.
    pub cols: u16,
    pub rows: u16,
    /// `Some` keeps this logical session id across the spawn. A respawn passes
    /// the old one so the sidebar row stays in place.
    pub session_id: Option<SessionId>,
    /// `Some` opens the PTY with THIS spec verbatim — no canonicalisation, no
    /// directory I/O, no re-resolution. A respawn of a session this worker still
    /// holds re-opens at the spec it was launched with, and re-resolving a
    /// folder that has since been deleted would fail a session that was working
    /// a second ago.
    pub shell_spec: Option<ShellSpec>,
    /// Which durable event announces this PTY. `Opened` for a spawn, `State`
    /// for a respawn — a respawn that announced an `opened` would tell every
    /// browser watching that row to paint a start moment it never had.
    pub event: DurableEventKind,
    /// Which core lease this spawn takes: `Fresh`, or `Replacement` for a
    /// respawn of a session this worker still holds (v2 `session-respawn.ts:78`).
    pub core_allocation: TerminalCoreAllocationKind,
    /// Who gives the two claims back when the spawn fails.
    pub claims: ClaimsOnFailure,
}

/// Who owns the claims of a spawn that failed (v2 `releaseReservationsOnFailure`):
/// the spawn itself, or a caller that releases them after its own retries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimsOnFailure {
    Release,
    Keep,
}

/// The keeper seam: open a PTY from a resolved launch contract.
pub trait ShellSpawner: Send + Sync {
    /// Open the channel and report the child pid.
    ///
    /// The binding arrives HERE rather than being registered afterwards: a shell
    /// prints its first prompt inside the spawn round trip, and a channel with
    /// no delivery target for that first chunk is a chunk the session can never
    /// account for.
    fn spawn_channel(
        &self,
        channel_id: ChannelId,
        spec: &ShellSpec,
        cols: u16,
        rows: u16,
        binding: Arc<dyn ChannelBinding>,
    ) -> Result<u32, String>;

    /// End the channel. Called on the failure path when a PTY was opened and the
    /// spawn did not complete, because an orphan PTY nobody can reach is the
    /// one thing worse than a failed spawn.
    fn kill_channel(&self, channel_id: ChannelId);
}

/// The resolver that PRODUCES a launch contract from a folder.
///
/// Declared here, implemented by `crate::host`: `shell_spec.rs` says so itself,
/// because resolution materialises directories, reads `SHELL` and writes a
/// bootstrap rcfile, and a record must be able to hold a spec without pulling
/// any of that in. The implementation must apply the agent-status environment
/// overlay and strip every keeper control credential from the PTY's
/// environment.
pub trait ShellSpecResolver: Send + Sync {
    fn resolve_shell_spec(&self, cwd: &str, session_id: &str) -> Result<ShellSpec, String>;
}

/// The per-session variables every launch contract carries — the agent
/// report endpoint and its capability, derived from the session id. An `Err`
/// refuses the spawn: a PTY must not carry an endpoint nobody serves.
pub trait SessionEnvironmentOverlay: Send + Sync {
    fn session_overlay(&self, session_id: &str) -> Result<Vec<(String, String)>, String>;
}

/// The collaborators one spawn borrows for its duration.
pub struct SpawnContext<'a> {
    pub spawner: &'a dyn ShellSpawner,
    pub resolver: &'a dyn ShellSpecResolver,
    pub events: &'a dyn SessionEventSink,
    pub worker_fp: &'a WorkerFp,
    /// The worker's one terminal-core admission; a spawn's core is leased here.
    pub core_capacity: &'a TerminalCoreCapacity,
}

impl std::fmt::Debug for SpawnContext<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The three collaborators are trait objects, so what a log line can say
        // about them is their type. The fingerprint is the one field that names
        // a machine, and a spawn refusal is read against it.
        formatter
            .debug_struct("SpawnContext")
            .field("spawner", &"dyn ShellSpawner")
            .field("resolver", &"dyn ShellSpecResolver")
            .field("events", &"dyn SessionEventSink")
            .field("worker_fp", &self.worker_fp)
            .finish()
    }
}

/// Why a spawn did not happen. Every variant is a condition the caller can act
/// on; none of them leaves a claim taken or a PTY open.
#[derive(Debug, thiserror::Error)]
pub enum SpawnRefusal {
    #[error("spawn geometry must be within 1..=256 on both axes, got {cols}x{rows}")]
    Geometry { cols: u16, rows: u16 },
    #[error("this spawn announces {kind:?}, which is not a spawn or a respawn")]
    UnnameableEvent { kind: DurableEventKind },
    #[error("a session identity could not be minted: {0}")]
    Identity(#[from] MintError),
    #[error("the launch contract for {cwd} could not be resolved: {reason}")]
    Unresolvable { cwd: String, reason: String },
    #[error("a minted session identity is not a uuid: {0}")]
    MintedId(String),
    #[error("a minted trace id is not the shape trace ids validate: {0}")]
    MintedTraceId(String),
    #[error("the keeper refused to open channel {channel_id}: {reason}")]
    KeeperRefused {
        channel_id: ChannelId,
        reason: String,
    },
    #[error("the terminal core did not retain the validated spawn geometry")]
    CoreGeometryDrift,
    #[error("the durable boundary refused the spawn: {0}")]
    Event(#[from] SessionEventError),
    #[error("the terminal core was refused: {0}")]
    TerminalCoreCapacity(#[from] TerminalCoreCapacityError),
}

pub async fn spawn_shell(
    context: &SpawnContext<'_>,
    opened_reservation: Reservation,
    close_reservation: Reservation,
    binding: Arc<dyn ChannelBinding>,
    request: SpawnRequest,
    now_ms: i64,
) -> Result<SessionRecord, SpawnRefusal> {
    let claims = request.claims;
    let spawned = spawn_claimed(
        context,
        opened_reservation,
        close_reservation,
        binding,
        request,
        now_ms,
    )
    .await;
    if spawned.is_err() && claims == ClaimsOnFailure::Release {
        release_both(context.events, opened_reservation, close_reservation).await;
    }
    spawned
}

async fn spawn_claimed(
    context: &SpawnContext<'_>,
    opened_reservation: Reservation,
    close_reservation: Reservation,
    binding: Arc<dyn ChannelBinding>,
    request: SpawnRequest,
    now_ms: i64,
) -> Result<SessionRecord, SpawnRefusal> {
    let cols = if request.cols == 0 {
        DEFAULT_SPAWN_COLS
    } else {
        request.cols
    };
    let rows = if request.rows == 0 {
        DEFAULT_SPAWN_ROWS
    } else {
        request.rows
    };
    if !is_terminal_geometry(&TerminalGeometry {
        cols: u32::from(cols),
        rows: u32::from(rows),
    }) {
        return Err(SpawnRefusal::Geometry { cols, rows });
    }
    if !matches!(
        request.event,
        DurableEventKind::Opened | DurableEventKind::State
    ) {
        return Err(SpawnRefusal::UnnameableEvent {
            kind: request.event,
        });
    }
    let session_id = match request.session_id.clone() {
        Some(session_id) => session_id,
        None => SessionId::try_from(mint_uuid()?)
            .map_err(|error| SpawnRefusal::MintedId(error.to_string()))?,
    };
    let cwd = canonical_session_cwd(&request.folder, None);
    let spec = match request.shell_spec.clone() {
        Some(spec) => spec,
        None => context
            .resolver
            .resolve_shell_spec(&cwd, session_id.as_str())
            .map_err(|reason| SpawnRefusal::Unresolvable {
                cwd: cwd.clone(),
                reason,
            })?,
    };
    // After the launch contract, before the core: v2 `allocateTerminalCore`.
    let lease = match context.core_capacity.reserve(request.core_allocation) {
        Ok(lease) => lease,
        Err(refusal) => {
            return Err(SpawnRefusal::TerminalCoreCapacity(refusal));
        }
    };
    let core = RioCore::new(cols, rows);
    if core.cols() != cols || core.rows() != rows {
        return Err(SpawnRefusal::CoreGeometryDrift);
    }
    let channel_id = request.channel_id;
    let mut record = SessionRecord::new(
        SessionIdentity {
            session_id,
            channel_id,
            // The keeper channel's address. Retained so a diagnostic can name
            // the endpoint a channel belongs to without re-deriving it.
            socket_path: format!("mux:{channel_id}"),
            cwd,
            shell_spec: spec.clone(),
            session_trace_id: TraceId::try_from(mint_trace_id()?)
                .map_err(|error| SpawnRefusal::MintedTraceId(error.to_string()))?,
            spawned_at_ms: now_ms,
        },
        close_reservation,
        Box::new(core) as Box<dyn TerminalCore + Send>,
        CellEmitState::new(mint_uuid()?, mint_uuid()?),
        ScrollbackRing::default(),
    );
    let raw_channel = channel_id.as_u32() as u16;
    // The record now holds the core: the lease is resident from here until the
    // record's teardown (`SessionManager::close_channel`) or a failure below.
    if let Err(misuse) = context.core_capacity.install_channel(raw_channel, lease) {
        tracing::error!(%channel_id, error = %misuse, "a spawned core's lease could not become resident");
    }
    let opened = spawn_event(&record, context.worker_fp, request.event, now_ms);
    // The record exists before the PTY does: the shell's first prompt can arrive
    // inside the spawn round trip, and a chunk with no record to receive it is
    // a chunk this session can never account for.
    let child_pid = match context
        .spawner
        .spawn_channel(channel_id, &spec, cols, rows, binding)
    {
        Ok(child_pid) => child_pid,
        Err(reason) => {
            context.core_capacity.release_channel(raw_channel);
            return Err(SpawnRefusal::KeeperRefused { channel_id, reason });
        }
    };
    record.child_pid = Some(child_pid);
    if let Err(error) = context.events.emit(&opened, Some(opened_reservation)).await {
        context.spawner.kill_channel(channel_id);
        context.core_capacity.release_channel(raw_channel);
        return Err(SpawnRefusal::Event(error));
    }
    // The future close's claim is now committed: it no longer blocks a snapshot,
    // and nobody else may take its capacity.
    context.events.hold(close_reservation).await;
    if let Err(refusal) = record.fsm.send(ChannelEvent::Attach) {
        tracing::warn!(%channel_id, reason = %refusal.reason(), "a spawned channel refused its attach");
    }
    tracing::info!(
        session_id = %record.identity.session_id,
        %channel_id,
        child_pid,
        cwd = %record.identity.cwd,
        cols,
        rows,
        event = ?request.event,
        "a shell session was spawned and its stream is attached"
    );
    Ok(record)
}

/// The durable event that announces this PTY.
fn spawn_event(
    record: &SessionRecord,
    worker_fp: &WorkerFp,
    kind: DurableEventKind,
    now_ms: i64,
) -> SessionEvent {
    let trace_id = Some(record.identity.session_trace_id.clone());
    let session_id = record.identity.session_id.clone();
    let channel = record.identity.channel_id;
    match kind {
        DurableEventKind::Opened => SessionEvent::Opened {
            session_id,
            worker_fp: worker_fp.clone(),
            channel,
            session_kind: SessionKind::Shell,
            cwd: record.identity.cwd.clone(),
            ts: now_ms,
            trace_id,
        },
        _ => SessionEvent::Respawned {
            session_id,
            new_channel: channel,
            ts: now_ms,
            trace_id,
        },
    }
}

/// Give both claims back. A spawn that failed after reserving must not leave the
/// store's capacity spent on events that will never be written.
async fn release_both(events: &dyn SessionEventSink, opened: Reservation, close: Reservation) {
    events.release(opened).await;
    events.release(close).await;
}
