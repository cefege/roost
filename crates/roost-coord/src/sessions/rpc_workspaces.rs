//! The five workspace Connect methods, and the one write whose ORDER is the
//! request's own semantics: a membership rewrite that must collect the
//! workspaces it empties. Everything a write is made of is in `workspaces.rs`.
//! Ported from `apps/coord/src/sessions/handlers-workspaces.ts`.
//!
//! EVERY MUTATION LEASES FROM THE ONE WRITE GATE, in the handler rather than the
//! interceptor because a handler is the only place a test can hold the gate and
//! watch a mutation be refused (`workers/rpc.rs`).
//!
//! A WRITE COMMITS BEFORE IT PUBLISHES. A delta that precedes the state it
//! describes hands a browser a workspace its next re-fetch contradicts, so the
//! tree never publishes and one place decides both.

use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_proto::buffa::MessageField;
use roost_protocol::wire::{SessionId, Workspace, WorkspaceDelta, WorkspaceId};

use crate::coord_core::{Caller, CoordCore};
use crate::rpc::service::{now_ms, ok_response};
use crate::sessions::workspaces::{
    COLUMNS, Row, WorkspaceError, create_workspace, delete_workspace,
    detach_members, id_list, junction, list_workspaces, members_of, project, session_cwds,
    set_membership, unclaim, unique, update_workspace,
};
use crate::write_gate::SharedLease;

/// The Connect method each handler answers, and the function that answers it.
///
/// The integrator's list: every row is one arm of `rpc/service_impl.rs`.
pub const METHOD_HANDLERS: [(&str, &str); 5] = [
    ("WorkspacesList", "sessions::rpc_workspaces::handle_workspaces_list"),
    ("WorkspacesCreate", "sessions::rpc_workspaces::handle_workspaces_create"),
    ("WorkspacesUpdate", "sessions::rpc_workspaces::handle_workspaces_update"),
    ("WorkspacesDelete", "sessions::rpc_workspaces::handle_workspaces_delete"),
    (
        "WorkspacesSetSessions",
        "sessions::rpc_workspaces::handle_workspaces_set_sessions",
    ),
];

/// Another workspace's membership, as this rewrite left it.
#[derive(Debug, Clone)]
pub enum OtherMembership {
    /// Still there, with the membership it ended up with.
    Kept {
        id: WorkspaceId,
        version: i64,
        session_ids: Vec<SessionId>,
    },
    /// Deleted for losing its last session.
    Deleted { id: WorkspaceId },
}

/// What a membership rewrite decided. `workspace` is the target as it stands: one
/// the rewrite emptied carries no members, because the collector deleted it.
#[derive(Debug, Clone)]
pub struct RewrittenMembership {
    pub workspace: Workspace,
    pub target_deleted: bool,
    /// Every other workspace the rewrite touched, in the order it touched them.
    pub others: Vec<OtherMembership>,
}

impl RewrittenMembership {
    /// The target's own delta, which is a DELETION when the rewrite emptied it.
    fn target_delta(&self) -> WorkspaceDelta {
        if self.target_deleted {
            return WorkspaceDelta::Deleted {
                id: self.workspace.id.clone(),
            };
        }
        WorkspaceDelta::SessionsSet {
            id: self.workspace.id.clone(),
            session_ids: self.workspace.session_ids.clone(),
            version: self.workspace.version,
        }
    }
}

impl OtherMembership {
    /// The delta this workspace owes a subscriber. It is named here rather than
    /// at the publish site so the two shapes of "this workspace changed" cannot
    /// be spelled two ways.
    fn delta(&self) -> WorkspaceDelta {
        match self {
            Self::Kept {
                id,
                version,
                session_ids,
            } => WorkspaceDelta::SessionsSet {
                id: id.clone(),
                session_ids: session_ids.clone(),
                version: *version,
            },
            Self::Deleted { id } => WorkspaceDelta::Deleted { id: id.clone() },
        }
    }
}

