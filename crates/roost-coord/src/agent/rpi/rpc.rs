//! Account-device Rpi catalog and saved-session activation endpoints.
//! Catalogs use the existing typed worker correlation table and process fence.
//! Session activation durably admits OPEN without submitting a user message.

use std::sync::Arc;

use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_proto as proto;
use roost_protocol::wire::{WorkerFp, coord_worker::CoordWorkerDownstream};

use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore, worker_handle::WorkerHandle};
use crate::rpc::service::ok_response;
use crate::terminal_screen::pending_rpcs::DEFAULT_PENDING_RPC_TIMEOUT_MS;
use crate::workers::send::{current_routable_worker, send_frame_through};

pub(super) fn worker(
    core: &CoordCore,
    fingerprint: &str,
) -> Result<Arc<WorkerHandle>, ConnectError> {
    let fingerprint = WorkerFp::try_from(fingerprint.to_owned())
        .map_err(|_| ConnectError::new(ErrorCode::InvalidArgument, "invalid worker fingerprint"))?;
    let worker = current_routable_worker(&core.services.workers, &fingerprint)
        .ok_or_else(|| ConnectError::new(ErrorCode::Unavailable, "worker offline"))?;
    if !worker
        .capabilities
        .contains(roost_protocol::versioning::CAPABILITY_RPI_AGENT_V1)
        || worker.process_epoch.as_deref().is_none_or(str::is_empty)
    {
        return Err(ConnectError::new(
            ErrorCode::FailedPrecondition,
            "Update Roost on this machine to use RPI chats",
        ));
    }
    Ok(worker)
}

pub async fn handle_agent_rpi_models_list(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentRpiModelsListRequest,
) -> ServiceResult<proto::AgentRpiModelsListResponse> {
    require_account_device(caller)?;
    let models = available_models(core, &request.worker_fp).await?;
    ok_response(proto::AgentRpiModelsListResponse {
        models,
        ..Default::default()
    })
}

pub(crate) async fn available_models(
    core: &CoordCore,
    fingerprint: &str,
) -> Result<Vec<proto::RpiModel>, ConnectError> {
    let worker = worker(core, fingerprint)?;
    let mut pending = core
        .services
        .scrollback
        .pending()
        .create_fresh(Some(fingerprint), super::super::now_ms())?;
    let frame = CoordWorkerDownstream::RpiModelsList(proto::DRpiModelsList {
        request_id: pending.request_id().to_owned(),
        expected_worker_epoch: worker.process_epoch.clone().unwrap_or_default(),
        ..Default::default()
    });
    if !send_frame_through(&core.services.workers, &worker, frame).is_admitted() {
        return Err(ConnectError::new(ErrorCode::Unavailable, "worker offline"));
    }
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(DEFAULT_PENDING_RPC_TIMEOUT_MS),
        pending.settle_typed::<proto::WRpiModelsListResult>(),
    )
    .await
    .map_err(|_| ConnectError::new(ErrorCode::DeadlineExceeded, "RPI model catalog timed out"))??;
    if !worker.is_routable()
        || worker.process_epoch.as_deref() != Some(result.worker_epoch.as_str())
    {
        return Err(ConnectError::new(
            ErrorCode::Unavailable,
            "worker connection changed",
        ));
    }
    if !result.error_code.is_empty() {
        return Err(ConnectError::new(
            ErrorCode::FailedPrecondition,
            "RPI provider setup failed on this machine; check roost rpi models",
        ));
    }
    Ok(result.models)
}

pub async fn handle_agent_rpi_session_open(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentRpiSessionOpenRequest,
) -> ServiceResult<proto::AgentRpiSessionOpenResponse> {
    require_account_device(caller)?;
    super::super::rpc_chat::require_request_id(&request.request_id)?;
    let (worker, command) = super::commands::admit_open(core, &request).await?;
    let _ = send_frame_through(
        &core.services.workers,
        &worker,
        CoordWorkerDownstream::RpiCommand(command),
    );
    ok_response(proto::AgentRpiSessionOpenResponse::default())
}

pub const METHOD_HANDLERS: &[(&str, &str)] = &[
    (
        "AgentRpiModelsList",
        "agent::rpi::rpc::handle_agent_rpi_models_list",
    ),
    (
        "AgentRpiSessionOpen",
        "agent::rpi::rpc::handle_agent_rpi_session_open",
    ),
];
