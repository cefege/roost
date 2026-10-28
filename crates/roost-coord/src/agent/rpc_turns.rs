//! Device-authorized actions on individual agent transcript turns.
//!
//! Answers, rewinds and forks are delegated to the shared agent runtime; this
//! module translates the coordinator's JSON and protobuf boundary types. Each
//! is applied once per client `request_id`; a retry returns the first result.

use std::collections::BTreeMap;

use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_proto as proto;
use roost_protocol::wire::agent_chat::{ConversationSummary, conversation_to_proto};

use super::rpc_chat::require_request_id;
use super::rpc_errors::agent_status;
use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::rpc::service::ok_response;

pub async fn handle_agent_chat_answer(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatAnswerRequest,
) -> ServiceResult<proto::AgentChatAnswerResponse> {
    require_account_device(caller)?;
    require_request_id(&request.request_id)?;
    let answers: BTreeMap<String, Vec<String>> = serde_json::from_str(&request.answers_json)
        .map_err(|error| {
            ConnectError::new(ErrorCode::InvalidArgument, format!("answers_json: {error}"))
        })?;
    let runtime = &core.services.agent.runtime;
    runtime
        .deduplicated(
            &request.conversation_id,
            &request.request_id,
            "answer",
            || async {
                runtime
                    .answer(&request.conversation_id, &request.item_id, answers)
                    .await?;
                Ok(String::new())
            },
        )
        .await
        .map_err(agent_status)?;
    ok_response(proto::AgentChatAnswerResponse::default())
}

pub async fn handle_agent_chat_rewind(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatRewindRequest,
) -> ServiceResult<proto::AgentChatRewindResponse> {
    require_account_device(caller)?;
    require_request_id(&request.request_id)?;
    let runtime = &core.services.agent.runtime;
    let text = runtime
        .deduplicated(
            &request.conversation_id,
            &request.request_id,
            "rewind",
            || {
                runtime.rewind(
                    &request.conversation_id,
                    &request.item_id,
                    request.restore_files,
                )
            },
        )
        .await
        .map_err(agent_status)?;
    let Some(text) = text else {
        return ok_response(proto::AgentChatRewindResponse::default());
    };
    let original = runtime
        .original_user_message(&request.conversation_id, &request.item_id)
        .await
        .map_err(agent_status)?;
    ok_response(proto::AgentChatRewindResponse {
        text,
        attachments: original
            .attachments
            .into_iter()
            .map(|attachment| proto::AgentChatAttachment {
                filename: attachment.filename,
                mime_type: attachment.mime_type,
                data: attachment.data,
                path: attachment.path,
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    })
}

pub async fn handle_agent_chat_fork(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatForkRequest,
) -> ServiceResult<proto::AgentConversation> {
    require_account_device(caller)?;
    require_request_id(&request.request_id)?;
    let item_id = (!request.item_id.is_empty()).then_some(request.item_id.as_str());
    let runtime = &core.services.agent.runtime;
    let forked_id = runtime
        .deduplicated(
            &request.conversation_id,
            &request.request_id,
            "fork",
            || async { Ok(runtime.fork(&request.conversation_id, item_id).await?.id) },
        )
        .await
        .map_err(agent_status)?
        .ok_or_else(|| {
            ConnectError::new(ErrorCode::Unavailable, "this fork is still being created")
        })?;
    let conversation: ConversationSummary = runtime
        .conversation(&forked_id)
        .await
        .map_err(agent_status)?;
    ok_response(conversation_to_proto(&conversation))
}

pub const METHOD_HANDLERS: &[(&str, &str)] = &[
    (
        "AgentChatAnswer",
        "agent::rpc_turns::handle_agent_chat_answer",
    ),
    (
        "AgentChatRewind",
        "agent::rpc_turns::handle_agent_chat_rewind",
    ),
    ("AgentChatFork", "agent::rpc_turns::handle_agent_chat_fork"),
];
