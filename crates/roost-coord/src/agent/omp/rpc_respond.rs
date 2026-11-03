//! Explicit ACP interaction responses scoped to their durable conversation.
//! Admission deduplicates caller request ids before routing to the owning worker.

use super::{rpc, rpc_settings::database_error, store};
use crate::{coord_core::CoordCore, rpc::service::ok_response};
use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_proto as proto;
use roost_protocol::wire::{
    agent_chat::validate_omp_interaction_response, coord_worker::CoordWorkerDownstream,
};

pub async fn handle_agent_omp_respond(
    core: &CoordCore,
    caller: &crate::coord_core::Caller,
    request: proto::AgentOmpRespondRequest,
) -> ServiceResult<proto::AgentOmpRespondResponse> {
    crate::auth::principal::require_account_device(caller)?;
    crate::agent::rpc_chat::require_request_id(&request.request_id)?;
    let inserted = store::admit_input(
        &core.services.db,
        &request.conversation_id,
        &request.request_id,
        "respond",
    )
    .await
    .map_err(database_error)?;
    if !inserted {
        return ok_response(proto::AgentOmpRespondResponse::default());
    }
    let result = respond_to_omp(core, &request).await;
    if result.is_err() {
        store::remove_admission(
            &core.services.db,
            &request.conversation_id,
            &request.request_id,
        )
        .await
        .map_err(database_error)?;
    }
    result?;
    ok_response(proto::AgentOmpRespondResponse::default())
}

async fn respond_to_omp(
    core: &CoordCore,
    request: &proto::AgentOmpRespondRequest,
) -> Result<(), ConnectError> {
    let conversation = rpc::find_omp(core, &request.conversation_id)
        .await?
        .ok_or_else(|| ConnectError::new(ErrorCode::NotFound, "OMP conversation not found"))?;
    let (_, transcript) = store::snapshot(&core.services.db, &conversation.id)
        .await
        .map_err(database_error)?;
    let interaction = transcript
        .omp_interactions
        .iter()
        .find(|interaction| interaction.id == request.item_id)
        .ok_or_else(|| {
            ConnectError::new(
                ErrorCode::FailedPrecondition,
                "OMP interaction is no longer pending",
            )
        })?;
    let normalized = validate_omp_interaction_response(
        interaction,
        &request.item_id,
        &request.action,
        &request.content_json,
    )
    .map_err(|error| {
        ConnectError::new(
            ErrorCode::InvalidArgument,
            format!("invalid OMP interaction response: {error:?}"),
        )
    })?;
    let content_json = if matches!(request.action.as_str(), "accept" | "permission") {
        normalized.to_string()
    } else {
        request.content_json.clone()
    };
    let worker = core
        .services
        .omp
        .interaction_worker(&conversation.worker_fp)
        .map_err(rpc::unavailable)?;
    if worker.send(CoordWorkerDownstream::OmpRespond(proto::DOmpRespond {
        conversation_id: conversation.id,
        request_id: request.item_id.clone(),
        action: request.action.clone(),
        content_json,
        ..Default::default()
    })) == 0
    {
        return Err(rpc::unavailable(super::OmpUnavailable::Offline));
    }
    Ok(())
}
