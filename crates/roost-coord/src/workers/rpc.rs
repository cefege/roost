//! The five worker RPCs, each a `handle_*` the service impl delegates to.
//!
//! Ported from `apps/coord/src/workers/handlers-workers.ts` and
//! `apps/coord/src/workers/handlers-workers-heartbeat.ts`. The methods are not
//! applied to `rpc::service_impl.rs` here: that file holds the ONE `impl
//! CoordinatorService` block, so it is written once by the integrator.
//!
//! EVERY MUTATION LEASES FROM THE ONE WRITE GATE. `WorkersHeartbeat` reads like
//! a read and is a durable write, which is why `write_gate::method_holds_lease`
//! names it (`write_gate.rs:189-195`); the lease is taken here rather than in
//! the interceptor because a handler is also the only place a test can hold the
//! gate and watch a mutation be refused.
//!
//! A WORKER MAY NOT CALL THE OPERATOR METHODS. `list`, `rename` and `delete` act
//! for an account device; `register` and `heartbeat` are the worker speaking
//! about itself. The auth gate already refuses the wrong principal, and the
//! check is repeated here because a handler that trusts its caller to have been
//! checked is one refactor away from being an authorization bypass.

use std::collections::BTreeSet;
use std::sync::PoisonError;

use connectrpc::{ConnectError, ErrorCode, Response, ServiceResult};
use roost_proto::buffa::MessageField;
use roost_protocol::wire::{SessionId, WorkerFp, WorkerPresenceEvent};

use super::claims::{bounded_worker_text, heartbeat_claim, register_claims};
use super::delete::{WorkerDeleteError, best_effort_cleanup, delete_worker, released_sessions};
use super::heartbeat::{HeartbeatOutcome, MalformedClaim, apply_worker_heartbeat};
use super::projection::{worker_row_to_proto, worker_row_to_wire_presence};
use super::register::{WorkerWriteError, apply_worker_registration, apply_worker_rename};
use super::registry::{fence_worker_credential, list_routable_fps, publish_routable};
use super::rows::{StoredWorkerRow, read_live_worker, read_live_workers, read_worker_tombstone};
use crate::coord_core::{Caller, CoordCore};
use crate::db::CoordDb;
use crate::write_gate::SharedLease;

/// The Connect method each handler answers, and the function that answers it.
///
/// The integrator's list: every row is one arm of the single `impl
/// CoordinatorService` block in `rpc/service_impl.rs`.
pub const METHOD_HANDLERS: [(&str, &str); 5] = [
    ("WorkersList", "workers::rpc::handle_workers_list"),
    ("WorkersRegister", "workers::rpc::handle_workers_register"),
    ("WorkersHeartbeat", "workers::rpc::handle_workers_heartbeat"),
    ("WorkersRename", "workers::rpc::handle_workers_rename"),
    ("WorkersDelete", "workers::rpc::handle_workers_delete"),
];

/// The fleet view: every worker that is not tombstoned, and which of them the
/// coordinator can route to right now.
///
/// The two lists answer different questions and the SPA needs both: the rows say
/// what exists, `routable_fps` says what is usable. A heartbeat-fresh worker
/// whose socket dropped is in the first and not the second, and the online
/// indicator gates on the second.
pub async fn handle_workers_list(
    core: &CoordCore,
    caller: &Caller,
    _request: roost_proto::WorkersListRequest,
) -> ServiceResult<roost_proto::WorkersListResponse> {
    account_device(caller)?;
    let rows = read_live_workers(&core.services.db)
        .await
        .map_err(internal)?;
    let live: BTreeSet<&str> = rows.iter().map(|row| row.fp.as_str()).collect();
    let routable: Vec<String> = list_routable_fps(&core.services.workers)
        .into_iter()
        .filter(|worker_fp| live.contains(worker_fp.as_str()))
        .map(|worker_fp| worker_fp.to_string())
        .collect();
    let workers = rows
        .iter()
        .map(worker_row_to_proto)
        .collect::<Result<Vec<_>, _>>()
        .map_err(internal)?;
    Response::ok(roost_proto::WorkersListResponse {
        workers,
        routable_fps: routable,
        ..Default::default()
    })
}

/// A worker states what it is, and its row is rewritten from that statement.
pub async fn handle_workers_register(
    core: &CoordCore,
    caller: &Caller,
    request: roost_proto::WorkersRegisterRequest,
) -> ServiceResult<roost_proto::WorkersRegisterResponse> {
    let fingerprint = worker_caller(caller)?;
    let claims = register_claims(&request)?;
    let now_ms = now_ms();
    let _lease = lease(core)?;
    let database = &core.services.db;
    let Some(prior) = read_live_worker(database, fingerprint)
        .await
        .map_err(internal)?
    else {
        return Err(not_enrolled(database, fingerprint).await);
    };
    let updated = apply_worker_registration(database, &prior, &claims, now_ms)
        .await
        .map_err(internal)?;
    publish_registered(core, &updated)?;
    tracing::info!(worker_fp = %fingerprint, label = %updated.label, "a worker registered");
    Response::ok(roost_proto::WorkersRegisterResponse {
        worker: MessageField::some(worker_row_to_proto(&updated).map_err(internal)?),
        ..Default::default()
    })
}

