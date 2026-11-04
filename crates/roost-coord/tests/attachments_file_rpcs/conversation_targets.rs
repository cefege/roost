//! Archived native attachment admission against a real migrated coordinator.
//! Seeds retained native data with an explicit fixture; production must not
//! dispatch its upload or change that conversation.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::{WORKER_FP, accepting_worker, support::Harness};
use connectrpc::ErrorCode;
use roost_agent::{AgentStore, NewConversation};
use roost_coord::agent::{AgentService, ClusterMembership};
use roost_coord::attachments::rpc_files::handle_attach_file_chunk;
use roost_proto::AttachFileChunkRequest;
use serde_json::json;

#[tokio::test]
async fn archived_native_upload_is_refused_without_changing_conversation() {
    let harness = Harness::new("files-native-archived").await;
    let services = &harness.core.services;
    let native = AgentService::with_worker_tools(
        services.db.clone(),
        Arc::clone(&services.buses),
        Arc::new(roost_coord::agent::tool_calls::ToolCallRegistry::new(
            Arc::clone(&services.workers),
            services.db.clone(),
        )),
        BTreeMap::new(),
        ClusterMembership {
            instance_id: services.instance_id.clone(),
            notifier: Arc::clone(&services.cluster),
        },
    );
    let conversation = native
        .native_runtime()
        .unwrap()
        .create_conversation(NewConversation {
            worker_fp: WORKER_FP.to_owned(),
            worker_label: "desk".to_owned(),
            worker_os: "linux".to_owned(),
            cwd: "/tmp/project".to_owned(),
            title: Some("retained native attachment target".to_owned()),
            model: None,
            thinking_level: None,
        })
        .await
        .expect("legacy native conversation");
    let store = roost_coord::agent::store::CoordAgentStore::new(services.db.clone());
    let before = store.conversation(&conversation.id).await.unwrap();
    let sent = accepting_worker(
        &harness,
        "native-archived-worker",
        Some(json!({"abs_path": "/unexpected-native-upload"})),
    );
    let error = handle_attach_file_chunk(
        &harness.core,
        &harness.caller(),
        AttachFileChunkRequest {
            upload_id: "archived-upload".to_owned(),
            session_id: conversation.id.clone(),
            filename: "photo.png".to_owned(),
            data: vec![7, 8, 9],
            last: true,
            seq: 0,
            ..Default::default()
        },
    )
    .await
    .expect_err("an archived native upload must not reach a worker");
    assert_eq!(error.code, ErrorCode::FailedPrecondition);
    let unknown = handle_attach_file_chunk(
        &harness.core,
        &harness.caller(),
        AttachFileChunkRequest {
            upload_id: "unknown-upload".to_owned(),
            session_id: "c-ffffffffffffffffffffffff".to_owned(),
            filename: "photo.png".to_owned(),
            data: vec![7],
            last: true,
            ..Default::default()
        },
    )
    .await
    .expect_err("unknown conversations remain missing, not archived");
    assert_eq!(unknown.code, ErrorCode::NotFound);
    assert!(sent.lock().unwrap().is_empty());
    assert_eq!(store.conversation(&conversation.id).await.unwrap(), before);
    native.leave_cluster().await;
}
