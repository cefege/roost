//! Read-only agent policy does not block hidden controller operations.
//! The real registry must still dispatch repository instructions and explicit file rewind.
//! Parent fixtures capture worker frames and complete their actual registry waiters.

use super::{AgentFixture, AgentToolCallSpec, call_spec, register_worker};
use roost_protocol::wire::agent_chat::{TOOL_CONTEXT_FILES, TOOL_REWIND_FILES};
use roost_protocol::wire::coord_worker::{CoordWorkerDownstream, CoordWorkerUpstream};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn read_only_keeps_controller_context_and_explicit_rewind_available() {
    let fixture = AgentFixture::new("readonly-controller").await;
    let template = call_spec(&fixture).await;
    fixture
        .core
        .services
        .agent
        .runtime
        .set_read_only(&template.owner_conversation_id.0, true)
        .await
        .unwrap();
    let (_worker, mut downstream) = register_worker(&fixture);
    for tool in [TOOL_CONTEXT_FILES, TOOL_REWIND_FILES] {
        let spec = AgentToolCallSpec {
            conversation_id: template.conversation_id.clone(),
            owner_conversation_id: template.owner_conversation_id.clone(),
            call_id: format!("controller-{tool}"),
            cwd: template.cwd.clone(),
            tool: tool.to_owned(),
            args_json: if tool == TOOL_CONTEXT_FILES {
                "{}"
            } else {
                r#"{"checkpoint_id":42}"#
            }
            .into(),
            timeout_ms: 5_000,
            checkpoint_id: 42,
        };
        let call_id = spec.call_id.clone();
        let services = Arc::clone(&fixture.core.services);
        let (output, _chunks) = mpsc::channel(4);
        let execute = tokio::spawn(async move {
            services
                .agent_tools
                .execute(
                    super::agent_fixture::WORKER_A,
                    spec,
                    output,
                    CancellationToken::new(),
                )
                .await
        });
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), downstream.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(frame, CoordWorkerDownstream::AgentToolCall(call)
            if call.call_id == call_id && call.tool == tool));
        fixture.core.services.agent_tools.receive(
            super::agent_fixture::WORKER_A,
            CoordWorkerUpstream::AgentToolResult(roost_proto::WAgentToolResult {
                call_id,
                content: "controller operation completed".into(),
                ..Default::default()
            }),
        );
        let result = execute.await.unwrap().unwrap();
        assert!(!result.is_error);
    }
}
