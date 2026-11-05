//! Browser answers to pending host-service prompts, and the worker frame that
//! carries them with the coordinator-stored origin identity.

use super::host_service_store::{self as host_store, PendingKey};
use super::rpc;
use crate::coord_core::CoordCore;
use crate::serve::now_ms;
use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_proto as proto;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use serde::Deserialize;
use serde_json::{Value, json};

const MAX_SERVICE_ID_BYTES: usize = 128;
pub(super) const MAX_REPLY_ERROR_BYTES: usize = 4096;

/// One browser answer to a pending host service.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct HostReply {
    pub service_id: String,
    pub accepted: bool,
    #[serde(default)]
    pub error: String,
}

impl HostReply {
    /// Parse the browser's `host_reply` parameters. Origin identity is never
    /// accepted here; only the pending row may supply it.
    pub(crate) fn parse(value: Value) -> Result<Self, ConnectError> {
        let reply: Self = serde_json::from_value(value)
            .map_err(|error| invalid(&format!("invalid OMP host reply: {error}")))?;
        if reply.service_id.is_empty()
            || reply.service_id.len() > MAX_SERVICE_ID_BYTES
            || !reply
                .service_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(invalid("OMP host reply service id is invalid"));
        }
        if reply.error.len() > MAX_REPLY_ERROR_BYTES {
            return Err(invalid("OMP host reply error exceeds the size limit"));
        }
        Ok(reply)
    }
}

/// Consume one pending host service and forward the browser's answer with the
/// identities the coordinator stored when the effect was raised.
pub(crate) async fn reply(
    core: &CoordCore,
    caller: &crate::coord_core::Caller,
    request: &proto::AgentOmpUiRequest,
    reply: HostReply,
) -> ServiceResult<proto::AgentOmpUiResponse> {
    let fingerprint = caller.fingerprint().to_owned();
    let now = now_ms();
    let current = host_store::current_transport(&core.services.db, &request.conversation_id)
        .await
        .map_err(database_error)?;
    if current.is_empty() || current != request.transport_id {
        return Err(stale());
    }
    let pending = host_store::consume_pending(
        &core.services.db,
        &PendingKey {
            service_id: reply.service_id.as_str(),
            conversation_id: request.conversation_id.as_str(),
            fingerprint: fingerprint.as_str(),
            tab_id: request.tab_id.as_str(),
            view_id: request.view_id.as_str(),
            transport_id: request.transport_id.as_str(),
        },
        now,
    )
    .await
    .map_err(database_error)?
    .ok_or_else(stale)?;
    if !reply.accepted {
        // A dismissed dashboard prompt must not leave a URL that still opens
        // the dashboard, even if the frame was already delivered.
        core.services
            .omp
            .dashboards
            .revoke_service(&reply.service_id);
    }
    let conversation = rpc::find_omp(core, &request.conversation_id)
        .await?
        .ok_or_else(|| ConnectError::new(ErrorCode::NotFound, "OMP conversation not found"))?;
    if conversation.id != pending.conversation_id || conversation.worker_fp != pending.worker_fp {
        return Err(stale());
    }
    let worker = core
        .services
        .omp
        .worker_for_session(&conversation.worker_fp)
        .map_err(rpc::unavailable)?;
    let mut params = serde_json::Map::new();
    params.insert("serviceId".into(), json!(reply.service_id.clone()));
    params.insert("accepted".into(), json!(reply.accepted));
    if !reply.error.is_empty() {
        params.insert("error".into(), json!(reply.error.clone()));
    }
    let action_json = serde_json::to_string(&json!({
        "method": "host_reply",
        "params": Value::Object(params),
    }))
    .map_err(|_| ConnectError::new(ErrorCode::Internal, "OMP host reply encoding failed"))?;
    let frame = CoordWorkerDownstream::OmpUi(proto::DOmpUi {
        conversation_id: conversation.id.clone(),
        request_id: request.request_id.clone(),
        action_json,
        cwd: conversation.cwd,
        acp_session_id: conversation.acp_session_id.unwrap_or_default(),
        client_id: pending.client_id,
        view_id: pending.view_id,
        tab_id: pending.tab_id,
        transport_id: pending.transport_id,
        receipt: false,
        ..Default::default()
    });
    if worker.send(frame) == 0 {
        return Err(rpc::unavailable(super::OmpUnavailable::Offline));
    }
    tracing::info!(conversation_id = %conversation.id, service_id = %reply.service_id,
        accepted = reply.accepted, failed = !reply.error.is_empty(),
        "OMP host reply forwarded");
    crate::rpc::service::ok_response(proto::AgentOmpUiResponse::default())
}

pub(super) fn bounded_error(error: &str) -> String {
    if error.len() <= MAX_REPLY_ERROR_BYTES {
        error.to_owned()
    } else {
        "native OMP action failed".to_owned()
    }
}

fn invalid(message: &str) -> ConnectError {
    ConnectError::new(ErrorCode::InvalidArgument, message)
}

fn stale() -> ConnectError {
    ConnectError::new(
        ErrorCode::FailedPrecondition,
        "OMP host reply is stale or already consumed",
    )
}

fn database_error(error: sqlx::Error) -> ConnectError {
    ConnectError::new(ErrorCode::Internal, error.to_string())
}
