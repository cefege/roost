//! Registry policy checks use the persisted owner while preserving worker namespaces.

use super::{AgentFixture, call_spec, register_worker};
use roost_agent::WorkerConversationNamespace;
use roost_protocol::wire::coord_worker::{CoordWorkerDownstream, CoordWorkerUpstream};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn readonly_policy_uses_owner_and_keeps_advisor_and_loop_worker_namespaces() {
    let fixture = AgentFixture::new("tool-call-owned-policy").await;
    let template = call_spec(&fixture).await;
    let owner = template.owner_conversation_id.0.clone();
    fixture
        .core
        .services
        .agent
        .runtime
        .set_read_only(&owner, true)
        .await
        .expect("read-only setting persists");
    let (_worker, mut downstream) = register_worker(&fixture);

    let mut mutation = template.clone();
    mutation.call_id = "loop-mutation".to_owned();
    mutation.conversation_id = WorkerConversationNamespace(format!("{owner}-loop"));
    mutation.tool = "bash".to_owned();
    mutation.args_json = r#"{"command":"touch forbidden"}"#.to_owned();
    let (output, _chunks) = mpsc::channel(4);
    let denied = fixture
        .core
        .services
        .agent_tools
        .execute(
            super::agent_fixture::WORKER_A,
            mutation,
            output,
            CancellationToken::new(),
        )
        .await
        .expect("policy denial is a tool outcome");
    assert!(denied.is_error);
    assert!(denied.content.contains("disabled in read-only mode"));
    assert!(
        downstream.try_recv().is_err(),
        "mutation never reaches worker"
    );

    let mut read = template;
    read.call_id = "advisor-read".to_owned();
    read.conversation_id = WorkerConversationNamespace(format!("{owner}-advisor"));
    let worker_namespace = read.conversation_id.0.clone();
    let services = Arc::clone(&fixture.core.services);
    let (output, _chunks) = mpsc::channel(4);
    let execute = tokio::spawn(async move {
        services
            .agent_tools
            .execute(
                super::agent_fixture::WORKER_A,
                read,
                output,
                CancellationToken::new(),
            )
            .await
    });
    let frame = tokio::time::timeout(std::time::Duration::from_secs(5), downstream.recv())
        .await
        .expect("read call dispatches")
        .expect("worker frame");
    assert!(matches!(frame, CoordWorkerDownstream::AgentToolCall(call)
        if call.conversation_id == worker_namespace));
    fixture.core.services.agent_tools.receive(
        super::agent_fixture::WORKER_A,
        CoordWorkerUpstream::AgentToolResult(roost_proto::WAgentToolResult {
            call_id: "advisor-read".into(),
            content: "read completed".into(),
            ..Default::default()
        }),
    );
    let outcome = execute
        .await
        .expect("read execution joins")
        .expect("read succeeds");
    assert!(!outcome.is_error);
    assert_eq!(outcome.content, "read completed");
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_during_policy_lookup_never_dispatches_a_worker_frame() {
    let fixture = AgentFixture::new("tool-call-policy-cancel").await;
    let mut spec = call_spec(&fixture).await;
    spec.tool = "bash".to_owned();
    spec.args_json = r#"{"command":"touch forbidden"}"#.to_owned();
    let (_worker, mut downstream) = register_worker(&fixture);
    let mut held_connections = super::db_support::hold_every_connection(fixture.database()).await;
    let cancel = CancellationToken::new();
    let execute_cancel = cancel.clone();
    let services = Arc::clone(&fixture.core.services);
    let (output, _chunks) = mpsc::channel(4);
    let mut execute = tokio::spawn(async move {
        services
            .agent_tools
            .execute(super::agent_fixture::WORKER_A, spec, output, execute_cancel)
            .await
    });
    tokio::task::yield_now().await;
    let policy_gate_pool = fixture.database().pool().clone();
    let policy_gate_waiter = tokio::spawn(async move {
        policy_gate_pool
            .acquire()
            .await
            .expect("policy gate connection")
    });
    tokio::task::yield_now().await;
    drop(held_connections.pop());
    let policy_gate = tokio::time::timeout(std::time::Duration::from_secs(5), policy_gate_waiter)
        .await
        .expect("policy gate acquires after worker label lookup")
        .expect("policy gate task joins");
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut execute)
            .await
            .is_err(),
        "policy lookup remains blocked on the held database connection"
    );
    cancel.cancel();
    drop(policy_gate);
    drop(held_connections);

    let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), execute)
        .await
        .expect("cancelled policy wait completes")
        .expect("execution joins")
        .expect("cancellation is an outcome");
    assert!(!outcome.is_error);
    assert_eq!(outcome.content, "Cancelled before dispatch.");
    let details: serde_json::Value =
        serde_json::from_str(&outcome.details_json).expect("cancel details");
    assert_eq!(details["cancelled"], true);
    assert!(
        downstream.try_recv().is_err(),
        "cancelled call never reaches worker"
    );
}
