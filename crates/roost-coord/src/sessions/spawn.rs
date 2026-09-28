//! `SessionsSpawn`: reserve the session UUID, send the worker one spawn command,
//! and answer with the identity the worker opened. Called from the arm in
//! `rpc/service_impl.rs`. Ports `apps/coord/src/sessions/handler-session-spawn.ts`.
//!
//! THE WORKER REPLY IS SETTLED OFF THE REQUEST. v2 attaches the reply handler to
//! the pending RPC and lets the HTTP request await only the reservation; the
//! same split here is a spawned task, so a caller that disconnects mid-spawn
//! still leaves an answer an exact retry of the same UUID can join, instead of
//! a reservation that can only time out.
//!
//! A reply names `session_id` and `channel_id`; one that names another session
//! or no positive channel is `DataLoss`, a definite failure.

use std::sync::Arc;

use connectrpc::{ConnectError, ErrorCode, Response, ServiceResult};
use roost_proto::{SessionsSpawnRequest, SessionsSpawnResponse};
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::{SessionId, WorkerFp};
use serde_json::Value;

use crate::attachments::relay::settle_within;
use crate::auth::principal::require_account_device;
use crate::coord_core::ids::{draw, render_v4};
use crate::coord_core::{Caller, CoordCore};
use crate::sessions::pending_spawns::{
    PendingSpawnResult, PendingSpawnSignature, PendingSpawns, SpawnReservation,
};
use crate::sessions::rpc_sessions::{lease, read_failed, viewer_key};
use crate::terminal_screen::pending_rpcs::PendingRpc;
use crate::workers::send::{SendOutcome, send_browser_command};

/// How long the worker has to answer the spawn command itself. Past it the
/// outcome is ambiguous and the reservation keeps waiting for `opened`.
const WORKER_SPAWN_REPLY_TIMEOUT_MS: u64 = 15_000;

/// Spawn a session on a worker, or join the in-flight spawn of the same UUID.
pub async fn handle_sessions_spawn(
    core: &CoordCore,
    caller: &Caller,
    req: SessionsSpawnRequest,
) -> ServiceResult<SessionsSpawnResponse> {
    let browser_fp = require_account_device(caller)?;
    let _lease = lease(core)?;
    let caller_key = viewer_key(browser_fp, caller.tab_id.as_deref());
    let session_id = spawn_session_id(req.session_id.as_deref())?;
    let worker_fp = registered_worker(core, &req.worker_fp).await?;
    let signature = PendingSpawnSignature {
        caller_key: caller_key.clone(),
        worker_fp: worker_fp.as_str().to_owned(),
        kind: req.kind.clone(),
        folder: req.folder.clone(),
        cols: req.cols,
        rows: req.rows,
    };
    let spawns = core.services.sessions.pending_spawns();
    let waiter = match spawns.reserve(session_id.as_str(), signature) {
        SpawnReservation::Conflict => {
            return Err(ConnectError::new(
                ErrorCode::AlreadyExists,
                "session_id is already pending with different caller or parameters",
            ));
        }
        SpawnReservation::Capacity => {
            tracing::warn!(session_id = %session_id, "sessions: the pending spawn table is full");
            return Err(ConnectError::new(
                ErrorCode::ResourceExhausted,
                "too many pending session spawns",
            ));
        }
        SpawnReservation::Joined(waiter) => {
            tracing::info!(session_id = %session_id, "sessions: a spawn retry joined the pending spawn");
            waiter
        }
        SpawnReservation::New(waiter) => {
            let dispatch = SpawnDispatch {
                core,
                spawns,
                browser_fp,
                caller_key: &caller_key,
                worker_fp: &worker_fp,
                session_id: &session_id,
            };
            if let Err(error) = dispatch.send(&req).await {
                spawns.reject(session_id.as_str(), error, true);
            }
            waiter
        }
    };
    let result = waiter.outcome().await?;
    Response::ok(SessionsSpawnResponse {
        session_id: result.session_id,
        channel_id: result.channel_id,
        ..Default::default()
    })
}

/// The first reservation's one worker command.
struct SpawnDispatch<'a> {
    core: &'a CoordCore,
    spawns: &'a Arc<PendingSpawns>,
    browser_fp: &'a str,
    caller_key: &'a str,
    worker_fp: &'a WorkerFp,
    session_id: &'a SessionId,
}

