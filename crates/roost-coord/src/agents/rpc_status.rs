//! The five observed-agent Connect methods: status get, list, wait, and the
//! two default-agent config methods.
//!
//! Ported from `apps/coord/src/agents/handlers-agent-status.ts` and
//! `handlers-agent-config.ts`. Every method is Shape A: it takes
//! `(core, caller, request)` and reaches the hub, the wait registry and the
//! config through `core.services`, so a handler cannot be called against a
//! table that is not the coordinator's.
//!
//! DURABLE EXISTENCE IS AUTHORIZED BEFORE VOLATILE STATE IS CONSULTED. A
//! session that does not exist, belongs to another dashboard, or has closed is
//! one answer -- `NotFound: agent status not found` -- and it is produced by the
//! sessions table, not by the hub. A caller therefore cannot use the wait's
//! error shape to discover which session ids exist, and a worker route that has
//! gone offline does not make a live session's retained status unreadable.
//!
//! THE LIST ORDER AND THE BROADCAST ORDER ARE ONE ANSWER. The list is the hub's
//! own snapshot -- session-id order, from the table itself -- filtered to the
//! sessions that are still open. A client that re-fetches and a client that
//! applies broadcasts therefore converge, rather than disagreeing about which
//! agent is first.

use std::time::Duration;

use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_observability::LogFields;
use roost_proto as proto;
use sqlx::Row;
use roost_protocol::wire::agent_status::agent_status_identity;
use roost_protocol::wire::{AgentStatus, AgentStatusSource, SessionId};

use crate::agents::config::{AgentLauncherConfig, get_agent_config, set_agent_config};
use crate::agents::status_wait::{
    AgentStatusWaitError, AgentStatusWaitErrorKind, AgentStatusWaitOutcome, AgentStatusWaitRequest,
    AgentStatusWaiter,
};
use crate::coord_core::{Caller, CoordCore};
use crate::rpc::service::ok_response;

/// The largest integer a JavaScript peer can have sent as a revision without
/// losing precision. A larger one did not come from a counting worker.
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// `CoordinatorService.AgentStatusGet` -- one session's retained status.
pub async fn handle_agent_status_get(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentStatusGetRequest,
) -> ServiceResult<proto::AgentStatusGetResponse> {
    require_account_device(caller)?;
    let session_id = require_open_agent_status_session(core, &request.session_id).await?;
    let Some(status) = core.services.agents.status.status_for(&session_id) else {
        return Err(status_not_found());
    };
    ok_response(proto::AgentStatusGetResponse {
        status: roost_proto::buffa::MessageField::some(agent_status_view(&status)),
        ..Default::default()
    })
}

/// `CoordinatorService.AgentStatusList` -- every open session's status, in
/// session-id order.
pub async fn handle_agent_status_list(
    core: &CoordCore,
    caller: &Caller,
    _request: proto::AgentStatusListRequest,
) -> ServiceResult<proto::AgentStatusListResponse> {
    require_account_device(caller)?;
    let dashboard_id = &core.services.boot.require_tenant()?.dashboard_id;
    let open = open_session_ids(core, dashboard_id).await?;
    // The hub's own order, filtered -- not a second sort here, because two sorts
    // of the same rows are two answers the first time they disagree.
    let statuses: Vec<proto::AgentStatusView> = core
        .services
        .agents
        .status
        .snapshot()
        .into_iter()
        .filter(|status| open.contains(&status.common.session_id.to_string()))
        .map(|status| agent_status_view(&status))
        .collect();
    ok_response(proto::AgentStatusListResponse {
        statuses,
        ..Default::default()
    })
}

