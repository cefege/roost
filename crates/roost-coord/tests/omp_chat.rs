//! OMP RPC admission, ordered projection and worker acknowledgements.
//! Uses the real migrated coordinator and routable worker handle boundary.
//! No OMP process or provider credentials are needed for durable-state checks.

mod agent_fixture;
mod db_support;
#[path = "omp_chat/ordering.rs"]
mod ordering;
#[path = "omp_chat/respond.rs"]
mod respond;
#[path = "omp_chat/settings_restart.rs"]
mod settings_restart;

use agent_fixture::{AgentFixture, WORKER_A, WORKER_B};
use roost_coord::agent::omp::rpc::handle_agent_omp_session_open;
use roost_coord::{
    agent::{omp, rpc_chat::*, rpc_chat_transcript::*},
    coord_core::{worker_handle::WorkerHandle, worker_lifecycle::WorkerLifecycleObserver},
};
use roost_proto as proto;
use roost_protocol::{
    versioning::{
        CAPABILITY_OMP_ACP_INTERACTIONS_V1, CAPABILITY_OMP_ACP_SESSION_V1, CAPABILITY_OMP_ACP_V1,
    },
    wire::{
        WorkerFp,
        agent_chat::*,
        coord_worker::{CoordWorkerDownstream, CoordWorkerUpstream},
    },
};
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use tokio::sync::mpsc;

fn register_worker(
    fixture: &AgentFixture,
    epoch: &str,
) -> (
    Arc<WorkerHandle>,
    mpsc::UnboundedReceiver<CoordWorkerDownstream>,
) {
    register_worker_with_session_capability(fixture, epoch, true, true)
}
fn register_worker_with_session_capability(
    fixture: &AgentFixture,
    epoch: &str,
    session_open: bool,
    interactions: bool,
) -> (
    Arc<WorkerHandle>,
    mpsc::UnboundedReceiver<CoordWorkerDownstream>,
) {
    let (sender, receiver) = mpsc::unbounded_channel();
    let mut capabilities = BTreeSet::from([CAPABILITY_OMP_ACP_V1.into()]);
    if session_open {
        capabilities.insert(CAPABILITY_OMP_ACP_SESSION_V1.into());
    }
    if interactions {
        capabilities.insert(CAPABILITY_OMP_ACP_INTERACTIONS_V1.into());
    }
    let handle = Arc::new(WorkerHandle::new(
        WorkerFp::try_from(WORKER_A.to_owned()).unwrap(),
        Some(epoch.into()),
        format!("gen-{epoch}"),
        capabilities,
        Arc::new(move |frame| if sender.send(frame).is_ok() { 1 } else { 0 }),
    ));
    handle.mark_ready();
    fixture.core.services.workers.insert(Arc::clone(&handle));
    (handle, receiver)
}
async fn receive(
    receiver: &mut mpsc::UnboundedReceiver<CoordWorkerDownstream>,
) -> CoordWorkerDownstream {
    tokio::time::timeout(Duration::from_secs(5), receiver.recv())
        .await
        .expect("worker frame deadline")
        .expect("worker frame")
}
async fn create(fixture: &AgentFixture) -> String {
    handle_agent_chat_create(
        &fixture.core,
        &fixture.caller,
        proto::AgentChatCreateRequest {
            worker_fp: WORKER_A.into(),
            cwd: "/work".into(),
            backend: "omp".into(),
            ..Default::default()
        },
    )
    .await
    .expect("create OMP")
    .body
    .id
}
async fn submit(fixture: &AgentFixture, id: &str, request: &str) {
    handle_agent_chat_submit(
        &fixture.core,
        &fixture.caller,
        proto::AgentChatSubmitRequest {
            conversation_id: id.into(),
            request_id: request.into(),
            text: "hello".into(),
            when_busy: "follow_up".into(),
            ..Default::default()
        },
    )
    .await
    .expect("submit");
}
async fn snapshot(fixture: &AgentFixture, id: &str) -> (u64, Transcript) {
    let response = handle_agent_chat_snapshot(
        &fixture.core,
        &fixture.caller,
        proto::AgentChatSnapshotRequest {
            conversation_id: id.into(),
            ..Default::default()
        },
    )
    .await
    .expect("snapshot")
    .body;
    (
        response.seq,
        serde_json::from_str(&response.transcript_json).expect("transcript"),
    )
}
fn output(id: &str) -> proto::WOmpEvents {
    let events = [
        OmpWorkerEvent::Update {
            update: serde_json::json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Hel"}}),
        },
        OmpWorkerEvent::Update {
            update: serde_json::json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"lo"}}),
        },
        OmpWorkerEvent::TurnEnded {
            request_id: "req".into(),
            stop_reason: "end_turn".into(),
            error: None,
            outstanding: 0,
        },
    ];
    proto::WOmpEvents {
        conversation_id: id.into(),
        run_id: "r1".into(),
        first_seq: 1,
        events_json: events
            .iter()
            .map(|event| serde_json::to_string(event).unwrap())
            .collect(),
        ..Default::default()
    }
}

