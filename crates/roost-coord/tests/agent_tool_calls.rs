//! Native tool calls wait for offline workers; dispatched calls are never replayed.
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
        checkpoint_id: 42,
    }
}

#[tokio::test]
async fn an_offline_worker_waits_then_dispatches_once_after_reconnect() {
    let fixture = AgentFixture::new("tool-call-offline").await;
    let (output, _chunks) = mpsc::channel(4);
    let services = Arc::clone(&fixture.core.services);
    let mut execute = tokio::spawn(async move {
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
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut execute)
            .await
            .is_err()
    );
    let (_worker, mut downstream) = register_worker(&fixture);
    let frame = tokio::time::timeout(std::time::Duration::from_secs(5), downstream.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(frame, CoordWorkerDownstream::AgentToolCall(call) if call.call_id == "call-1")
    );
    fixture.core.services.agent_tools.receive(
        agent_fixture::WORKER_A,
        roost_protocol::wire::coord_worker::CoordWorkerUpstream::AgentToolResult(
            roost_proto::WAgentToolResult {
                call_id: "call-1".into(),
                content: "saved progress".into(),
                ..Default::default()
            },
        ),
    );
    let result = execute.await.unwrap().unwrap();
    assert_eq!(result.content, "saved progress");
    assert!(downstream.try_recv().is_err(), "no duplicate dispatch");
}

#[tokio::test]
async fn waiting_for_an_offline_worker_remains_cancellable() {
    let fixture = AgentFixture::new("tool-call-offline-abort").await;
    let (output, _chunks) = mpsc::channel(4);
    let cancel = CancellationToken::new();
    let services = Arc::clone(&fixture.core.services);
    let execute_cancel = cancel.clone();
    let mut execute = tokio::spawn(async move {
        services
            .agent_tools
            .execute(agent_fixture::WORKER_A, call_spec(), output, execute_cancel)
            .await
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut execute)
            .await
            .is_err()
    );
    cancel.cancel();
    let outcome = execute.await.unwrap().unwrap();
    assert!(!outcome.is_error);
    let details: serde_json::Value = serde_json::from_str(&outcome.details_json).unwrap();
    assert_eq!(details["cancelled"], true);
    assert_ne!(details["outcome_unknown"], true);
    let (_worker, mut downstream) = register_worker(&fixture);
    assert!(
        downstream.try_recv().is_err(),
        "a cancelled wait must never dispatch"
    );
}

#[tokio::test]
async fn a_dispatched_call_is_rejected_on_disconnect_and_not_replayed_on_reconnect() {
    let fixture = AgentFixture::new("tool-call-disconnect").await;
    let (worker, mut downstream_rx) = register_worker(&fixture);
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
    assert!(matches!(
        &frame,
        CoordWorkerDownstream::AgentToolCall(call) if call.checkpoint_id == 42
    ));
    fixture
        .core
        .services
        .worker_lifecycle
        .closed(&worker, LinkEnd::Closed { replaced: false });
    assert!(execute.await.expect("execute task settled").is_err());
    let (_replacement, mut reconnected) = register_worker(&fixture);
    assert!(
        reconnected.try_recv().is_err(),
        "a dispatched call must not replay"
    );
    assert!(!fixture.core.services.agent_tools.receive(
        agent_fixture::WORKER_A,
        roost_protocol::wire::coord_worker::CoordWorkerUpstream::AgentToolResult(
            roost_proto::WAgentToolResult {
                call_id: "call-1".into(),
                content: "late result from the disconnected dispatch".into(),
                ..Default::default()
            },
        ),
    ));
}

#[tokio::test]
async fn a_dispatched_cancel_is_neutral_but_preserves_uncertainty() {
    let fixture = AgentFixture::new("tool-call-dispatched-abort").await;
    let (_worker, mut downstream) = register_worker(&fixture);
    let (output, _chunks) = mpsc::channel(4);
    let cancel = CancellationToken::new();
    let execute_cancel = cancel.clone();
    let services = Arc::clone(&fixture.core.services);
    let execute = tokio::spawn(async move {
        services
            .agent_tools
            .execute(agent_fixture::WORKER_A, call_spec(), output, execute_cancel)
            .await
    });
    assert!(matches!(
        downstream.recv().await.unwrap(),
        CoordWorkerDownstream::AgentToolCall(_)
    ));
    cancel.cancel();
    let outcome = execute.await.unwrap().unwrap();
    assert!(!outcome.is_error);
    let details: serde_json::Value = serde_json::from_str(&outcome.details_json).unwrap();
    assert_eq!(details["cancelled"], true);
    assert_eq!(details["outcome_unknown"], true);
    assert_eq!(details["timed_out"], false);
    assert!(matches!(
        downstream.recv().await.unwrap(),
        CoordWorkerDownstream::AgentToolCancel(_)
    ));
    assert!(!fixture.core.services.agent_tools.receive(
        agent_fixture::WORKER_A,
        roost_protocol::wire::coord_worker::CoordWorkerUpstream::AgentToolResult(
            roost_proto::WAgentToolResult {
                call_id: "call-1".into(),
                content: "late completion".into(),
                ..Default::default()
            }
        ),
    ));
}

#[tokio::test]
async fn a_dispatched_timeout_retains_warning_and_unknown_outcome() {
    let fixture = AgentFixture::new("tool-call-dispatched-timeout").await;
    let (_worker, mut downstream) = register_worker(&fixture);
    let (output, _chunks) = mpsc::channel(4);
    let services = Arc::clone(&fixture.core.services);
    let execute = tokio::spawn(async move {
        let mut spec = call_spec();
        spec.timeout_ms = 10;
        services
            .agent_tools
            .execute(
                agent_fixture::WORKER_A,
                spec,
                output,
                CancellationToken::new(),
            )
            .await
    });
    assert!(matches!(
        downstream.recv().await.unwrap(),
        CoordWorkerDownstream::AgentToolCall(_)
    ));
    let outcome = execute.await.unwrap().unwrap();
    assert!(outcome.is_error);
    let details: serde_json::Value = serde_json::from_str(&outcome.details_json).unwrap();
    assert_eq!(details["cancelled"], false);
    assert_eq!(details["timed_out"], true);
    assert_eq!(details["outcome_unknown"], true);
    assert!(matches!(
        downstream.recv().await.unwrap(),
        CoordWorkerDownstream::AgentToolCancel(_)
    ));
}

fn register_worker(
    fixture: &AgentFixture,
) -> (
    Arc<WorkerHandle>,
    mpsc::UnboundedReceiver<CoordWorkerDownstream>,
) {
    let (downstream_tx, downstream_rx) = mpsc::unbounded_channel();
    let fp = WorkerFp::try_from(agent_fixture::WORKER_A.to_owned()).expect("worker fingerprint");
    let capabilities = BTreeSet::from([CAPABILITY_AGENT_TOOLS_V1.to_owned()]);
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
    (worker, downstream_rx)
}
