//! Coordinator keeper-update preparation, serialized across downstream
//! requests: close channel creation, join the reconcile boundary, verify the
//! coordinator's canonical session set against this worker's, then apply the
//! journaled action or the maintenance shutdown. Ports v2
//! `apps/worker/src/transport/coord-link-keeper-update.ts`. Built by
//! `runtime::owners`; `runtime::downstream` routes `keeperUpdatePrepare` to it.
//!
//! A successful journaled preparation leaves admission CLOSED and the reconcile
//! boundary HELD: the deploy is about to replace this worker, and a spawn or a
//! reconcile against the keeper it just proved would invalidate the proof. Only
//! a failure, or a completed maintenance shutdown, reopens them.

use std::sync::Arc;

use roost_proto::DKeeperUpdatePrepare;
use roost_protocol::keeper_update::{
    JournaledKeeperUpdateV1, validate_keeper_coordinator_open_session_ids,
};
use serde_json::Value;

use super::update_admission::{
    JournaledKeeperUpdateActionV1, KeeperUpdateActionResult, KeeperUpdateHost, UpdateDirection,
    apply_journaled_keeper_update_action, shutdown_keeper_for_maintenance,
};
use crate::link_ports::KeeperUpdatePort;
use crate::session::lifecycle::{SessionManager, SessionTable};
use crate::uplink::OwnerFuture;

/// Releases a held reconcile boundary. Called at most once.
pub type BoundaryRelease = Box<dyn FnOnce() + Send>;

/// v2 `acquireKeeperUpdateBoundary`: block reconciliation and wait out the one
/// in flight. `Err` carries that reconcile's failure.
pub trait KeeperUpdateBoundary: Send + Sync {
    fn acquire(&self) -> OwnerFuture<Result<BoundaryRelease, String>>;
}

/// The two keeper actions a preparation ends in (v2 `applyKeeperUpdateAction`
/// and `shutdownKeeperForMaintenance`, injectable exactly as there).
pub trait KeeperUpdateActions: Send + Sync {
    fn apply(
        &self,
        action: JournaledKeeperUpdateActionV1,
    ) -> OwnerFuture<Result<KeeperUpdateActionResult, String>>;
    fn maintenance_shutdown(&self, force_live: bool) -> OwnerFuture<Result<&'static str, String>>;
}

/// The production actions, over this worker's keeper.
pub struct HostKeeperUpdateActions {
    host: Arc<dyn KeeperUpdateHost>,
}

impl std::fmt::Debug for HostKeeperUpdateActions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HostKeeperUpdateActions")
    }
}

impl HostKeeperUpdateActions {
    pub fn new(host: Arc<dyn KeeperUpdateHost>) -> Self {
        Self { host }
    }
}

impl KeeperUpdateActions for HostKeeperUpdateActions {
    fn apply(
        &self,
        action: JournaledKeeperUpdateActionV1,
    ) -> OwnerFuture<Result<KeeperUpdateActionResult, String>> {
        let host = Arc::clone(&self.host);
        Box::pin(async move { apply_journaled_keeper_update_action(&action, host.as_ref()).await })
    }

    fn maintenance_shutdown(&self, force_live: bool) -> OwnerFuture<Result<&'static str, String>> {
        let host = Arc::clone(&self.host);
        Box::pin(async move { shutdown_keeper_for_maintenance(force_live, host.as_ref()).await })
    }
}

/// The downstream owner of `keeperUpdatePrepare`.
pub struct KeeperUpdatePreparer {
    manager: Arc<SessionManager>,
    table: Arc<SessionTable>,
    boundary: Arc<dyn KeeperUpdateBoundary>,
    actions: Arc<dyn KeeperUpdateActions>,
    /// v2 `preparationTail`: one preparation at a time, in arrival order.
    tail: Arc<tokio::sync::Mutex<()>>,
}

impl std::fmt::Debug for KeeperUpdatePreparer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("KeeperUpdatePreparer")
    }
}

