//! Account-device view of deployment ownership and durable access operations.
//! Reads the running coordinator configuration, never browser ownership claims.
//! Mutation capabilities require proof of the local service owner.

use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_host::access::controller::{ConfiguredOwner, access_status};
use roost_host::access::operation::{OperationState, latest_operation, load_operation};
use roost_host::{ProcessEnv, supported_host_platform};
use roost_proto as proto;

use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::rpc::service::ok_response;

pub async fn get_settings(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AccessGetSettingsRequest,
) -> ServiceResult<proto::AccessSettings> {
    require_account_device(caller)?;
    let config = core.services.boot.require_config()?;
    let env = ProcessEnv::new();
    let status = access_status(config, &env);
    let platform = supported_host_platform()
        .map_err(|error| ConnectError::new(ErrorCode::FailedPrecondition, error.to_string()))?;
    let local = matches!(&status.owner, ConfiguredOwner::Unknown)
        && roost_host::access::operation_schedule::coordinator_owns_local_service(&env, platform)
            .await;
    let external = matches!(&status.owner, ConfiguredOwner::Flux(_));
    let (owner, can_preview, unavailable_reason) = match status.owner {
        ConfiguredOwner::Flux(_) => (
            proto::AccessOwner::Flux,
            true,
            "Externally managed deployment; changes require review and application by its owner.",
        ),
        ConfiguredOwner::Unknown if local => (proto::AccessOwner::LocalService, true, ""),
        ConfiguredOwner::Unknown => (
            proto::AccessOwner::Unknown,
            false,
            "This process does not own an installed coordinator service.",
        ),
    };
    let tailscale_failure = if local {
        match roost_host::access::operation_schedule::preview_local_definition(
            &env, platform, false,
        )
        .await
        {
            Ok(_) => roost_host::access::operation_schedule::preview_tailscale_change(
                config, &env, platform,
            )
            .await
            .err()
            .map(|error| error.to_string()),
            Err(error) => Some(error.to_string()),
        }
    } else {
        Some("Tailscale configuration requires an owned local coordinator service.".to_string())
    };
    let operation = if let Some(id) = request.operation_id.as_deref() {
        Some(
            load_operation(&env, platform, id).map_err(|error| {
                ConnectError::new(ErrorCode::FailedPrecondition, error.to_string())
            })?,
        )
    } else if external {
        None
    } else {
        latest_operation(&env, platform)
            .map_err(|error| ConnectError::new(ErrorCode::FailedPrecondition, error.to_string()))?
    };
    let (operation_id, operation_state, operation_detail) = match operation {
        Some(operation) => (
            operation.id,
            match operation.state {
                OperationState::Scheduled => proto::AccessOperationState::Scheduled,
                OperationState::Applying => proto::AccessOperationState::Applying,
                OperationState::Succeeded => proto::AccessOperationState::Succeeded,
                OperationState::Failed => proto::AccessOperationState::Failed,
            },
            operation.detail.unwrap_or_default(),
        ),
        None => (
            String::new(),
            proto::AccessOperationState::None,
            String::new(),
        ),
    };
    ok_response(proto::AccessSettings {
        owner: owner.into(),
        owner_verified: local,
        browser_origin: status.browser_origin.unwrap_or_default(),
        worker_origin: status.worker_origin.unwrap_or_default(),
        can_preview,
        can_apply: local,
        unavailable_reason: unavailable_reason.to_string(),
        operation_id,
        can_configure_tailscale: tailscale_failure.is_none(),
        tailscale_unavailable_reason: tailscale_failure.unwrap_or_default(),
        operation_state: operation_state.into(),
        operation_detail,
        ..Default::default()
    })
}
