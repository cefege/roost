use super::{WORKER_FP, accepting_worker, frame_of, support::Harness};
use connectrpc::ErrorCode;
use roost_coord::attachments::rpc_files::{
    handle_attach_file_chunk, handle_attachment_probe, handle_delete_attachment,
    handle_list_attachments,
};
use roost_proto::{
    AttachFileChunkRequest, AttachmentProbeRequest, DeleteAttachmentRequest, ListAttachmentsRequest,
};
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use serde_json::json;

#[tokio::test]
async fn conversation_ids_relay_upload_probe_list_delete_and_refuse_unknown_ids() {
    let harness = Harness::new("files-agent-attachment").await;
    let conversation = harness
        .core
        .services
        .agent
        .runtime
        .create_conversation(roost_agent::NewConversation {
            worker_fp: WORKER_FP.to_owned(),
            worker_label: "desk".to_owned(),
            worker_os: "linux".to_owned(),
            cwd: "/tmp/project".to_owned(),
            title: Some("attachment target".to_owned()),
            model: None,
            thinking_level: None,
        })
        .await
        .expect("conversation created");
    assert!(conversation.id.starts_with("c-"));
    let sent = accepting_worker(
        &harness,
        "gen-agent-attachment",
        Some(json!({
            "abs_path": "/att/photo.png",
            "hit": true,
            "entries": [],
            "ok": true
        })),
    );

    let upload = handle_attach_file_chunk(
        &harness.core,
        &harness.caller(),
        AttachFileChunkRequest {
            upload_id: "agent-upload".to_owned(),
            session_id: conversation.id.clone(),
            filename: "photo.png".to_owned(),
            data: vec![7, 8, 9],
            last: true,
            seq: 0,
            ..Default::default()
        },
    )
    .await
    .expect("conversation upload relayed");
    assert_eq!(upload.body.abs_path, "/att/photo.png");

    let probe = handle_attachment_probe(
        &harness.core,
        &harness.caller(),
        AttachmentProbeRequest {
            session_id: conversation.id.clone(),
            sha256: "a".repeat(64),
            size: 3,
            short_path: false,
            ..Default::default()
        },
    )
    .await
    .expect("conversation probe relayed");
    assert!(probe.body.hit);
    let listing = handle_list_attachments(
        &harness.core,
        &harness.caller(),
        ListAttachmentsRequest {
            session_id: conversation.id.clone(),
            ..Default::default()
        },
    )
    .await
    .expect("conversation attachment query relayed");
    assert!(listing.body.entries.is_empty());
    let deleted = handle_delete_attachment(
        &harness.core,
        &harness.caller(),
        DeleteAttachmentRequest {
            session_id: conversation.id.clone(),
            filename: "photo.png".to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect("conversation attachment delete relayed");
    assert!(deleted.body.ok);

    {
        let sent_frames = sent.lock().unwrap();
        assert!(matches!(
            &sent_frames[0],
            CoordWorkerDownstream::AttachmentChunk(chunk)
                if chunk.session_id == conversation.id
        ));
        for (idx, expected_kind) in [
            (1, "attachment-probe"),
            (2, "list-attachments"),
            (3, "delete-attachment"),
        ] {
            let (_, frame, _, _) = frame_of(&sent_frames[idx]);
            assert_eq!(frame.kind(), expected_kind);
        }
        assert!(matches!(
            frame_of(&sent_frames[1]).1,
            ClientControlFrame::AttachmentProbe { session_id, .. }
                if session_id.as_str() == conversation.id
        ));
    }

    for invalid_id in ["c-ffffffffffffffffffffffff", "not-a-session"] {
        let refused = handle_list_attachments(
            &harness.core,
            &harness.caller(),
            ListAttachmentsRequest {
                session_id: invalid_id.to_owned(),
                ..Default::default()
            },
        )
        .await
        .expect_err("unknown and malformed attachment targets are refused");
        assert_eq!(refused.code, ErrorCode::NotFound);
    }
    assert_eq!(sent.lock().unwrap().len(), 4);
}
