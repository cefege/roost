//! Attachment admission refuses an unknown session without routing to a worker.
//! This preserves the session boundary for uploads.

use super::{accepting_worker, support::Harness};
use connectrpc::ErrorCode;
use roost_coord::attachments::rpc_files::handle_attach_file_chunk;
use roost_proto::AttachFileChunkRequest;

#[tokio::test]
async fn an_unknown_session_is_missing_and_never_reaches_a_worker() {
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
    .expect_err("an unknown session is missing");
    assert_eq!(error.code, ErrorCode::NotFound);
    assert!(sent.lock().unwrap().is_empty());
}
