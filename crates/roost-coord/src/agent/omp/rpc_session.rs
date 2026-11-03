//! Reopens a persisted ACP session for an authorized OMP conversation.
//! The worker capability gate rejects peers that cannot expose session modes.

use super::rpc;
use crate::{coord_core::CoordCore, rpc::service::ok_response};
use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_proto as proto;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;

pub async fn handle_agent_omp_session_open(
    core: &CoordCore,
    caller: &crate::coord_core::Caller,
    request: proto::AgentOmpSessionOpenRequest,
) -> ServiceResult<proto::AgentOmpSessionOpenResponse> {
    crate::auth::principal::require_account_device(caller)?;
    let conversation = rpc::find_omp(core, &request.conversation_id)
        .await?
        .ok_or_else(|| ConnectError::new(ErrorCode::NotFound, "OMP conversation not found"))?;
    let worker = core
        .services
        .omp
        .worker_for_session(&conversation.worker_fp)
        .map_err(rpc::unavailable)?;
    if worker.send(CoordWorkerDownstream::OmpOpen(proto::DOmpOpen {
        conversation_id: conversation.id,
        cwd: conversation.cwd,
        acp_session_id: conversation.acp_session_id.unwrap_or_default(),
        ..Default::default()
    })) == 0
    {
        return Err(rpc::unavailable(super::OmpUnavailable::Offline));
    }
    ok_response(proto::AgentOmpSessionOpenResponse::default())
}