/// One liveness beat, and the presence frame it publishes.
pub async fn handle_workers_heartbeat(
    core: &CoordCore,
    caller: &Caller,
    request: roost_proto::WorkersHeartbeatRequest,
) -> ServiceResult<roost_proto::WorkersHeartbeatResponse> {
    let fingerprint = worker_caller(caller)?;
    let claim = heartbeat_claim(&request)?;
    let now_ms = now_ms();
    let _lease = lease(core)?;
    let database = &core.services.db;
    let Some(prior) = read_live_worker(database, fingerprint)
        .await
        .map_err(internal)?
    else {
        return Err(not_enrolled(database, fingerprint).await);
    };
    let outcome = apply_worker_heartbeat(database, &prior, &claim, now_ms)
        .await
        .map_err(internal)?;
    match outcome {
        HeartbeatOutcome::Committed { updated, changed } => {
            let presence = if changed.any() {
                registered_presence(&updated)?
            } else {
                WorkerPresenceEvent::Heartbeat {
                    fp: worker_fp_of(fingerprint)?,
                    last_seen_ms: now_ms,
                    host_metrics: claim.host_metrics.clone(),
                    terminal_core_capacity: claim.terminal_core_capacity.clone(),
                }
            };
            core.services.buses.presence_bus.publish(presence);
            tracing::debug!(worker_fp = %fingerprint, last_seen_ms = now_ms, "worker heartbeat");
            Response::ok(roost_proto::WorkersHeartbeatResponse::default())
        }
        HeartbeatOutcome::Refused { cleared, malformed } => {
            // A malformed keeper proof clears a field the fleet view renders, so
            // it rides the full record; a malformed capacity report does not, so
            // it rides the light beat. v2 makes the same split
            // (`handlers-workers-heartbeat.ts:113-126`).
            let presence = if malformed == MalformedClaim::KeeperRuntime {
                registered_presence(&cleared)?
            } else {
                WorkerPresenceEvent::Heartbeat {
                    fp: worker_fp_of(fingerprint)?,
                    last_seen_ms: now_ms,
                    host_metrics: None,
                    terminal_core_capacity: None,
                }
            };
            core.services.buses.presence_bus.publish(presence);
            Err(ConnectError::new(
                ErrorCode::InvalidArgument,
                malformed.reason(),
            ))
        }
        HeartbeatOutcome::Tombstoned => Err(not_enrolled(database, fingerprint).await),
    }
}

/// An operator renames a machine.
pub async fn handle_workers_rename(
    core: &CoordCore,
    caller: &Caller,
    request: roost_proto::WorkersRenameRequest,
) -> ServiceResult<roost_proto::WorkersRenameResponse> {
    account_device(caller)?;
    let _lease = lease(core)?;
    let label = bounded_worker_text(&request.label);
    let updated = apply_worker_rename(&core.services.db, &request.fp, &label)
        .await
        .map_err(|error| match error {
            WorkerWriteError::Retired { .. } => {
                ConnectError::new(ErrorCode::NotFound, "worker not found")
            }
            other => internal(other),
        })?;
    publish_registered(core, &updated)?;
    tracing::info!(worker_fp = %request.fp, label = %updated.label, "a worker was renamed");
    Response::ok(roost_proto::WorkersRenameResponse {
        worker: MessageField::some(worker_row_to_proto(&updated).map_err(internal)?),
        ..Default::default()
    })
}

/// An operator removes a machine, and everything that could authenticate as it.
pub async fn handle_workers_delete(
    core: &CoordCore,
    caller: &Caller,
    request: roost_proto::WorkersDeleteRequest,
) -> ServiceResult<roost_proto::WorkersDeleteResponse> {
    let revoked_by = account_device(caller)?;
    let now_ms = now_ms();
    let fingerprint = WorkerFp::try_from(request.fp.as_str()).map_err(|_| {
        ConnectError::new(ErrorCode::InvalidArgument, "malformed worker fingerprint")
    })?;
    let _lease = lease(core)?;
    let deletion = delete_worker(&core.services.db, &fingerprint, revoked_by, now_ms)
        .await
        .map_err(|error| match error {
            WorkerDeleteError::NotFound => {
                ConnectError::new(ErrorCode::NotFound, "worker not found")
            }
            other => internal(other),
        })?;
    fence_deleted_worker(core, &fingerprint);
    release_worker_state(core, &fingerprint, &deletion.persisted_session_ids);
    Response::ok(roost_proto::WorkersDeleteResponse {
        ok: true,
        ..Default::default()
    })
}

