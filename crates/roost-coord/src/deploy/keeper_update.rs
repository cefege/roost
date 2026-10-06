//! The coordinator's half of a keeper update: the exclusive write drain, the
//! fail-closed admission decision, and the frame the worker must converge on.
//! Answers `WorkersPrepareKeeperUpdate`; the deploy domain reaches it as
//! `core.services.deploy`. Depends on the write gate, the maintenance sender
//! (`workers::maintenance_send`), and the shared `roost_protocol::keeper_update` contract.
//!
//! WHY THE COORDINATOR DECIDES AT ALL. The keeper is the process every live PTY
//! depends on and only the worker can observe it on the host, so this RPC holds
//! the drain, re-reads the caller's credential inside it, and refuses unless
//! the journaled envelope and the worker's own proof agree about what may
//! happen to that keeper. "I do not know" is a refusal: proceeding on an
//! unprovable runtime is the failure this gate exists to prevent, and
//! `keeper_survivor_identity_unproven` is what it looks like from the host.
//! Ported whole from `apps/coord/src/deploy/handlers-workers-update.ts`.

mod identity;
mod refusal;
mod request;

use std::time::Duration;

use connectrpc::{Response, ServiceResult};
use roost_protocol::wire::WorkerFp;

use crate::auth::authorized_keys::resolve_key_principal;
use crate::coord_core::{Caller, CoordCore};
use crate::workers::maintenance_send::{KeeperUpdatePreparation, send_keeper_update_preparation};

// The module directory is private, so these are the names the service arm and
// this crate's own tests reach the decision through. Each `pub use` is also
// the local binding, which is why there is no second private `use` of the
// same names.
pub use self::identity::{KeeperIdentity, verify_worker_result, worker_outcome};
pub use self::refusal::KeeperUpdateRefusal;
pub use self::request::{
    KeeperUpdateAction, KeeperUpdateAdmission, KeeperUpdateRequest, MAINTENANCE_ACTION,
    decide_keeper_update, read_open_session_ids,
};

/// v2's `sendKeeperUpdatePreparation(workerFp, message, 10_000)`, and shorter
/// than the shared default on purpose: an update that cannot be decided
/// promptly must free the drain rather than hold this coordinator's writes
/// behind it.
const KEEPER_UPDATE_RPC_DEADLINE: Duration = Duration::from_millis(10_000);

/// The envelope as the worker link carries it, field for field.
impl KeeperUpdateAdmission {
    /// The maintenance command this admission sends.
    #[must_use]
    pub fn preparation(&self) -> KeeperUpdatePreparation {
        KeeperUpdatePreparation {
            journaled_update_json: self.journaled_update_json.clone(),
            direction: self.direction.clone(),
            maintenance: self.maintenance,
            force_live: self.force_live,
            coordinator_open_session_ids: self.open_session_ids.clone(),
        }
    }
}

/// Whether the caller's key still says it is a browser.
///
/// Re-read inside the drain, never taken from the interceptor: a key or device
/// revoked between that resolve and here must not proceed on the stale
/// admission. v2 needed this because its fence queues behind another update;
/// this gate fails fast, so the window is shorter, but it is the same window.
async fn reauthorize_device(
    core: &CoordCore,
    device_fingerprint: &str,
) -> Result<(), KeeperUpdateRefusal> {
    let current = resolve_key_principal(&core.services.db, device_fingerprint)
        .await
        .map_err(|error| {
            tracing::error!(device_fingerprint, %error, "the keeper caller could not be re-read");
            KeeperUpdateRefusal::CoordinatorReadFailed
        })?;
    match current {
        Some(principal) if principal.is_browser() => Ok(()),
        _ => Err(KeeperUpdateRefusal::AuthenticationRequired),
    }
}

/// Whether the machine has a live registry row rather than a tombstone.
async fn require_live_worker(
    core: &CoordCore,
    worker_fp: &WorkerFp,
) -> Result<(), KeeperUpdateRefusal> {
    let live: Option<String> =
        sqlx::query_scalar("SELECT fp FROM workers WHERE fp = $1 AND deleted_at_ms IS NULL")
            .bind(worker_fp.as_str())
            .fetch_optional(core.services.db.pool())
            .await
            .map_err(|error| {
                tracing::error!(%worker_fp, %error, "the worker row could not be read");
                KeeperUpdateRefusal::CoordinatorReadFailed
            })?;
    live.map(|_| ()).ok_or(KeeperUpdateRefusal::WorkerNotFound)
}

/// Prepare a keeper update: may this release take over the live PTYs.
///
/// The whole answer is one question, asked in one order. Auth, then the
/// envelope, then the drain, then the machine, then the decision, then the
/// worker -- so a request that was never admissible never blocks a mutation in
/// this coordinator, and a request that was admissible decides against a fleet
/// nobody can write to while it does.
pub async fn handle_workers_prepare_keeper_update(
    core: &CoordCore,
    caller: &Caller,
    request: roost_proto::WorkersPrepareKeeperUpdateRequest,
) -> ServiceResult<roost_proto::WorkersPrepareKeeperUpdateResponse> {
    let device_fingerprint = caller
        .principal
        .require_account_device()
        .map_err(|_| KeeperUpdateRefusal::AuthenticationRequired)?;
    let worker_fp = WorkerFp::try_from(request.worker_fp.as_str())
        .map_err(|_| KeeperUpdateRefusal::MalformedWorkerFingerprint)?;
    let parsed = KeeperUpdateRequest::parse(request)?;
    // Taken BEFORE the machine is looked at: the drain is what makes the
    // emptiness proof below worth anything.
    let _drain = core
        .services
        .write_gate()
        .acquire_exclusive()
        .map_err(|_| KeeperUpdateRefusal::DrainHeld)?;
    tracing::info!(
        %worker_fp,
        action = parsed.action.as_str(),
        force_live = parsed.force_live,
        "a keeper update preparation holds the exclusive write drain",
    );
    reauthorize_device(core, device_fingerprint).await?;
    require_live_worker(core, &worker_fp).await?;
    let decision = decide_keeper_update(
        &core.services.write_gate,
        &core.services.db,
        &worker_fp,
        &parsed,
    )
    .await?;
    if parsed.force_live {
        // The one authorization here that destroys live PTYs, logged with what
        // it destroys and on whose authority.
        tracing::warn!(
            %worker_fp,
            device_fingerprint,
            coordinator_open_sessions = decision.open_session_ids.len(),
            "keeper_maintenance_force_live_authorized",
        );
    }
    let payload = send_keeper_update_preparation(
        &core.services.scrollback,
        &worker_fp,
        decision.preparation(),
        KEEPER_UPDATE_RPC_DEADLINE,
    )
    .await?;
    let identity = match verify_worker_result(decision.action, &payload) {
        Ok(identity) => identity,
        Err(refusal) => {
            tracing::error!(
                %worker_fp,
                action = decision.action.as_str(),
                outcome = worker_outcome(&payload).unwrap_or_default(),
                reason = %refusal,
                "the worker returned a keeper proof this coordinator refuses",
            );
            return Err(refusal.into());
        }
    };
    let identity = match decision.action {
        KeeperUpdateAction::Preserve => identity,
        _ => KeeperIdentity::default(),
    };
    Response::ok(roost_proto::WorkersPrepareKeeperUpdateResponse {
        outcome: worker_outcome(&payload).unwrap_or_default().to_owned(),
        keeper_pid: identity.keeper_pid,
        keeper_epoch: identity.keeper_epoch,
        binding_digest: identity.binding_digest,
        ..Default::default()
    })
}
