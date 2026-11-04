//! Production archive admission and retained native-history behavior.
//! Seeds a journal with an explicit native fixture, then uses the production service.
//! Exercises real chat RPC handlers and journal-backed transcript reads.

use std::sync::Arc;

use roost_agent::{AgentStore, Entry, NewConversation};
use roost_coord::agent::rpc_chat::{
    handle_agent_chat_create, handle_agent_chat_delete, handle_agent_chat_list,
};
use roost_coord::agent::rpc_chat_transcript::handle_agent_chat_snapshot;
use roost_coord::coord_core::CoordCore;
use roost_coord::services::CoordServices;
use roost_proto as proto;
use serde_json::Value;

use super::{WORKER_A, setup};

#[tokio::test]
async fn production_archive_rejects_native_delete_and_preserves_transcript() {
    let setup = setup("agent-native-archive").await;
    let native = Arc::clone(&setup.fixture.core.services.agent);
    let summary = native
        .native_runtime()
        .unwrap()
        .create_conversation(NewConversation {
            worker_fp: WORKER_A.to_owned(),
            worker_label: "worker-a".to_owned(),
            worker_os: "linux".to_owned(),
            cwd: "/repo".to_owned(),
            title: Some("retained native history".to_owned()),
            model: None,
            thinking_level: None,
        })
        .await
        .unwrap();
    let store =
        roost_coord::agent::store::CoordAgentStore::new(setup.fixture.core.services.db.clone());
    store
        .append_entry(
            &summary.id,
            &Entry::User {
                text: "retained".to_owned(),
                attachments: Vec::new(),
                loop_repeat: false,
                item_id: Some("kept-item".to_owned()),
            },
            &[],
            None,
        )
        .await
        .unwrap();
    native.leave_cluster().await;
    store
        .set_schedule(
            &summary.id,
            &roost_agent::schedules::ScheduleRow {
                id: "retained-schedule".to_owned(),
                text: "retained reminder".to_owned(),
                due_ms: 2_000_000_000_000,
                every_ms: None,
                fired: 0,
                created_ms: 1,
            },
        )
        .await
        .unwrap();

    let core = CoordCore::new(Arc::new(CoordServices::new(
        setup.fixture.core.services.db.clone(),
    )));
    let create_error = handle_agent_chat_create(
        &core,
        &setup.fixture.caller,
        proto::AgentChatCreateRequest {
            worker_fp: WORKER_A.to_owned(),
            backend: "native".to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect_err("production native creation was accepted");
    assert_eq!(create_error.code, connectrpc::ErrorCode::FailedPrecondition);
    let listed = handle_agent_chat_list(
        &core,
        &setup.fixture.caller,
        proto::AgentChatListRequest::default(),
    )
    .await
    .expect("archived conversation metadata")
    .body;
    assert!(
        listed
            .conversations
            .iter()
            .any(|conversation| conversation.id == summary.id && conversation.backend == "native"),
        "legacy native links need their retained backend metadata"
    );

    let error = handle_agent_chat_delete(
        &core,
        &setup.fixture.caller,
        proto::AgentChatDeleteRequest {
            conversation_id: summary.id.clone(),
            ..Default::default()
        },
    )
    .await
    .expect_err("archived native delete was accepted");
    assert_eq!(error.code, connectrpc::ErrorCode::FailedPrecondition);

    let snapshot = handle_agent_chat_snapshot(
        &core,
        &setup.fixture.caller,
        proto::AgentChatSnapshotRequest {
            conversation_id: summary.id,
            ..Default::default()
        },
    )
    .await
    .expect("retained native snapshot")
    .body;
    let transcript: Value = serde_json::from_str(&snapshot.transcript_json).unwrap();
    assert_eq!(transcript["items"][0]["text"], "retained");
    assert_eq!(transcript["schedules"][0]["id"], "retained-schedule");
    assert_eq!(transcript["schedules"][0]["text"], "retained reminder");
}
