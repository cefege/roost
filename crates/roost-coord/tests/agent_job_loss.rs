//! A background job whose worker link drops is reported to its conversation as failed.
//!
//! The fake worker reports the bash call as running in the background, then the
//! link closes; the coordinator must deliver the lost job into the transcript.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_fixture;
mod db_support;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_fixture::AgentFixture;
use roost_agent::{AgentRuntime, NewConversation};
use roost_coord::agent::tool_calls::AgentToolCallSpec;
use roost_coord::coord_core::worker_handle::WorkerHandle;
use roost_coord::coord_core::worker_lifecycle::LinkEnd;
use roost_protocol::versioning::CAPABILITY_AGENT_TOOLS_V1;
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::agent_chat::{JobState, TranscriptItem, async_details_json};
use roost_protocol::wire::coord_worker::{CoordWorkerDownstream, CoordWorkerUpstream};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn a_lost_background_job_is_delivered_to_its_conversation_as_failed() {
    let fixture = AgentFixture::new("agent-job-loss").await;
    let runtime: AgentRuntime = fixture.core.services.agent.runtime.clone();
    let conversation = runtime
        .create_conversation(NewConversation {
            worker_fp: agent_fixture::WORKER_A.to_owned(),
            worker_label: "worker-a".to_owned(),
            worker_os: "linux".to_owned(),
            cwd: "/tmp/project".to_owned(),
            title: Some("job loss".to_owned()),
            model: None,
            thinking_level: None,
        })
        .await
        .expect("conversation created");
    let conversation_id = conversation.id.clone();

    let (worker, mut downstream_rx) = register_worker(&fixture);
    let spec = AgentToolCallSpec {
        call_id: "call-bg".to_owned(),
        conversation_id: conversation_id.clone(),
        cwd: "/tmp/project".to_owned(),
        tool: "bash".to_owned(),
        args_json: r#"{"command":"sleep 600"}"#.to_owned(),
        timeout_ms: 5_000,
        checkpoint_id: 7,
    };
    let (output, _chunks) = mpsc::channel(4);
    let services = Arc::clone(&fixture.core.services);
    let execute = tokio::spawn(async move {
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
        downstream_rx.recv().await,
        Some(CoordWorkerDownstream::AgentToolCall(_))
    ));
    assert!(fixture.core.services.agent_tools.receive(
        agent_fixture::WORKER_A,
        CoordWorkerUpstream::AgentToolResult(roost_proto::WAgentToolResult {
            call_id: "call-bg".into(),
            content: "started".into(),
            details_json: async_details_json(JobState::Running, "bg_1"),
            ..Default::default()
        }),
    ));
    execute.await.expect("execute task settled").ok();

    fixture
        .core
        .services
        .worker_lifecycle
        .closed(&worker, LinkEnd::Closed { replaced: false });

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let transcript = runtime.transcript(&conversation_id).await.unwrap();
        let failed = transcript.items.iter().any(|item| {
            matches!(
                item,
                TranscriptItem::BackgroundJob { job_id, state: JobState::Failed, .. }
                    if job_id == "bg_1"
            )
        });
        if failed {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "lost job never reached the transcript: {:?}",
            transcript.items
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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