/// `CoordinatorService.WorkspacesList` -- every workspace, in the store's order.
pub async fn handle_workspaces_list(
    core: &CoordCore,
    caller: &Caller,
    _request: roost_proto::WorkspacesListRequest,
) -> ServiceResult<roost_proto::WorkspacesListResponse> {
    require_account_device(caller)?;
    let rows = list_workspaces(&core.services.db).await.map_err(refuse)?;
    ok_response(roost_proto::WorkspacesListResponse {
        workspaces: rows.iter().map(workspace_to_proto).collect(),
        ..Default::default()
    })
}

/// `CoordinatorService.WorkspacesCreate` -- add one, or answer with the row that
/// already lives at that folder.
pub async fn handle_workspaces_create(
    core: &CoordCore,
    caller: &Caller,
    request: roost_proto::WorkspacesCreateRequest,
) -> ServiceResult<roost_proto::WorkspacesCreateResponse> {
    require_account_device(caller)?;
    let dashboard_id = dashboard(core)?;
    let _lease = lease(core)?;
    let created = create_workspace(&core.services.db, &request, dashboard_id, now_ms())
        .await
        .map_err(refuse)?;
    if created.created {
        let delta = WorkspaceDelta::Created {
            workspace: created.workspace.clone(),
        };
        publish(core, delta);
        tracing::info!(workspace_id = %created.workspace.id, "workspace created");
    }
    ok_response(roost_proto::WorkspacesCreateResponse {
        workspace: MessageField::some(workspace_to_proto(&created.workspace)),
        ..Default::default()
    })
}

/// `CoordinatorService.WorkspacesUpdate` -- rename, recolour, re-file, reorder.
pub async fn handle_workspaces_update(
    core: &CoordCore,
    caller: &Caller,
    request: roost_proto::WorkspacesUpdateRequest,
) -> ServiceResult<roost_proto::WorkspacesUpdateResponse> {
    require_account_device(caller)?;
    workspace_id(&request.id)?;
    let _lease = lease(core)?;
    let workspace = update_workspace(&core.services.db, &request, now_ms())
        .await
        .map_err(refuse)?;
    let delta = WorkspaceDelta::Updated {
        workspace: workspace.clone(),
    };
    publish(core, delta);
    tracing::info!(workspace_id = %workspace.id, version = workspace.version, "workspace updated");
    ok_response(roost_proto::WorkspacesUpdateResponse {
        workspace: MessageField::some(workspace_to_proto(&workspace)),
        ..Default::default()
    })
}

/// `CoordinatorService.WorkspacesDelete` -- the workspace and its membership.
pub async fn handle_workspaces_delete(
    core: &CoordCore,
    caller: &Caller,
    request: roost_proto::WorkspacesDeleteRequest,
) -> ServiceResult<roost_proto::WorkspacesDeleteResponse> {
    require_account_device(caller)?;
    let id = workspace_id(&request.id)?;
    let _lease = lease(core)?;
    let deleted = delete_workspace(&core.services.db, id.as_str(), request.if_version)
        .await
        .map_err(refuse)?;
    let delta = WorkspaceDelta::Deleted { id: deleted.clone() };
    publish(core, delta);
    tracing::info!(workspace_id = %deleted, "workspace deleted with its membership");
    ok_response(roost_proto::WorkspacesDeleteResponse {
        ok: true,
        ..Default::default()
    })
}

