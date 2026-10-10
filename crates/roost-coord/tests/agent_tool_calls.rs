//! Native tool calls reject offline workers and settle immediately when their link generation ends.
//!
//! A fake worker socket captures the coordinator's downstream frame and lifecycle transitions.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_fixture;
mod db_support;

use std::collections::BTreeSet;
use std::sync::Arc;

use agent_fixture::AgentFixture;
use roost_coord::agent::tool_calls::AgentToolCallSpec;
use roost_coord::coord_core::worker_handle::WorkerHandle;
use roost_coord::coord_core::worker_lifecycle::LinkEnd;
use roost_protocol::versioning::CAPABILITY_AGENT_TOOLS_V1;
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn call_spec() -> AgentToolCallSpec {
    AgentToolCallSpec {
        call_id: "call-1".to_owned(),
        conversation_id: "conversation-1".to_owned(),
        cwd: "/tmp/project".to_owned(),
        tool: "read".to_owned(),
        args_json: r#"{"path":"README.md"}"#.to_owned(),
        timeout_ms: 5_000,
    }
}

#[tokio::test]
async fn offline_worker_returns_machine_label() {
    let fixture = AgentFixture::new("tool-call-offline").await;
    let (output, _chunks) = mpsc::channel(4);
    let error = fixture
        .core
        .services
        .agent_tools
        .execute(
            agent_fixture::WORKER_A,
            call_spec(),
            output,
            CancellationToken::new(),
        )
        .await
        .expect_err("an offline worker cannot receive a tool call");
    assert_eq!(error, "machine agent-worker is offline");
}

#[tokio::test]
async fn worker_link_close_fails_pending_call_as_disconnected() {
    let fixture = AgentFixture::new("tool-call-disconnect").await;
    let (downstream_tx, mut downstream_rx) = mpsc::unbounded_channel();
    let fp = WorkerFp::try_from(agent_fixture::WORKER_A.to_owned()).expect("worker fingerprint");
    let mut capabilities = BTreeSet::new();
    capabilities.insert(CAPABILITY_AGENT_TOOLS_V1.to_owned());
    let send = Arc::new(move |frame| {
        let _ = downstream_tx.send(frame);
        1
    });
    let worker = Arc::new(WorkerHandle::new(
        fp,
        Some("worker-epoch".into()),
        "generation-1".into(),
        capabilities,
        send,
    ));
    worker.mark_ready();
    fixture.core.services.workers.insert(Arc::clone(&worker));
    let (output, _chunks) = mpsc::channel(4);
    let services = Arc::clone(&fixture.core.services);
    let execute = tokio::spawn(async move {
        services
            .agent_tools
            .execute(
                agent_fixture::WORKER_A,
                call_spec(),
                output,
                CancellationToken::new(),
            )
            .await
    });
    let frame = downstream_rx
        .recv()
        .await
        .expect("tool call reached fake worker");
    assert!(matches!(frame, CoordWorkerDownstream::AgentToolCall(_)));
    fixture
        .core
        .services
        .worker_lifecycle
        .closed(&worker, LinkEnd::Closed { replaced: false });
    let error = execute
        .await
        .expect("execute task settled")
        .expect_err("closed link rejects pending call");
    assert_eq!(error, "machine agent-worker disconnected");
}
