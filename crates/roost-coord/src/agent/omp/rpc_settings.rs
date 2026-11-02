//! Device-authorized OMP-only settings and machine catalog RPCs.
//! Saving a scope republishes effective summaries for every OMP conversation.
//! These routes never read or distribute the machines' OMP credentials.

use super::{store, summary::summary};
use crate::{
    auth::principal::require_account_device,
    coord_core::{Caller, CoordCore},
    rpc::service::ok_response,
};
use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_agent::ChatSink;
use roost_proto as proto;
use roost_protocol::wire::agent_chat::*;

pub async fn handle_agent_omp_settings_get(
    core: &CoordCore,
    caller: &Caller,
    _request: proto::AgentOmpSettingsGetRequest,
) -> ServiceResult<proto::AgentOmpSettingsGetResponse> {
    require_account_device(caller)?;
    let view = OmpSettingsView {
        scopes: store::settings(&core.services.db)
            .await
            .map_err(database_error)?,
        catalogs: store::catalogs(&core.services.db)
            .await
            .map_err(database_error)?,
    };
    ok_response(proto::AgentOmpSettingsGetResponse {
        settings_json: serde_json::to_string(&view)
            .map_err(|error| ConnectError::new(ErrorCode::Internal, error.to_string()))?,
        ..Default::default()
    })
}

pub async fn handle_agent_omp_settings_set(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentOmpSettingsSetRequest,
) -> ServiceResult<proto::AgentOmpSettingsSetResponse> {
    require_account_device(caller)?;
    let scoped: OmpScopedSettings = serde_json::from_str(&request.scoped_json)
        .map_err(|error| ConnectError::new(ErrorCode::InvalidArgument, error.to_string()))?;
    let valid = match &scoped.scope {
        OmpSettingsScope::Defaults => true,
        OmpSettingsScope::Machine { worker_fp } => !worker_fp.is_empty(),
        OmpSettingsScope::Folder { worker_fp, cwd } => !worker_fp.is_empty() && !cwd.is_empty(),
    };
    if !valid {
        return Err(ConnectError::new(
            ErrorCode::InvalidArgument,
            "OMP scope requires a machine and folders require a working directory",
        ));
    }
    store::put_settings(&core.services.db, scoped)
        .await
        .map_err(database_error)?;
    let scopes = store::settings(&core.services.db)
        .await
        .map_err(database_error)?;
    for conversation in store::list(&core.services.db)
        .await
        .map_err(database_error)?
    {
        core.services
            .agent
            .sink
            .conversation(&summary(&conversation, &scopes))
            .await;
    }
    tracing::info!("OMP settings scope saved");
    ok_response(proto::AgentOmpSettingsSetResponse::default())
}

pub(crate) fn database_error(error: sqlx::Error) -> ConnectError {
    ConnectError::new(ErrorCode::Internal, format!("OMP database: {error}"))
}

pub const METHOD_HANDLERS: &[(&str, &str)] = &[
    (
        "AgentOmpSettingsGet",
        "agent::omp::rpc_settings::handle_agent_omp_settings_get",
    ),
    (
        "AgentOmpSettingsSet",
        "agent::omp::rpc_settings::handle_agent_omp_settings_set",
    ),
];
