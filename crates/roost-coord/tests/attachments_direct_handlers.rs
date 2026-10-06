//! The three direct-attachment RPCs over a booted coordinator and a real
//! database: the grant binds the authenticated tab, the status call returns
//! only a typed receipt, and deleting the worker retires its grants so an
//! in-flight negotiation fails at once. Ports
//! `apps/coord/tests/attachments/attachment-direct-handlers.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod attachments_direct_support;
mod db_support;

use std::sync::Arc;

use attachments_direct_support::{
    DirectHarness, EPOCH, SESSION_ID, SentFrames, TAB_ID, UPLOAD_ID, WORKER_FP, browser_caller,
    offer_of, owner_key, valid_sdp, wait_for_frame,
};
use connectrpc::ErrorCode;
use roost_coord::attachments::grant_state::AttachmentGrantPort;
use roost_coord::attachments::rpc_direct::{
    handle_attachments_direct_status, handle_attachments_grant_direct,
    handle_sessions_negotiate_attachment_peer,
};
use roost_proto::buffa::MessageField;
use roost_proto::{
    AttachmentTransferStatus, AttachmentsDirectStatusRequest, AttachmentsGrantDirectRequest,
    AttachmentsGrantDirectResponse, SessionsNegotiateAttachmentPeerRequest,
    WAttachmentDirectStatus, WorkersDeleteRequest,
};
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;

fn grant_request(tab_id: &str) -> AttachmentsGrantDirectRequest {
    AttachmentsGrantDirectRequest {
        session_id: SESSION_ID.to_owned(),
        worker_fp: WORKER_FP.to_owned(),
        tab_id: tab_id.to_owned(),
        upload_id: UPLOAD_ID.to_owned(),
        filename: "diagram.png".to_owned(),
        short_path: false,
        total_bytes: 1_024,
        ..Default::default()
    }
}

async fn grant(harness: &DirectHarness) -> AttachmentsGrantDirectResponse {
    let caller = browser_caller(Some(TAB_ID));
    handle_attachments_grant_direct(&harness.core, &caller, grant_request(TAB_ID))
        .await
        .expect("a granted upload")
        .body
}

fn installs(sent: &SentFrames) -> usize {
    let frames = sent.lock().unwrap();
    frames
        .iter()
        .filter(|frame| matches!(frame, CoordWorkerDownstream::LocalAttachmentGrant(_)))
        .count()
}

// v2 "AttachmentsGrantDirect binds the authenticated tab and uses only separate attachment grant state".
#[tokio::test]
async fn the_grant_binds_the_authenticated_tab_and_its_own_lease() {
    let harness = DirectHarness::new("grant-tab").await;
    let (handle, sent) = harness.attach_acking_worker();
    let response = grant(&harness).await;

    assert_eq!(response.secret.len(), 64);
    assert_eq!(response.ttl_ms, 60_000);
    assert_eq!(response.worker_epoch, EPOCH);
    assert!(response.peer_supported);
    assert_eq!(
        response.stun_urls,
        vec!["stun:stun.example.test:3478".to_owned()]
    );
    let grants = harness.services().attachments.grants();
    let lease = grants
        .owned_grant(&owner_key(), TAB_ID, WORKER_FP, &response.grant_id)
        .expect("the lease");
    assert_eq!(lease.descriptor.session_id, SESSION_ID);
    assert_eq!(lease.descriptor.filename, "diagram.png");
    assert_eq!(lease.descriptor.total_bytes, 1_024);
    assert!(Arc::ptr_eq(&lease.worker_handle, &handle));

    let caller = browser_caller(Some(TAB_ID));
    let other_tab =
        handle_attachments_grant_direct(&harness.core, &caller, grant_request("other-tab")).await;
    assert_eq!(other_tab.unwrap_err().code, ErrorCode::PermissionDenied);
    let no_tab = browser_caller(None);
    let untabbed =
        handle_attachments_grant_direct(&harness.core, &no_tab, grant_request(TAB_ID)).await;
    assert_eq!(untabbed.unwrap_err().code, ErrorCode::InvalidArgument);
    assert_eq!(installs(&sent), 1, "a refused tab never reached the worker");
}

// v2 "AttachmentsDirectStatus returns only an authenticated typed receipt".
#[tokio::test]
async fn the_status_call_returns_the_workers_typed_receipt() {
    let harness = DirectHarness::new("status-receipt").await;
    let (handle, sent) = harness.attach_acking_worker();
    let core_services = Arc::clone(harness.services());
    let caller = browser_caller(Some(TAB_ID));
    let request = AttachmentsDirectStatusRequest {
        session_id: SESSION_ID.to_owned(),
        upload_id: UPLOAD_ID.to_owned(),
        ..Default::default()
    };
    let call = handle_attachments_direct_status(&harness.core, &caller, request);
    let answer = async {
        let asked = wait_for_frame(&sent, |frame| match frame {
            CoordWorkerDownstream::AttachmentDirectStatusRequest(asked) => Some(asked.clone()),
            _ => None,
        })
        .await;
        assert_eq!(
            (asked.session_id.as_str(), asked.upload_id.as_str()),
            (SESSION_ID, UPLOAD_ID)
        );
        let receipt = WAttachmentDirectStatus {
            request_id: asked.request_id,
            status: MessageField::some(AttachmentTransferStatus {
                upload_id: UPLOAD_ID.to_owned(),
                next_seq: 2,
                bytes_received: 1_024,
                last_chunk_sha256: "a".repeat(64),
                committed: true,
                abs_path: "/attachment/path".to_owned(),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(
            core_services
                .attachments
                .statuses()
                .accept_status(&handle, &receipt)
        );
    };
    let (response, ()) = tokio::join!(call, answer);
    let status = response
        .expect("the receipt")
        .body
        .status
        .into_option()
        .expect("a status");
    assert!(status.committed);
    assert_eq!(status.abs_path, "/attachment/path");
}

// Acceptance: a deleted worker's grants are retired before its generation is
// fenced (v2 `handlers-workers.ts:210-226`), so a negotiation in flight fails
// with the grant refusal at once instead of waiting out its answer deadline.
#[tokio::test]
async fn deleting_the_worker_retires_its_grants_and_fails_the_negotiation() {
    let harness = DirectHarness::new("delete-retires").await;
    let (handle, sent) = harness.attach_acking_worker();
    let granted = grant(&harness).await;
    let caller = browser_caller(Some(TAB_ID));
    let request = SessionsNegotiateAttachmentPeerRequest {
        worker_fp: WORKER_FP.to_owned(),
        grant_id: granted.grant_id,
        tab_id: TAB_ID.to_owned(),
        peer_id: "00000000-0000-4000-8000-000000000010".to_owned(),
        offer_sdp: valid_sdp(),
        worker_epoch: EPOCH.to_owned(),
        ..Default::default()
    };
    let negotiation = handle_sessions_negotiate_attachment_peer(&harness.core, &caller, request);
    let delete = async {
        wait_for_frame(&sent, offer_of).await;
        let deletion = WorkersDeleteRequest {
            fp: WORKER_FP.to_owned(),
            ..Default::default()
        };
        roost_coord::workers::rpc::handle_workers_delete(&harness.core, &caller, deletion)
            .await
            .expect("the worker is deleted");
    };
    let started = std::time::Instant::now();
    let (negotiated, ()) = tokio::join!(negotiation, delete);
    let error = negotiated.unwrap_err();

    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(4),
        "the refusal did not wait for the deadline"
    );
    assert!(handle.is_revoked(), "the deleted generation is fenced");
}
