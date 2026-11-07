//! Attachment admission against a real migrated coordinator: an OMP
//! conversation resolves to the worker that owns it, and an unknown id stays
//! missing rather than reaching any worker.

use super::{WORKER_FP, accepting_worker, support::Harness};
use connectrpc::ErrorCode;
use roost_coord::agent::omp::store::{OmpNew, create};
use roost_coord::attachments::rpc_files::handle_attach_file_chunk;
use roost_proto::AttachFileChunkRequest;
use serde_json::json;

#[tokio::test]
async fn an_omp_conversation_routes_its_upload_to_the_owning_worker() {
    let harness = Harness::new("files-omp-target").await;
    let conversation = create(
        &harness.core.services.db,
        OmpNew {
            worker_fp: WORKER_FP.to_owned(),
            worker_label: "desk".to_owned(),
            worker_os: "linux".to_owned(),
            cwd: "/tmp/project".to_owned(),
        },
    )
    .await
    .expect("an OMP conversation");
    let sent = accepting_worker(
        &harness,
        "omp-target-worker",
        Some(json!({"abs_path": "/tmp/project/photo.png"})),
    );
    handle_attach_file_chunk(
        &harness.core,
        &harness.caller(),
        AttachFileChunkRequest {
            upload_id: "omp-upload".to_owned(),
            session_id: conversation.id.clone(),
            filename: "photo.png".to_owned(),
            data: vec![7, 8, 9],
            last: true,
            seq: 0,
            ..Default::default()
        },
    )
    .await
    .expect("the upload reaches the owning worker");
    assert!(!sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_unknown_conversation_is_missing_and_never_reaches_a_worker() {
    let harness = Harness::new("files-unknown-target").await;
    let sent = accepting_worker(&harness, "unknown-target-worker", None);
    let error = handle_attach_file_chunk(
        &harness.core,
        &harness.caller(),
        AttachFileChunkRequest {
            upload_id: "unknown-upload".to_owned(),
            session_id: "c-ffffffffffffffffffffffff".to_owned(),
            filename: "photo.png".to_owned(),
            data: vec![7],
            last: true,
            seq: 0,
            ..Default::default()
        },
    )
    .await
    .expect_err("an unknown conversation is missing");
    assert_eq!(error.code, ErrorCode::NotFound);
    assert!(sent.lock().unwrap().is_empty());
}