/// How a serialized preparation ended.
enum Prepared {
    Maintenance(&'static str),
    Journaled(KeeperUpdateActionResult),
}

impl KeeperUpdatePreparer {
    pub fn new(
        manager: Arc<SessionManager>,
        table: Arc<SessionTable>,
        boundary: Arc<dyn KeeperUpdateBoundary>,
        actions: Arc<dyn KeeperUpdateActions>,
    ) -> Self {
        Self {
            manager,
            table,
            boundary,
            actions,
            tail: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// The production preparer: its actions act on this worker's keeper, over
    /// the pool's own connection (see `update_host`). Called once by
    /// `runtime::owners`.
    pub fn over_pool(
        manager: Arc<SessionManager>,
        table: Arc<SessionTable>,
        boundary: Arc<dyn KeeperUpdateBoundary>,
        pool: Arc<super::KeeperPool>,
        keeper_endpoint: roost_keeper::client::KeeperEndpoint,
    ) -> Self {
        tracing::info!(socket = %keeper_endpoint.socket.display(), "the keeper update preparer is bound to the pool");
        let host: Arc<dyn KeeperUpdateHost> = Arc::new(super::update_host::PoolKeeperHost::new(
            pool,
            keeper_endpoint,
        ));
        Self::new(
            manager,
            table,
            boundary,
            Arc::new(HostKeeperUpdateActions::new(host)),
        )
    }
}

impl KeeperUpdatePort for KeeperUpdatePreparer {
    fn prepare(&self, request: DKeeperUpdatePrepare) -> OwnerFuture<Result<Value, String>> {
        // Close admission in THIS call, before joining the tail, so a spawn
        // issued after the frame arrived cannot cross the preparation boundary.
        let admission = self.manager.begin_keeper_update_preparation();
        let tail = Arc::clone(&self.tail);
        let table = Arc::clone(&self.table);
        let boundary = Arc::clone(&self.boundary);
        let actions = Arc::clone(&self.actions);
        tracing::info!(
            request_id = %request.request_id,
            maintenance = request.maintenance,
            direction = %request.direction,
            "keeper update preparation closed channel admission"
        );
        Box::pin(async move {
            let _turn = tail.lock_owned().await;
            let rollback_admission = admission.await;
            let mut reconcile: Option<BoundaryRelease> = None;
            let prepared = prepare_serialized(
                &request,
                &table,
                boundary.as_ref(),
                actions.as_ref(),
                &mut reconcile,
            )
            .await;
            let answer = match prepared {
                Ok(Prepared::Maintenance(outcome)) => {
                    if let Some(release) = reconcile.take() {
                        release();
                    }
                    rollback_admission.rollback();
                    Ok(serde_json::json!({ "outcome": outcome }))
                }
                Ok(Prepared::Journaled(result)) => {
                    serde_json::to_value(result).map_err(|error| error.to_string())
                }
                Err(error) => Err(error),
            };
            match &answer {
                Ok(data) => {
                    tracing::info!(request_id = %request.request_id, %data, "keeper update preparation completed")
                }
                Err(error) => {
                    rollback_admission.rollback();
                    if let Some(release) = reconcile.take() {
                        release();
                    }
                    tracing::warn!(request_id = %request.request_id, %error, "keeper update preparation failed; admission reopened");
                }
            }
            answer
        })
    }
}

/// The serialized body. The reconcile boundary is handed back through
/// `reconcile` the moment it is held, so every failure after it releases it.
async fn prepare_serialized(
    request: &DKeeperUpdatePrepare,
    table: &SessionTable,
    boundary: &dyn KeeperUpdateBoundary,
    actions: &dyn KeeperUpdateActions,
    reconcile: &mut Option<BoundaryRelease>,
) -> Result<Prepared, String> {
    *reconcile = Some(boundary.acquire().await?);
    let coordinator_ids = request.coordinator_open_session_ids.clone();
    validate_keeper_coordinator_open_session_ids("coordinator_open_session_ids", &coordinator_ids)
        .map_err(|error| error.to_string())?;
    let live = table.live();
    let mut worker_ids: Vec<String> = live
        .iter()
        .map(|(session, _)| session.to_string())
        .collect();
    worker_ids.sort();
    validate_keeper_coordinator_open_session_ids("worker_open_session_ids", &worker_ids)
        .map_err(|error| error.to_string())?;
    let mut worker_channels: Vec<u32> = live
        .iter()
        .map(|(_, channel)| u32::from(*channel))
        .collect();
    worker_channels.sort_unstable();
    if coordinator_ids != worker_ids {
        return Err(
            "coordinator and worker open sessions changed after update admission".to_owned(),
        );
    }
    if request.maintenance {
        if request.journaled_update_json.is_some()
            || !request.direction.is_empty()
            || (!request.force_live && !coordinator_ids.is_empty())
        {
            return Err("keeper maintenance request is malformed".to_owned());
        }
        let outcome = actions.maintenance_shutdown(request.force_live).await?;
        return Ok(Prepared::Maintenance(outcome));
    }
    // A journaled envelope never authorizes destruction: force_live belongs to
    // the live maintenance request alone.
    let malformed = || "journaled keeper update request is malformed".to_owned();
    let (Some(journaled), false) = (&request.journaled_update_json, request.force_live) else {
        return Err(malformed());
    };
    let direction = match request.direction.as_str() {
        "source" => UpdateDirection::Source,
        "target" => UpdateDirection::Target,
        _ => return Err(malformed()),
    };
    let value: Value = serde_json::from_str(journaled).map_err(|error| error.to_string())?;
    let update = JournaledKeeperUpdateV1::parse(&value).map_err(|error| error.to_string())?;
    let result = actions
        .apply(JournaledKeeperUpdateActionV1 {
            schema_version: 1,
            update,
            direction,
            coordinator_open_session_ids: coordinator_ids,
            worker_open_channel_ids: worker_channels,
        })
        .await?;
    Ok(Prepared::Journaled(result))
}