/// `CoordinatorService.WorkspacesSetSessions` -- replace the membership, and
/// report every workspace the move touched, target first.
pub async fn handle_workspaces_set_sessions(
    core: &CoordCore,
    caller: &Caller,
    request: roost_proto::WorkspacesSetSessionsRequest,
) -> ServiceResult<roost_proto::WorkspacesSetSessionsResponse> {
    require_account_device(caller)?;
    let dashboard_id = dashboard(core)?;
    workspace_id(&request.id)?;
    let _lease = lease(core)?;
    let outcome = rewrite_membership(core, &request, dashboard_id, now_ms())
        .await
        .map_err(refuse)?;
    publish(core, outcome.target_delta());
    for other in &outcome.others {
        publish(core, other.delta());
    }
    if !outcome.others.is_empty() {
        let touched = outcome.others.len();
        tracing::info!(workspace_id = %outcome.workspace.id, touched, "membership rewritten");
    }
    ok_response(roost_proto::WorkspacesSetSessionsResponse {
        workspace: MessageField::some(workspace_to_proto(&outcome.workspace)),
        ..Default::default()
    })
}

/// Replace a workspace's members, and collect the workspaces that lost their
/// last one. The order is the correctness of the collector:
///
/// 1. the claim, so a stale `if_version` aborts before anything moved;
/// 2. the rewrite of the junction, which is what empties a workspace;
/// 3. THEN the emptiness read, because emptiness is a function of the junction --
///    read it before the rewrite and a workspace that still holds a session reads
///    as empty, so the collector deletes a live parent and cascades its
///    membership away;
/// 4. then the column, so nothing is left naming a workspace this call deleted --
///    from the junction read taken BEFORE step 2, because by now it has cascaded.
async fn rewrite_membership(
    core: &CoordCore,
    request: &roost_proto::WorkspacesSetSessionsRequest,
    dashboard_id: &str,
    now_ms: i64,
) -> Result<RewrittenMembership, WorkspaceError> {
    let mut transaction = core.services.db.pool().begin().await?;
    let target = sqlx::query_as::<_, Row>(&format!(
        "UPDATE workspaces SET updated_at_ms = {now_ms}, version = version + 1 \
         WHERE id = ? AND version = ? RETURNING {COLUMNS}"
    ))
    .bind(&request.id)
    .bind(i64::try_from(request.if_version).unwrap_or(i64::MAX))
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or(WorkspaceError::VersionMismatch)?;
    let members = unique(&request.session_ids);
    if session_cwds(&mut transaction, &members).await?.len() != members.len() {
        return Err(WorkspaceError::SessionNotFound);
    }
    // The target's own members, read BEFORE its junction rows go: they are
    // deleted two steps down, and a target this rewrite empties must not leave a
    // session holding a workspace_id that names a row the rewrite deleted.
    let prior = members_of(&mut transaction, &request.id).await?;
    // The workspaces losing these sessions, read before the rewrite: after it,
    // the junction no longer names them.
    let mut affected = vec![request.id.clone()];
    for source in sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT workspace_id FROM workspace_sessions \
         WHERE session_id IN (SELECT value FROM json_each(?)) AND workspace_id != ?",
    )
    .bind(id_list(&members)?)
    .bind(&request.id)
    .fetch_all(&mut *transaction)
    .await?
    {
        if !affected.contains(&source) {
            affected.push(source);
        }
    }
    sqlx::query("DELETE FROM workspace_sessions WHERE workspace_id = ?")
        .bind(&request.id)
        .execute(&mut *transaction)
        .await?;
    set_membership(&mut transaction, &request.id, dashboard_id, &members, now_ms).await?;
    let survivors = junction(&mut transaction, &affected).await?;
    let emptied: Vec<String> = affected
        .iter()
        .filter(|id| !survivors.contains_key(*id))
        .cloned()
        .collect();
    for workspace_id in &emptied {
        if workspace_id == &request.id {
            unclaim(&mut transaction, &request.id, &prior).await?;
        } else {
            detach_members(&mut transaction, workspace_id).await?;
        }
    }
    if !emptied.is_empty() {
        sqlx::query("DELETE FROM workspaces WHERE id IN (SELECT value FROM json_each(?))")
            .bind(id_list(&emptied)?)
            .execute(&mut *transaction)
            .await?;
    }
    let mut others = Vec::new();
    for workspace_id in &affected {
        if workspace_id == &request.id {
            continue;
        }
        let id = WorkspaceId::try_from(workspace_id.clone())?;
        others.push(match survivors.get(workspace_id) {
            Some((version, session_ids)) => OtherMembership::Kept {
                id,
                version: *version,
                session_ids: session_ids.clone(),
            },
            None => OtherMembership::Deleted { id },
        });
    }
    let target_deleted = !survivors.contains_key(&request.id);
    let members_left = survivors
        .get(&request.id)
        .map_or_else(Vec::new, |(_, session_ids)| session_ids.clone());
    transaction.commit().await?;
    Ok(RewrittenMembership {
        workspace: project(&target, members_left)?,
        target_deleted,
        others,
    })
}