/// Stop the deleted worker's generation before anything else can yield.
///
/// Synchronous and outside every best-effort block: the commit is irrevocable,
/// so a browser command that reached this worker one line later is a command a
/// removed machine executed. The publication slots go with it, because a
/// committed event waiting on a generation that will never publish again is a row
/// no replay can release.
fn fence_deleted_worker(core: &CoordCore, fingerprint: &WorkerFp) {
    fence_worker_credential(&core.services.workers, fingerprint);
    let mut store = core
        .services
        .pending_publications
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    store.clear_worker(fingerprint.as_str());
    tracing::info!(%fingerprint, "a deleted worker is fenced");
}

/// The volatile state a delete has to clear, one isolated step at a time.
///
/// The order is v2's (`handlers-workers.ts:247-272`) and each step has a
/// different owner: the key cache is the auth domain's, the routes are the byte
/// hub's, the views are the view hub's, the two buses are this domain's. The
/// route retirement and the view notification are deliberately TWO steps -- they
/// are separate objects with separate lifetimes, and folding them into one would
/// make the view hub's failure the byte hub's.
fn release_worker_state(core: &CoordCore, fingerprint: &WorkerFp, persisted: &[String]) {
    best_effort_cleanup("jwt_cache", fingerprint, || {
        core.services.jwt_keys.forget_worker(fingerprint.as_str());
    });
    let mut released: Vec<SessionId> = Vec::new();
    best_effort_cleanup("routes", fingerprint, || {
        released = released_sessions(
            persisted,
            &core.terminal.routes.retire_worker_routes(fingerprint),
        );
    });
    best_effort_cleanup("terminal_views", fingerprint, || {
        core.terminal
            .views
            .notify_worker_retired(fingerprint, &released);
    });
    best_effort_cleanup("routable_presence", fingerprint, || {
        publish_routable(&core.services.buses, &core.services.workers);
    });
    best_effort_cleanup("worker_presence", fingerprint, || {
        core.services
            .buses
            .presence_bus
            .publish(WorkerPresenceEvent::Removed {
                fp: fingerprint.clone(),
            });
    });
}

/// A shared write lease, or the refusal v2's gate throws.
fn lease(core: &CoordCore) -> Result<SharedLease, ConnectError> {
    core.services
        .write_gate()
        .acquire_shared()
        .map_err(|error| ConnectError::new(ErrorCode::Unavailable, error.to_string()))
}

/// The authenticated worker fingerprint, or a refusal.
fn worker_caller(caller: &Caller) -> Result<&str, ConnectError> {
    caller
        .principal
        .require_worker()
        .map_err(|error| ConnectError::new(ErrorCode::Unauthenticated, error.to_string()))
}

/// The acting account device's fingerprint, or a refusal.
fn account_device(caller: &Caller) -> Result<&str, ConnectError> {
    caller
        .principal
        .require_account_device()
        .map_err(|error| ConnectError::new(ErrorCode::Unauthenticated, error.to_string()))
}

/// The refusal for a fingerprint with no live row, naming a tombstone when
/// there is one.
///
/// v2 answers "worker not registered; redeem bootstrap token first" for both
/// cases, which sends an operator whose machine was deleted looking for a token
/// problem. The tombstone is the fact that explains the refusal, and it is the
/// only thing on the row that says an operator did this on purpose.
async fn not_enrolled(database: &CoordDb, fingerprint: &str) -> ConnectError {
    let tombstone = read_worker_tombstone(database, fingerprint)
        .await
        .ok()
        .flatten();
    match tombstone {
        Some(tombstone) => ConnectError::new(
            ErrorCode::Unauthenticated,
            format!(
                "worker {} was deleted at {}; redeem a new bootstrap token to re-enrol it",
                tombstone.fp, tombstone.deleted_at_ms
            ),
        ),
        None => ConnectError::new(
            ErrorCode::Unauthenticated,
            "worker not registered; redeem bootstrap token first",
        ),
    }
}

/// Publish the full worker record, which is the only frame that replaces a
/// browser's copy of the row.
fn publish_registered(core: &CoordCore, row: &StoredWorkerRow) -> Result<(), ConnectError> {
    core.services
        .buses
        .presence_bus
        .publish(registered_presence(row)?);
    Ok(())
}

/// The full presence frame for a row.
fn registered_presence(row: &StoredWorkerRow) -> Result<WorkerPresenceEvent, ConnectError> {
    worker_row_to_wire_presence(row)
        .map(|worker| WorkerPresenceEvent::Registered { worker })
        .map_err(internal)
}

/// The branded fingerprint of an authenticated worker.
fn worker_fp_of(fingerprint: &str) -> Result<WorkerFp, ConnectError> {
    WorkerFp::try_from(fingerprint)
        .map_err(|error| ConnectError::new(ErrorCode::Internal, error.to_string()))
}

/// A durable failure, as a Connect internal error.
fn internal(error: impl std::fmt::Display) -> ConnectError {
    ConnectError::new(ErrorCode::Internal, error.to_string())
}

/// The coordinator's clock, in epoch milliseconds.
fn now_ms() -> i64 {
    crate::rpc::service::now_ms()
}
