//! Respawning a logical session onto a fresh keeper PTY without replacing the
//! session: a new channel under the SAME session id, announced by `respawned`,
//! and — when this worker still held a record for it — the old record retired
//! and its PTY killed only once that event is durable. Ports
//! `apps/worker/src/session/session-respawn.ts` and
//! `session-respawn-admission.ts`. Called by `runtime::session_reconcile`
//! (reservations owned by the reconcile) and by the browser's
//! `respawn-if-missing` (`lifecycle_commands`).

use std::sync::Arc;

use roost_protocol::viewport::{TerminalGeometry, is_terminal_geometry};
use roost_protocol::wire::brand::SessionId;

use super::binding::RecordBinding;
use super::lifecycle::SessionManager;
use super::sinks::ChannelBinding;
use super::spawn::{self, ClaimsOnFailure, SpawnContext, SpawnRefusal, SpawnRequest};
use crate::browser_commands::Refusal;
use crate::browser_commands::session_lifecycle::SessionOutcome;
use crate::event_store::{DurableEventKind, Reservation};
use crate::shell_spec::ShellSpec;
use crate::terminal_core_capacity::{TerminalCoreAllocationKind, TerminalCoreCapacityError};

/// What a respawn is asked to open.
#[derive(Debug, Clone)]
pub struct RespawnRequest {
    pub session_id: SessionId,
    pub cwd: String,
    /// Used when this worker holds no record for the session; a held record's
    /// own launch contract always wins (v2 `existing?.shellSpec ?? opts.shellSpec`).
    pub shell_spec: Option<ShellSpec>,
    pub cols: Option<u16>,
    pub rows: Option<u16>,
}

/// The channel a respawn opened, and the folder its record reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Respawned {
    pub channel_id: u16,
    pub cwd: String,
}

/// Why a respawn failed. The first two are the classes the reconcile stops on
/// (v2 `isSessionEventDurabilityError`, `isTerminalCoreCapacityError`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RespawnError {
    #[error("{0}")]
    Durability(String),
    #[error("{0}")]
    Capacity(TerminalCoreCapacityError),
    #[error("{0}")]
    Failed(String),
}

impl SessionManager {
    /// The launch contract a session is opened under, through the manager's one
    /// resolver (v2 `resolveShellSpec({cwd, sessionId, envOverlay})`).
    pub fn resolve_shell_spec(&self, cwd: &str, session_id: &str) -> Result<ShellSpec, String> {
        self.resolver.resolve_shell_spec(cwd, session_id)
    }

    /// Claim durable capacity for one event, keeping the store's own verdict:
    /// a reconcile tells a full outbox from a store that refused outright.
    pub(crate) async fn reserve_event(&self, kind: DurableEventKind) -> Result<Reservation, super::sinks::SessionEventError> {
        self.events.reserve(kind).await
    }

    /// v2 `respawnIfMissing`: the held record when there is one, else a respawn
    /// under fresh claims that are given back if it fails.
    pub async fn respawn_lost_child(
        &self,
        session_id: &SessionId,
        cwd: &str,
        cols: u16,
        rows: u16,
    ) -> Result<SessionOutcome, Refusal> {
        let live = |manager: &Self| {
            manager.sessions.with_record(session_id, |record| SessionOutcome::Live {
                channel_id: record.channel_id().as_u32() as u16,
                already_live: record.identity.cwd == cwd,
            })
        };
        if let Some(held) = live(self) {
            return Ok(held);
        }
        let Some(_pending) = self.pending_respawns.begin(session_id) else {
            return Err(Refusal::failed("sessions", format!("session {session_id} is already live or spawning")));
        };
        let event = self.reserve(DurableEventKind::State).await?;
        let close = match self.reserve(DurableEventKind::Closed).await {
            Ok(close) => close,
            Err(refusal) => {
                self.events.release(event).await;
                return Err(refusal);
            }
        };
        let request = RespawnRequest {
            session_id: session_id.clone(),
            cwd: cwd.to_owned(),
            shell_spec: None,
            cols: Some(cols),
            rows: Some(rows),
        };
        self.respawn_session(request, event, close, ClaimsOnFailure::Release)
            .await
            .map_err(|error| Refusal::failed("sessions", error.to_string()))?;
        live(self).ok_or_else(|| Refusal::failed("sessions", format!("respawned session {session_id} is not live")))
    }