/// `CoordinatorService.AgentStatusWait` -- block until one exact occupant
/// reaches a state this client named, or until the question ends.
pub async fn handle_agent_status_wait(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentStatusWaitRequest,
) -> ServiceResult<proto::AgentStatusWaitResponse> {
    require_account_device(caller)?;
    // The session boundary runs BEFORE validation, so a malformed wait for a
    // session that does not exist is still the same not-found a well-formed one
    // would get.
    let session_id = require_open_agent_status_session(core, &request.session_id).await?;
    let after_revision = match request.after_revision {
        Some(revision) if revision > MAX_SAFE_INTEGER as u64 => {
            return Err(ConnectError::new(
                ErrorCode::InvalidArgument,
                "invalid agent status wait request",
            ));
        }
        other => other.and_then(|revision| i64::try_from(revision).ok()),
    };
    let wait = AgentStatusWaitRequest::new(
        session_id.as_str(),
        &request.status_epoch,
        &request.occupant_id,
        &request.desired_states,
        after_revision,
        u64::from(request.timeout_ms),
    )
    .map_err(|error| wait_error(error))?;
    let waiter = core
        .services
        .agents
        .status
        .wait_for_agent_status(wait)
        .map_err(wait_error)?;
    let outcome = settle_within(waiter.timeout(), waiter).await?;
    ok_response(proto::AgentStatusWaitResponse {
        outcome: outcome.as_str().to_owned(),
        ..Default::default()
    })
}

/// `CoordinatorService.AgentConfigGet` -- the operator's default agent.
pub async fn handle_agent_config_get(
    core: &CoordCore,
    caller: &Caller,
    _request: proto::AgentConfigGetRequest,
) -> ServiceResult<proto::AgentConfig> {
    require_account_device(caller)?;
    let dashboard_id = &core.services.boot.require_tenant()?.dashboard_id;
    let config = get_agent_config(&core.services.db, dashboard_id)
        .await
        .map_err(internal)?;
    ok_response(agent_config_message(&config))
}

/// `CoordinatorService.AgentConfigSet` -- store and echo the default agent.
pub async fn handle_agent_config_set(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentConfigSetRequest,
) -> ServiceResult<proto::AgentConfig> {
    require_account_device(caller)?;
    let dashboard_id = &core.services.boot.require_tenant()?.dashboard_id;
    let config = set_agent_config(
        &core.services.db,
        dashboard_id,
        &request.selected,
        &request.custom_command,
        request.auto_launch,
    )
    .await
    .map_err(internal)?;
    ok_response(agent_config_message(&config))
}

/// Wait for the outcome, or report the client's own budget as the outcome.
///
/// The waiter's own timer is not the mechanism: the outcome arrives when the
/// retained status changes, and this timeout is only the ceiling the client
/// asked for. A caller that disconnects mid-wait drops this future, which drops
/// the waiter, which deregisters it.
async fn settle_within(
    budget: Duration,
    waiter: AgentStatusWaiter,
) -> Result<AgentStatusWaitOutcome, ConnectError> {
    match tokio::time::timeout(budget, waiter.settle()).await {
        Ok(Ok(outcome)) => Ok(outcome),
        Ok(Err(error)) => Err(wait_error(error)),
        Err(_elapsed) => {
            roost_observability::log::debug(
                "agents.status",
                "wait_timed_out",
                LogFields::new().set("budget_ms", budget.as_millis() as u64),
            );
            Ok(AgentStatusWaitOutcome::TimedOut)
        }
    }
}

/// The hub's view of one retained status, as the RPC projects it.
///
/// PID-free by construction: the worker never reports one, so there is nothing
/// to strip here.
fn agent_status_view(status: &AgentStatus) -> proto::AgentStatusView {
    let identity = agent_status_identity(&status.common);
    let common = &status.common;
    proto::AgentStatusView {
        session_id: common.session_id.to_string(),
        agent_id: common.agent_id.as_str().to_owned(),
        state: common.state.as_str().to_owned(),
        message: common.message.clone(),
        revision: u64::try_from(common.revision).unwrap_or_default(),
        completed_revision: u64::try_from(common.completed_revision).unwrap_or_default(),
        updated_at: common.updated_at as f64,
        active: true,
        status_epoch: identity
            .as_ref()
            .map(|identity| identity.status_epoch.as_str().to_owned()),
        // Both fields read the same borrow, so they are copied out before
        // `identity` is consumed: `source` takes the Option by value.
        occupant_id: identity
            .as_ref()
            .map(|identity| identity.occupant_id.as_str().to_owned()),
        source: identity
            .as_ref()
            .map(|identity| identity.source.as_str().to_owned()),
        // The worker refuses a prompt proof from an occupant whose process is
        // gone, so a row retained only to carry its completion is not promptable.
        promptable: identity.is_some_and(|identity| {
            identity.source == AgentStatusSource::Integration && !common.occupant_exited
        }),
        ..Default::default()
    }
}

