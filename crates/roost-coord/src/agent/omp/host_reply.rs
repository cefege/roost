//! Browser answers to pending host-service prompts, and the worker frame that
//! carries them with the coordinator-stored origin identity.

use super::host_service_store::{self as host_store, PendingKey};
use super::rpc;
use crate::coord_core::CoordCore;
use crate::serve::now_ms;
use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_proto as proto;
use roost_protocol::wire::agent_chat::MAX_OMP_LIVE_AUDIO_BYTES;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use serde::Deserialize;
use serde_json::{Value, json};

const MAX_SERVICE_ID_BYTES: usize = 128;
pub(super) const MAX_REPLY_ERROR_BYTES: usize = 4096;

/// One browser answer to a pending host service.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct HostReply {
    pub service_id: String,
    pub accepted: bool,
    #[serde(default)]
    pub error: String,
    #[serde(default, deserialize_with = "reply_value")]
    pub value: Option<String>,
}

impl HostReply {
    /// Parse the browser's `host_reply` parameters. Origin identity is never
    /// accepted here; only the pending row may supply it.
    pub(crate) fn parse(value: Value) -> Result<Self, ConnectError> {
        let reply: Self =
            serde_json::from_value(value).map_err(|_| invalid("invalid OMP host reply"))?;
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
        if reply.value.as_ref().is_some_and(|value| {
            !reply.accepted
                || value.trim().is_empty()
                || value.len() > MAX_OMP_LIVE_AUDIO_BYTES
                || value.contains('\0')
        }) {
            return Err(invalid("invalid OMP host reply value"));
        }
        Ok(reply)
    }
}

fn reply_value<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
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
    // The worker owns the original operation; only its prepare may return SDP.
    if reply.value.is_some() && pending.kind != "live_audio" {
        return Err(invalid("this OMP host service cannot return a value"));
    }
    if !reply.accepted {
        // A dismissed dashboard prompt must not leave a URL that still opens
        // the dashboard, even if the frame was already delivered.
        core.services
            .omp
            .dashboards
            .revoke_service(&reply.service_id);
    }
    if pending.kind == "open_fork" {
        super::host_services::fork::finish(
            core,
            &request.conversation_id,
            &reply.service_id,
            reply.accepted,
        )
        .await
        .map_err(database_error)?;
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
    if let Some(value) = reply.value {
        params.insert("value".into(), json!(value));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_reply_values_are_optional_but_strict_and_bounded() {
        assert!(HostReply::parse(json!({"serviceId":"s","accepted":true})).is_ok());
        assert!(
            HostReply::parse(json!({
                "serviceId":"s","accepted":true,"value":"v=0\r\n",
            }))
            .is_ok()
        );
        for value in [Value::Null, json!(false), json!(""), json!(" \n")] {
            assert!(
                HostReply::parse(json!({
                    "serviceId":"s","accepted":true,"value":value,
                }))
                .is_err()
            );
        }
        assert!(
            HostReply::parse(json!({
                "serviceId":"s","accepted":false,"error":"denied","value":"v=0\r\n",
            }))
            .is_err()
        );
        assert!(
            HostReply::parse(json!({
                "serviceId":"s","accepted":true,
                "value":"s".repeat(MAX_OMP_LIVE_AUDIO_BYTES + 1),
            }))
            .is_err()
        );
        assert!(
            HostReply::parse(json!({
                "serviceId":"s","accepted":true,"peerId":"forged",
            }))
            .is_err()
        );
    }
}