    /// v2 `respawn`: open a new channel for `request.session_id`, announce it
    /// with `respawned` under `event`, and only then retire a record this
    /// worker still held for the session (its claim released, its PTY killed).
    pub async fn respawn_session(
        &self,
        request: RespawnRequest,
        event: Reservation,
        close: Reservation,
        claims: ClaimsOnFailure,
    ) -> Result<Respawned, RespawnError> {
        let existing = self.sessions.with_record(&request.session_id, |record| {
            let geometry = (record.terminal_core.cols(), record.terminal_core.rows());
            (record.channel_id().as_u32() as u16, record.identity.shell_spec.clone(), geometry)
        });
        let cols = request.cols.or(existing.as_ref().map(|held| held.2.0)).unwrap_or(80);
        let rows = request.rows.or(existing.as_ref().map(|held| held.2.1)).unwrap_or(24);
        if !is_terminal_geometry(&TerminalGeometry { cols: u32::from(cols), rows: u32::from(rows) }) {
            if claims == ClaimsOnFailure::Release {
                self.events.release(event).await;
                self.events.release(close).await;
            }
            return Err(RespawnError::Failed("respawn geometry must be within 1..256 on both axes".to_owned()));
        }
        let channel_id = match self.take_channel_id() {
            Ok(channel_id) => channel_id,
            Err(refusal) => {
                if claims == ClaimsOnFailure::Release {
                    self.events.release(event).await;
                    self.events.release(close).await;
                }
                return Err(RespawnError::Failed(refusal.message()));
            }
        };
        let raw_channel = channel_id.as_u32() as u16;
        let core_allocation = match existing {
            Some(_) => TerminalCoreAllocationKind::Replacement,
            None => TerminalCoreAllocationKind::Fresh,
        };
        let binding = RecordBinding::closing(self, raw_channel);
        let spawn_request = SpawnRequest {
            channel_id,
            folder: request.cwd.clone(),
            cols,
            rows,
            session_id: Some(request.session_id.clone()),
            shell_spec: existing.as_ref().map(|held| held.1.clone()).or(request.shell_spec),
            event: DurableEventKind::State,
            core_allocation,
            claims,
        };
        let context = SpawnContext {
            spawner: self.spawner.as_ref(),
            resolver: self.resolver.as_ref(),
            events: self.events.as_ref(),
            worker_fp: &self.worker_fp,
            core_capacity: &self.core_capacity,
        };
        let now_ms = self.clock.now_epoch_ms();
        let record = spawn::spawn_shell(&context, event, close, Arc::clone(&binding) as Arc<dyn ChannelBinding>, spawn_request, now_ms)
            .await
            .map_err(classify)?;
        let cwd = record.identity.cwd.clone();
        let stream_id = record.cell_emit.stream_id.clone();
        let displaced = match self.sessions.insert_replacing(record, existing.as_ref().map(|held| held.0)) {
            Ok((_, displaced)) => displaced,
            Err(refusal) => {
                // The `respawned` is already durable; a record nobody can hold
                // must not keep a PTY running behind it.
                self.spawner.kill_channel(channel_id);
                self.core_capacity.release_channel(raw_channel);
                binding.abandon();
                return Err(RespawnError::Failed(refusal.message()));
            }
        };
        if let Some(old) = displaced {
            self.retire_replaced(old).await;
        }
        if core_allocation == TerminalCoreAllocationKind::Replacement
            && let Err(misuse) = self.core_capacity.complete_channel_replacement(raw_channel)
        {
            tracing::error!(channel_id = raw_channel, error = %misuse, "a respawn's replacement core slot could not be completed");
        }
        self.note_applied_resize_seq(raw_channel, 0);
        self.terminal_streams.note_applied_size(channel_id, cols, rows);
        let (_, held_exit) = binding.go_live();
        self.cells
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .install_stream(channel_id, &stream_id);
        if let Some(exit_code) = held_exit
            && let Err(refusal) = self.close_channel(raw_channel, Some(exit_code)).await
        {
            tracing::error!(channel_id = raw_channel, reason = %refusal.message(), "a respawned channel that exited at once could not be closed");
        }
        tracing::info!(session_id = %request.session_id, channel_id = raw_channel, %cwd, "session-manager: respawned");
        Ok(Respawned { channel_id: raw_channel, cwd })
    }

    /// The record a durable `respawned` just superseded: its close claim goes
    /// back (the session did not close), its delivery state is dropped, and
    /// its PTY is killed so no orphan outlives it (v2 `session-respawn.ts:169-175`).
    async fn retire_replaced(&self, old: Arc<std::sync::Mutex<super::types::SessionRecord>>) {
        let (channel_id, reservation) = {
            let record = old.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            (record.channel_id(), record.close_reservation)
        };
        let raw = channel_id.as_u32() as u16;
        self.events.release(reservation).await;
        self.cells
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .forget_channel(channel_id);
        self.terminal_streams.forget(channel_id);
        self.core_capacity.release_channel(raw);
        self.keeper_health.mark_recently_closed(raw, self.clock.now_epoch_ms());
        if let Err(fault) = self.keeper.kill_channel(raw) {
            tracing::error!(channel_id = raw, error = %fault, "a respawn's superseded PTY would not die");
        }
        tracing::info!(channel_id = raw, "a respawn retired the record it replaced and killed its PTY");
    }
}

fn classify(refusal: SpawnRefusal) -> RespawnError {
    match refusal {
        SpawnRefusal::Event(error) => RespawnError::Durability(error.to_string()),
        SpawnRefusal::TerminalCoreCapacity(error) => RespawnError::Capacity(error),
        other => RespawnError::Failed(other.to_string()),
    }
}

/// The session ids with a respawn in flight (v2 `pendingSpawnSessionIds`).
#[derive(Debug, Default)]
pub struct PendingRespawns {
    inner: std::sync::Mutex<std::collections::HashSet<SessionId>>,
}

/// One claimed session id; dropping it ends the claim.
#[derive(Debug)]
pub struct PendingRespawn<'set> {
    set: &'set PendingRespawns,
    session_id: SessionId,
}

impl PendingRespawns {
    fn begin(&self, session_id: &SessionId) -> Option<PendingRespawn<'_>> {
        let mut held = self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        held.insert(session_id.clone()).then(|| PendingRespawn { set: self, session_id: session_id.clone() })
    }
}

impl Drop for PendingRespawn<'_> {
    fn drop(&mut self) {
        let mut held = self.set.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        held.remove(&self.session_id);
    }
}

