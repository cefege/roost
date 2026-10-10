//! Device-authorized actions on individual agent transcript turns.
//!
//! Answers, rewinds and forks are delegated to the shared agent runtime; this
//! module translates the coordinator's JSON and protobuf boundary types.

use std::collections::BTreeMap;

use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_proto as proto;
use roost_protocol::wire::agent_chat::conversation_to_proto;

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
    let answers: BTreeMap<String, Vec<String>> = serde_json::from_str(&request.answers_json)
        .map_err(|error| {
            ConnectError::new(ErrorCode::InvalidArgument, format!("answers_json: {error}"))
        })?;
    core.services
        .agent
        .runtime
        .answer(&request.conversation_id, &request.item_id, answers)
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
    let text = core
        .services
        .agent
        .runtime
        .rewind(
            &request.conversation_id,
            &request.item_id,
            request.restore_files,
        )
        .await
        .map_err(agent_status)?;
    ok_response(proto::AgentChatRewindResponse {
        text,
        ..Default::default()
    })
}

pub async fn handle_agent_chat_fork(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatForkRequest,
) -> ServiceResult<proto::AgentConversation> {
    require_account_device(caller)?;
    let item_id = (!request.item_id.is_empty()).then_some(request.item_id.as_str());
    let conversation = core
        .services
        .agent
        .runtime
        .fork(&request.conversation_id, item_id)
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
