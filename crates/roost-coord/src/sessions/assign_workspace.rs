//! `SessionsAssignWorkspace`: move one session into a workspace, or out of every
//! workspace, and publish the junction of each workspace the move touched.
//! Called from the arm in `rpc/service_impl.rs`. Ports the `sessionsAssignWorkspace`
//! handler of `apps/coord/src/sessions/handlers-sessions.ts`.
//!
//! BOTH MEMBERSHIP REPRESENTATIONS MOVE TOGETHER, inside the append's own
//! transaction (`events::projection_writes::set_workspace_membership`), because
//! a session in the column but not the junction is counted once as a member and
//! once as an orphan. The prior workspaces are read inside that same
//! transaction, before the move, and their `sessions-set` deltas are published
//! strictly after it commits: no bus delta may precede the state it describes.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};

use connectrpc::{ConnectError, ErrorCode, Response, ServiceResult};
use roost_proto::{SessionsAssignWorkspaceRequest, SessionsAssignWorkspaceResponse};
use roost_protocol::wire::{SessionEvent, WorkspaceDelta, WorkspaceId};

use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::events::append::AtomicExtraWork;
use crate::rpc::service::now_ms;
use crate::sessions::rpc_sessions::{
    append_coordinator_event, lease, read_failed, session_exists, stored_session_id,
};
use crate::sessions::workspaces::members_of;

/// Assign a session to a workspace; an absent or empty id unassigns it.
pub async fn handle_sessions_assign_workspace(
    core: &CoordCore,
    caller: &Caller,
    req: SessionsAssignWorkspaceRequest,
) -> ServiceResult<SessionsAssignWorkspaceResponse> {
    require_account_device(caller)?;
    let _lease = lease(core)?;
    let target = req.workspace_id.as_deref().filter(|id| !id.is_empty());
    if !session_exists(core, &req.session_id).await? {
        return assigned(false);
    }
    if let Some(target) = target
        && version_of(core, target).await?.is_none()
    {
        return assigned(false);
    }
    let session = stored_session_id(&req.session_id)?;
    let workspace_id = target
        .map(|target| {
            WorkspaceId::try_from(target).map_err(|error| {
                tracing::error!(%error, "sessions: a stored workspace id is malformed");
                ConnectError::new(ErrorCode::Internal, "stored workspace id is invalid")
            })
        })
        .transpose()?;
    let prior = Arc::new(Mutex::new(BTreeSet::new()));
    let assigned_event = SessionEvent::WorkspaceAssigned {
        session_id: session.clone(),
        workspace_id: workspace_id.clone(),
        ts: now_ms(),
        trace_id: None,
    };
    append_coordinator_event(
        core,
        assigned_event,
        Some(collect_prior_workspaces(session.as_str(), &prior)),
    )
    .await?;
    tracing::info!(
        session_id = %session,
        workspace_id = target.unwrap_or_default(),
        "sessions: workspace assigned"
    );
    let prior = std::mem::take(&mut *prior.lock().unwrap_or_else(PoisonError::into_inner));
    let touched = target
        .map(str::to_owned)
        .into_iter()
        .chain(prior.into_iter().filter(|id| Some(id.as_str()) != target));
    for workspace_id in touched {
        publish_sessions_set(core, &workspace_id).await?;
    }
    assigned(true)
}

/// The atomic extra work that records which workspaces held the session before
/// the move, read in the append's transaction so no concurrent move slips in.
fn collect_prior_workspaces(
    session_id: &str,
    prior: &Arc<Mutex<BTreeSet<String>>>,
) -> AtomicExtraWork<'static> {
    let session_id = session_id.to_owned();
    let prior = Arc::clone(prior);
    Box::new(move |connection| {
        let session_id = session_id.clone();
        let prior = Arc::clone(&prior);
        Box::pin(async move {
            let held: Vec<String> = sqlx::query_scalar(
                "SELECT workspace_id FROM workspace_sessions WHERE session_id = ?1",
            )
            .bind(&session_id)
            .fetch_all(&mut *connection)
            .await?;
            prior
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend(held);
            Ok(())
        })
    })
}

/// Publish one workspace's committed junction, if the workspace still exists.
async fn publish_sessions_set(core: &CoordCore, workspace_id: &str) -> Result<(), ConnectError> {
    let Some(version) = version_of(core, workspace_id).await? else {
        return Ok(());
    };
    let mut connection = core
        .services
        .db
        .pool()
        .acquire()
        .await
        .map_err(|error| read_failed("workspace junction", &error))?;
    let session_ids = members_of(&mut connection, workspace_id)
        .await
        .map_err(|error| {
            tracing::error!(workspace_id, %error, "sessions: a workspace junction read failed");
            ConnectError::new(ErrorCode::Internal, "workspace junction read failed")
        })?;
    let Ok(id) = WorkspaceId::try_from(workspace_id) else {
        tracing::error!(workspace_id, "sessions: a stored workspace id is malformed");
        return Ok(());
    };
    core.services
        .buses
        .workspace_bus
        .publish(WorkspaceDelta::SessionsSet {
            id,
            session_ids,
            version,
        });
    Ok(())
}

async fn version_of(core: &CoordCore, workspace_id: &str) -> Result<Option<i64>, ConnectError> {
    sqlx::query_scalar("SELECT version FROM workspaces WHERE id = ?1")
        .bind(workspace_id)
        .fetch_optional(core.services.db.pool())
        .await
        .map_err(|error| read_failed("workspace lookup", &error))
}

fn assigned(ok: bool) -> ServiceResult<SessionsAssignWorkspaceResponse> {
    Response::ok(SessionsAssignWorkspaceResponse {
        ok,
        ..Default::default()
    })
}