impl SpawnDispatch<'_> {
    /// Send the command, or say which definite failure rejects the reservation.
    async fn send(&self, req: &SessionsSpawnRequest) -> Result<(), ConnectError> {
        let services = &self.core.services;
        let existing: Option<String> = sqlx::query_scalar("SELECT id FROM sessions WHERE id = ?1")
            .bind(self.session_id.as_str())
            .fetch_optional(services.db.pool())
            .await
            .map_err(|error| read_failed("spawn", &error))?;
        if existing.is_some() {
            return Err(ConnectError::new(
                ErrorCode::AlreadyExists,
                "session_id already exists",
            ));
        }
        if services.workers.current_routable(self.worker_fp).is_none() {
            let short: String = self.worker_fp.as_str().chars().take(12).collect();
            return Err(ConnectError::new(
                ErrorCode::FailedPrecondition,
                format!("worker {short} not connected"),
            ));
        }
        let frame = spawn_frame_for(req, self.session_id.clone())?;
        let relay = &services.scrollback;
        let pending = relay
            .pending()
            .create_fresh(Some(self.worker_fp.as_str()), relay.now_ms())?;
        let outcome = send_browser_command(
            &services.workers,
            self.worker_fp,
            self.browser_fp,
            self.caller_key,
            pending.request_id(),
            frame,
        );
        if let SendOutcome::Refused(refusal) = outcome {
            tracing::warn!(session_id = %self.session_id, %refusal, "sessions: the spawn command was not sent");
            return Err(ConnectError::new(
                ErrorCode::Unavailable,
                "worker send failed",
            ));
        }
        tracing::info!(
            session_id = %self.session_id,
            worker_fp = %self.worker_fp,
            "sessions: spawn requested"
        );
        tokio::spawn(settle_spawn_reply(
            Arc::clone(self.spawns),
            self.session_id.as_str().to_owned(),
            pending,
        ));
        Ok(())
    }
}

/// Settle the reservation from the worker's reply. Only a worker-reported
/// command failure (`Internal`) is definite; a lost link or a missed deadline
/// leaves the reservation for the durable `opened` to reconcile.
async fn settle_spawn_reply(
    spawns: Arc<PendingSpawns>,
    session_id: String,
    mut pending: PendingRpc,
) {
    match settle_within(&mut pending, WORKER_SPAWN_REPLY_TIMEOUT_MS).await {
        Ok(reply) => match spawn_identity(&reply, &session_id) {
            Some(channel_id) => {
                spawns.resolve(
                    &session_id,
                    PendingSpawnResult {
                        session_id: session_id.clone(),
                        channel_id,
                    },
                );
            }
            None => {
                tracing::warn!(
                    session_id,
                    "sessions: the worker returned an invalid spawn identity"
                );
                spawns.reject(
                    &session_id,
                    ConnectError::new(
                        ErrorCode::DataLoss,
                        "worker returned an invalid spawn identity",
                    ),
                    true,
                );
            }
        },
        Err(error) => {
            let definite = error.code == ErrorCode::Internal;
            tracing::info!(session_id, error = %error, definite, "sessions: the spawn command failed");
            spawns.reject(&session_id, error, definite);
        }
    }
}

/// The channel a worker's reply opened, when it names the reserved session and
/// a positive integer channel.
fn spawn_identity(reply: &Value, session_id: &str) -> Option<u32> {
    if reply.get("session_id").and_then(Value::as_str) != Some(session_id) {
        return None;
    }
    let channel = reply.get("channel_id")?;
    let whole = channel.as_u64().or_else(|| {
        channel
            .as_f64()
            .filter(|value| value.fract() == 0.0 && *value >= 0.0)
            .map(|value| value as u64)
    })?;
    u32::try_from(whole)
        .ok()
        .filter(|channel_id| *channel_id > 0)
}

/// The caller-minted UUID, checked, or a fresh one.
fn spawn_session_id(requested: Option<&str>) -> Result<SessionId, ConnectError> {
    match requested.filter(|id| !id.is_empty()) {
        Some(requested) => SessionId::try_from(requested).map_err(|_| {
            ConnectError::new(
                ErrorCode::InvalidArgument,
                "session_id must be a canonical UUID",
            )
        }),
        None => draw::<16>()
            .map_err(|error| error.to_string())
            .and_then(|bytes| {
                SessionId::try_from(render_v4(bytes)).map_err(|error| error.to_string())
            })
            .map_err(|reason| {
                tracing::error!(reason, "sessions: no session id could be minted");
                ConnectError::new(ErrorCode::Internal, "no session id could be minted")
            }),
    }
}

/// The worker a spawn names, when it is registered and not tombstoned.
async fn registered_worker(core: &CoordCore, worker_fp: &str) -> Result<WorkerFp, ConnectError> {
    let found: Option<String> =
        sqlx::query_scalar("SELECT fp FROM workers WHERE fp = ?1 AND deleted_at_ms IS NULL")
            .bind(worker_fp)
            .fetch_optional(core.services.db.pool())
            .await
            .map_err(|error| read_failed("spawn worker lookup", &error))?;
    found
        .and_then(|fp| WorkerFp::try_from(fp).ok())
        .ok_or_else(|| ConnectError::new(ErrorCode::NotFound, "worker not found"))
}

/// The worker command for a session kind. Spawn geometry is only the initial
/// PTY-size hint; live geometry begins when a mounted terminal view joins.
fn spawn_frame_for(
    req: &SessionsSpawnRequest,
    session_id: SessionId,
) -> Result<ClientControlFrame, ConnectError> {
    match req.kind.as_str() {
        "shell" => Ok(ClientControlFrame::SpawnShell {
            folder: req.folder.clone(),
            cols: req.cols.map(i64::from),
            rows: req.rows.map(i64::from),
            session_id: Some(session_id),
            trace_id: None,
        }),
        kind => Err(ConnectError::new(
            ErrorCode::InvalidArgument,
            format!("unknown session kind {kind}"),
        )),
    }
}