/// A committed write's delta, to every live Sync socket's workspace lane.
fn publish(core: &CoordCore, delta: WorkspaceDelta) {
    core.services.buses.workspace_bus.publish(delta);
}

/// The tree's value as the browser's `Workspace` message, field for field. One
/// function, because two projections is a pair that agrees until someone adds a
/// field to one of them.
fn workspace_to_proto(workspace: &Workspace) -> roost_proto::Workspace {
    roost_proto::Workspace {
        id: workspace.id.as_str().to_owned(),
        worker_fp: workspace.worker_fp.as_str().to_owned(),
        name: workspace.name.clone(),
        folder_path: workspace.folder_path.clone(),
        color: workspace.color.clone(),
        // The wire validator holds both nonnegative, so these are the same
        // numbers; the fallbacks exist because a `u32` field cannot carry one
        // that is out of range, and one row is not worth failing a list over.
        position: u32::try_from(workspace.position).unwrap_or(u32::MAX),
        version: u64::try_from(workspace.version).unwrap_or(0),
        created_at_ms: u64::try_from(workspace.created_at_ms).unwrap_or(0),
        updated_at_ms: u64::try_from(workspace.updated_at_ms).unwrap_or(0),
        session_ids: workspace
            .session_ids
            .iter()
            .map(|session_id| session_id.as_str().to_owned())
            .collect(),
        ..Default::default()
    }
}

/// A device is required even on the read: the list is dashboard-local state.
fn require_account_device(caller: &Caller) -> Result<&str, ConnectError> {
    caller.principal.require_account_device().map_err(|_| {
        let mut error = ConnectError::new(ErrorCode::Unauthenticated, "authentication required");
        error.response_headers_mut().insert(
            axum::http::HeaderName::from_static(crate::auth::principal::AUTH_LAYER_HEADER),
            axum::http::HeaderValue::from_static(crate::auth::principal::AUTH_LAYER_DEVICE),
        );
        error
    })
}

/// The tenancy scope every row and junction this domain writes carries.
fn dashboard(core: &CoordCore) -> Result<&str, ConnectError> {
    Ok(core.services.boot.require_tenant()?.dashboard_id.as_str())
}

fn lease(core: &CoordCore) -> Result<SharedLease, ConnectError> {
    core.services
        .write_gate()
        .acquire_shared()
        .map_err(|error| ConnectError::new(ErrorCode::Unavailable, error.to_string()))
}

/// A workspace id a caller named, branded before it reaches a query or a delta.
fn workspace_id(value: &str) -> Result<WorkspaceId, ConnectError> {
    WorkspaceId::try_from(value)
        .map_err(|error| ConnectError::new(ErrorCode::InvalidArgument, error.to_string()))
}

fn refuse(error: WorkspaceError) -> ConnectError {
    let code = match error {
        WorkspaceError::VersionMismatch => ErrorCode::FailedPrecondition,
        WorkspaceError::WorkerNotFound | WorkspaceError::SessionNotFound => ErrorCode::NotFound,
        WorkspaceError::Sqlite(_) | WorkspaceError::IdList(_) | WorkspaceError::Value(_) => {
            ErrorCode::Internal
        }
    };
    ConnectError::new(code, error.to_string())
}
