//! Device-authorized inspection for the native agent workspace.
//! Joins runtime state with the live transcript and worker activity registries.
//! The Connect adapter serializes that snapshot for the browser inspector.

use connectrpc::ServiceResult;
use roost_proto as proto;

use super::rpc_errors::{agent_status, internal};
use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::rpc::service::ok_response;

pub async fn handle_agent_chat_inspect(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AgentChatInspectRequest,
) -> ServiceResult<proto::AgentChatInspectResponse> {
    require_account_device(caller)?;
    let mut inspection = core
        .services
        .agent
        .runtime
        .inspect(&request.conversation_id)
        .await
        .map_err(agent_status)?;
    let (_, live_transcript) = core
        .services
        .agent
        .sink
        .snapshot(&request.conversation_id)
        .await
        .map_err(agent_status)?;
    // Journal rows are historical; the folded event snapshot owns foreground tool state.
    inspection
        .activity
        .retain(|activity| activity.kind != "background_job" && activity.kind != "tool");
    let mut live_tool_ids = Vec::new();
    for item in &live_transcript.items {
        if let roost_protocol::wire::agent_chat::TranscriptItem::Tool {
            call_id,
            tool_name,
            running: true,
            queued,
            ..
        } = item
        {
            live_tool_ids.push(call_id.as_str());
            inspection
                .activity
                .push(roost_protocol::wire::agent_chat::AgentActivity {
                    kind: "tool".into(),
                    label: tool_name.clone(),
                    state: if *queued { "queued" } else { "running" }.into(),
                    detail: Some(call_id.clone()),
                });
        }
    }
    inspection.activity.extend(
        core.services
            .agent_tools
            .running_jobs_for(&request.conversation_id)
            .into_iter()
            .map(|job| roost_protocol::wire::agent_chat::AgentActivity {
                kind: "background_job".into(),
                label: "Background job".into(),
                state: "running".into(),
                detail: Some(format!("{} · {}", job.key.job_id, job.worker_label)),
            }),
    );
    inspection.activity.extend(
        core.services
            .agent_tools
            .active_tool_calls_for(&request.conversation_id)
            .into_iter()
            .filter(|call| !live_tool_ids.contains(&call.call_id.as_str()))
            .map(|call| roost_protocol::wire::agent_chat::AgentActivity {
                kind: "tool".into(),
                label: call.tool_name,
                state: call.state.into(),
                detail: Some(format!("{} · {}", call.call_id, call.worker_label)),
            }),
    );
    let inspection_json = serde_json::to_string(&inspection)
        .map_err(|_| internal("agent inspection did not serialize"))?;
    ok_response(proto::AgentChatInspectResponse {
        inspection_json,
        ..Default::default()
    })
}