#[tokio::test]
async fn omp_chat_submit_projection_replay_and_native_isolation() {
    let fixture = AgentFixture::new("omp-chat").await;
    let (_handle, mut downstream) = register_worker(&fixture, "worker-epoch");
    let native_count = fixture
        .core
        .services
        .agent
        .runtime
        .conversations()
        .await
        .unwrap()
        .len();
    let id = create(&fixture).await;
    let list = handle_agent_chat_list(
        &fixture.core,
        &fixture.caller,
        proto::AgentChatListRequest::default(),
    )
    .await
    .unwrap()
    .body;
    assert!(
        list.conversations
            .iter()
            .any(|conversation| conversation.id == id
                && conversation_from_proto(conversation).backend == AgentBackend::Omp)
    );
    assert_eq!(
        fixture
            .core
            .services
            .agent
            .runtime
            .conversations()
            .await
            .unwrap()
            .len(),
        native_count
    );
    for (scope, model) in [
        (OmpSettingsScope::Defaults, "defaults/model"),
        (
            OmpSettingsScope::Folder {
                worker_fp: WORKER_A.into(),
                cwd: "/work".into(),
            },
            "folder/model",
        ),
    ] {
        omp::store::put_settings(
            &fixture.core.services.db,
            OmpScopedSettings {
                scope,
                values: OmpSettingValues {
                    model: Some(model.into()),
                    thinking_level: None,
                },
            },
        )
        .await
        .unwrap();
    }
    submit(&fixture, &id, "req").await;
    assert!(
        matches!(receive(&mut downstream).await,CoordWorkerDownstream::OmpPrompt(prompt) if prompt.model == "folder/model" && prompt.request_id == "req")
    );
    let (_, transcript) = snapshot(&fixture, &id).await;
    assert_eq!(transcript.run_state, AgentRunState::Running);
    assert!(matches!(&transcript.items[0],TranscriptItem::User {text,..} if text == "hello"));
    submit(&fixture, &id, "req").await;
    assert!(downstream.try_recv().is_err());
    let frame = output(&id);
    fixture
        .core
        .services
        .omp
        .receive(WORKER_A, CoordWorkerUpstream::OmpEvents(frame.clone()));
    assert!(
        matches!(receive(&mut downstream).await,CoordWorkerDownstream::OmpEventsAck(ack) if ack.seq == 3)
    );
    let before = snapshot(&fixture, &id).await;
    assert_eq!(before.1.run_state, AgentRunState::Idle);
    assert!(
        matches!(&before.1.items[1],TranscriptItem::Assistant {blocks,streaming:false,..} if blocks == &vec![TranscriptBlock::Text {text:"Hello".into()}])
    );
    fixture
        .core
        .services
        .omp
        .receive(WORKER_A, CoordWorkerUpstream::OmpEvents(frame.clone()));
    assert!(
        matches!(receive(&mut downstream).await,CoordWorkerDownstream::OmpEventsAck(ack) if ack.seq == 3)
    );
    assert_eq!(snapshot(&fixture, &id).await, before);
    fixture.core.services.omp.receive(
        WORKER_B,
        CoordWorkerUpstream::OmpEvents(proto::WOmpEvents {
            run_id: "foreign".into(),
            ..frame
        }),
    );
    fixture
        .core
        .services
        .omp
        .writers
        .local(&id, vec![], vec![])
        .await
        .unwrap();
    assert_eq!(snapshot(&fixture, &id).await, before);
    assert!(downstream.try_recv().is_err());
}

#[tokio::test]
async fn omp_session_open_reuses_saved_session_without_changing_transcript() {
    let fixture = AgentFixture::new("omp-session-open").await;
    let (_handle, mut downstream) = register_worker(&fixture, "worker-epoch");
    let id = create(&fixture).await;
    sqlx::query("UPDATE omp_sessions SET acp_session_id = $1 WHERE conversation_id = $2")
        .bind("acp-saved")
        .bind(&id)
        .execute(fixture.core.services.db.pool())
        .await
        .unwrap();
    let before = snapshot(&fixture, &id).await;
    assert_eq!(before.1.run_state, AgentRunState::Idle);
    assert!(before.1.items.is_empty());

    handle_agent_omp_session_open(
        &fixture.core,
        &fixture.caller,
        proto::AgentOmpSessionOpenRequest {
            conversation_id: id.clone(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        receive(&mut downstream).await,
        CoordWorkerDownstream::OmpOpen(open)
            if open.conversation_id == id && open.cwd == "/work" && open.acp_session_id == "acp-saved"
    ));
    assert_eq!(snapshot(&fixture, &id).await, before);
}

#[tokio::test]
async fn omp_session_open_requires_updated_worker_before_create_and_preserves_offline_snapshot() {
    let fixture = AgentFixture::new("omp-session-open-errors").await;
    let (_handle, _downstream) =
        register_worker_with_session_capability(&fixture, "old-worker", false, false);
    let error = handle_agent_chat_create(
        &fixture.core,
        &fixture.caller,
        proto::AgentChatCreateRequest {
            worker_fp: WORKER_A.into(),
            cwd: "/work".into(),
            backend: "omp".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, connectrpc::ErrorCode::FailedPrecondition);
    assert_eq!(
        error.message.as_deref(),
        Some("Update Roost on this machine to use OMP chats")
    );
    assert!(
        omp::store::list(&fixture.core.services.db)
            .await
            .unwrap()
            .is_empty()
    );

    let (_handle, mut downstream) = register_worker(&fixture, "new-worker");
    let id = create(&fixture).await;
    submit(&fixture, &id, "durable").await;
    assert!(matches!(
        receive(&mut downstream).await,
        CoordWorkerDownstream::OmpPrompt(prompt) if prompt.request_id == "durable"
    ));
    let before = snapshot(&fixture, &id).await;
    assert_eq!(before.1.run_state, AgentRunState::Running);
    let fp = WorkerFp::try_from(WORKER_A.to_owned()).unwrap();
    fixture.core.services.workers.fence(&fp);
    let error = handle_agent_omp_session_open(
        &fixture.core,
        &fixture.caller,
        proto::AgentOmpSessionOpenRequest {
            conversation_id: id.clone(),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, connectrpc::ErrorCode::Unavailable);
    assert_eq!(snapshot(&fixture, &id).await, before);
    assert!(downstream.try_recv().is_err());
}