fn agent_config_message(config: &AgentLauncherConfig) -> proto::AgentConfig {
    proto::AgentConfig {
        selected: config.selected.clone(),
        custom_command: config.custom_command.clone(),
        auto_launch: config.auto_launch,
        ..Default::default()
    }
}

/// Authorize the session, or refuse it as one indistinguishable not-found.
async fn require_open_agent_status_session(
    core: &CoordCore,
    session_id: &str,
) -> Result<SessionId, ConnectError> {
    let dashboard_id = &core.services.boot.require_tenant()?.dashboard_id;
    let open = open_session_ids(core, dashboard_id).await?;
    let matched = open
        .iter()
        .find(|candidate| candidate.as_str() == session_id)
        .ok_or_else(status_not_found)?;
    SessionId::try_from(matched.as_str()).map_err(|error| {
        roost_observability::log::warn(
            "agents.status",
            "malformed_session_id",
            LogFields::new().set("error", error.to_string()),
        );
        status_not_found()
    })
}

/// Every open session on this dashboard, in the database's own order.
///
/// A `HashSet` because the caller only asks "is this one open", and the ORDER of
/// the answer belongs to the hub's snapshot, never to this lookup.
async fn open_session_ids(
    core: &CoordCore,
    dashboard_id: &str,
) -> Result<std::collections::HashSet<String>, ConnectError> {
    let rows = sqlx::query("SELECT id FROM sessions WHERE dashboard_id = ?1 AND status = 'open'")
        .bind(dashboard_id)
        .fetch_all(core.services.db.pool())
        .await
        .map_err(internal)?;
    Ok(rows
        .into_iter()
        .map(|row| row.get::<String, _>("id"))
        .collect())
}

fn status_not_found() -> ConnectError {
    ConnectError::new(ErrorCode::NotFound, "agent status not found")
}

/// Map a wait refusal to the Connect code the client can act on.
fn wait_error(error: AgentStatusWaitError) -> ConnectError {
    let code = match error.kind() {
        AgentStatusWaitErrorKind::Invalid => ErrorCode::InvalidArgument,
        AgentStatusWaitErrorKind::Capacity => ErrorCode::ResourceExhausted,
        AgentStatusWaitErrorKind::Canceled => ErrorCode::Canceled,
    };
    ConnectError::new(code, error.message())
}

fn internal(error: sqlx::Error) -> ConnectError {
    roost_observability::log::error(
        "agents.status",
        "storage_failed",
        LogFields::new().set("error", error.to_string()),
    );
    ConnectError::new(ErrorCode::Internal, "agent status storage failed")
}

/// Refuse anything that is not a browser, with the marker header a client needs
/// to tell "log in again" from "this method needs a device credential".
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

/// The Connect method each handler answers, and the function that answers it.
///
/// The lead's list: every row is one arm of the single `impl
/// CoordinatorService` block in `rpc/service_impl.rs`, so wiring this domain is
/// reading this table rather than matching on names by hand.
pub const METHOD_HANDLERS: &[(&str, &str)] = &[
    (
        "AgentStatusGet",
        "agents::rpc_status::handle_agent_status_get",
    ),
    (
        "AgentStatusList",
        "agents::rpc_status::handle_agent_status_list",
    ),
    (
        "AgentStatusWait",
        "agents::rpc_status::handle_agent_status_wait",
    ),
    (
        "AgentConfigGet",
        "agents::rpc_status::handle_agent_config_get",
    ),
    (
        "AgentConfigSet",
        "agents::rpc_status::handle_agent_config_set",
    ),
];
