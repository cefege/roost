//! The coordinator task queue's one write a surface makes directly: enqueue a
//! task with its prefill. Called by the task editor through `CoordRpc::call`;
//! the queue itself reaches the browser through Sync, not through a list call.
//! v2 call site: `apps/web/src/components/agents/TaskEditor.tsx`.
//!
//! The payload travels as the JSON document the coordinator stores, which is
//! what the task rows read back — the editor composes it and nothing on the way
//! parses it.

use roost_proto::{Task as PbTask, TasksEnqueueRequest, TasksEnqueueResponse};
use roost_protocol::wire::Task;

use crate::client::rpc::codec::wire_rows::task_from_proto;
use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;

/// `TasksEnqueue`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnqueueTask {
    /// The task document, JSON-encoded.
    pub payload_json: String,
    /// The shell command a completed task has to satisfy, when the caller set one.
    pub completion_check: Option<String>,
}

impl UnaryMethod for EnqueueTask {
    const METHOD: &'static str = "TasksEnqueue";
    type Response = Task;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &TasksEnqueueRequest {
                payload_json: self.payload_json.clone(),
                completion_check: self.completion_check.clone(),
                claim_ttl_ms: None,
                __buffa_unknown_fields: Default::default(),
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<Task, RpcCodecError> {
        let response: TasksEnqueueResponse = decode_message(Self::METHOD, body)?;
        let row: &PbTask = response
            .task
            .as_option()
            .ok_or(RpcCodecError::MalformedResponse {
                method: Self::METHOD,
                detail: "the answer carried no task".to_owned(),
            })?;
        task_from_proto(row).map_err(|error| RpcCodecError::MalformedResponse {
            method: Self::METHOD,
            detail: error.to_string(),
        })
    }
}
