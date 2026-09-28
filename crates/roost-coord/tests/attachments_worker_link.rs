//! The worker link's half of direct attachments: the hello acknowledges the
//! attachment peer capability, typed peer and receipt results reach their
//! owners through the real frame dispatcher with the exact current handle, and
//! a closed generation fails what it would have answered.
//! Ports `apps/coord/tests/attachments/attachment-peer-worker-integration.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod attachments_direct_support;
mod worker_link_wire_support;
mod ws_client_support;
mod ws_credential_support;

use attachments_direct_support::{
    DirectHarness, EPOCH, PEER_ID, SESSION_ID, TAB_ID, UPLOAD_ID, WORKER_FP, browser_caller,
    offer_of, peer_answer, valid_sdp, wait_for_frame,
};
use connectrpc::ErrorCode;
use roost_coord::attachments::rpc_direct::{
    handle_attachments_direct_status, handle_attachments_grant_direct,
    handle_sessions_negotiate_attachment_peer,
};
use roost_coord::coord_core::worker_lifecycle::LinkEnd;
use roost_coord::worker_link::dispatch::{
    DispatchOutcome, FrameClass, FrameDispatch, InboundFrame,
};
use roost_proto::buffa::MessageField;
use roost_proto::{
    AttachmentTransferStatus, AttachmentsDirectStatusRequest, AttachmentsGrantDirectRequest,
    SessionsNegotiateAttachmentPeerRequest, WAttachmentDirectStatus, WLocalAttachmentPeerError,
};
use roost_protocol::versioning::CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1;
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::coord_worker::{CoordWorkerDownstream, CoordWorkerUpstream as Up};
use worker_link_wire_support::{WireFixture, next_downstream, upstream_bytes};
use ws_client_support::send_binary;

/// The capabilities a hello-ack granted a hello advertising `advertised`.
async fn acknowledged(fixture: &WireFixture, advertised: &[&str]) -> Vec<String> {
    let mut socket = fixture.dial_worker().await.socket();
    let hello = Up::Hello {
        worker_fp: WorkerFp::try_from(fixture.worker_fp.clone()).unwrap(),
        version: "attachment-test".to_owned(),
        capabilities: advertised
            .iter()
            .map(|capability| (*capability).to_owned())
            .collect(),
        process_epoch: EPOCH.to_owned(),
        trace_id: None,
    };
    send_binary(&mut socket, upstream_bytes(&hello)).await;
    match next_downstream(&mut socket).await {
        Some(CoordWorkerDownstream::HelloAck { capabilities, .. }) => capabilities,
        other => panic!("expected a hello-ack, saw {other:?}"),
    }
}

// v2 "acknowledges attachment peer capability only while its separate result owner is installed".
#[tokio::test]
async fn a_hello_is_acknowledged_the_attachment_peer_capability_it_advertised() {
    let fixture = WireFixture::start("attachment-peer-ack").await;
    let granted = acknowledged(&fixture, &[CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1]).await;
    assert!(
        granted
            .iter()
            .any(|capability| capability == CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1)
    );

    let silent = acknowledged(&fixture, &["terminal_metadata_v1"]).await;
    assert!(
        !silent
            .iter()
            .any(|capability| capability == CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1)
    );
}

fn rpc(frame: Up) -> InboundFrame {
    InboundFrame {
        class: FrameClass::Rpc,
        channel: 0,
        frame,
    }
}

fn negotiation_request(grant_id: String) -> SessionsNegotiateAttachmentPeerRequest {
    SessionsNegotiateAttachmentPeerRequest {
        worker_fp: WORKER_FP.to_owned(),
        grant_id,
        tab_id: TAB_ID.to_owned(),
        peer_id: PEER_ID.to_owned(),
        offer_sdp: valid_sdp(),
        worker_epoch: EPOCH.to_owned(),
        ..Default::default()
    }
}

async fn grant_id(harness: &DirectHarness) -> String {
    let request = AttachmentsGrantDirectRequest {
        session_id: SESSION_ID.to_owned(),
        worker_fp: WORKER_FP.to_owned(),
        tab_id: TAB_ID.to_owned(),
        upload_id: UPLOAD_ID.to_owned(),
        filename: "attachment.bin".to_owned(),
        total_bytes: 1_024,
        ..Default::default()
    };
    let caller = browser_caller(Some(TAB_ID));
    let response = handle_attachments_grant_direct(&harness.core, &caller, request).await;
    response.expect("a granted upload").body.grant_id
}

fn status_request() -> AttachmentsDirectStatusRequest {
    AttachmentsDirectStatusRequest {
        session_id: SESSION_ID.to_owned(),
        upload_id: UPLOAD_ID.to_owned(),
        ..Default::default()
    }
}

// v2 "dispatches attachment peer and status frames through the exact current handle".
#[tokio::test]
async fn peer_and_receipt_results_settle_through_the_frame_dispatcher() {
    let harness = DirectHarness::new("link-dispatch").await;
    let (handle, sent) = harness.attach_acking_worker();
    let dispatcher = harness
        .services()
        .worker_dispatcher(std::sync::Arc::clone(&handle));
    let caller = browser_caller(Some(TAB_ID));
    let request = negotiation_request(grant_id(&harness).await);
    let negotiation = handle_sessions_negotiate_attachment_peer(&harness.core, &caller, request);
    let answer = async {
        let offer = wait_for_frame(&sent, offer_of).await;
        let unmatched = WLocalAttachmentPeerError {
            request_id: "error-request".to_owned(),
            connection_generation: handle.connection_generation.clone(),
            worker_epoch: EPOCH.to_owned(),
            peer_id: PEER_ID.to_owned(),
            reason: "ice_failed".to_owned(),
            ..Default::default()
        };
        let error_outcome =
            dispatcher.handle_now(WORKER_FP, rpc(Up::LocalAttachmentPeerError(unmatched)));
        assert_eq!(error_outcome, DispatchOutcome::Handled);
        let answered =
            Up::LocalAttachmentPeerAnswer(peer_answer(&offer.request_id, PEER_ID, &handle));
        assert_eq!(
            dispatcher.handle_now(WORKER_FP, rpc(answered)),
            DispatchOutcome::Handled
        );
    };
    let (negotiated, ()) = tokio::join!(negotiation, answer);
    assert_eq!(
        negotiated.expect("the dispatched answer").body.answer_sdp,
        valid_sdp()
    );

    let status = handle_attachments_direct_status(&harness.core, &caller, status_request());
    let receipt = async {
        let request_id = wait_for_frame(&sent, |frame| match frame {
            CoordWorkerDownstream::AttachmentDirectStatusRequest(asked) => {
                Some(asked.request_id.clone())
            }
            _ => None,
        })
        .await;
        let result = WAttachmentDirectStatus {
            request_id,
            status: MessageField::some(AttachmentTransferStatus {
                upload_id: UPLOAD_ID.to_owned(),
                next_seq: 1,
                bytes_received: 512,
                last_chunk_sha256: "a".repeat(64),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            dispatcher.handle_now(WORKER_FP, rpc(Up::AttachmentDirectStatus(result))),
            DispatchOutcome::Handled
        );
    };
    let (received, ()) = tokio::join!(status, receipt);
    assert_eq!(
        received
            .expect("the dispatched receipt")
            .body
            .status
            .into_option()
            .unwrap()
            .bytes_received,
        512
    );
}

// v2 `worker-conn.ts` close(): a generation's end fails its attachment peer and
// receipt waits at once (`cancelAttachmentDirectWorkerResults`).
#[tokio::test]
async fn a_closed_generation_fails_its_pending_peer_and_receipt_waits() {
    let harness = DirectHarness::new("link-close").await;
    let (handle, sent) = harness.attach_acking_worker();
    let caller = browser_caller(Some(TAB_ID));
    let request = negotiation_request(grant_id(&harness).await);
    let negotiation = handle_sessions_negotiate_attachment_peer(&harness.core, &caller, request);
    let status = handle_attachments_direct_status(&harness.core, &caller, status_request());
    let close = async {
        wait_for_frame(&sent, offer_of).await;
        wait_for_frame(&sent, |frame| {
            matches!(
                frame,
                CoordWorkerDownstream::AttachmentDirectStatusRequest(_)
            )
            .then_some(())
        })
        .await;
        harness
            .services()
            .worker_lifecycle
            .closed(&handle, LinkEnd::Closed { replaced: false });
    };
    let started = std::time::Instant::now();
    let (negotiated, received, ()) = tokio::join!(negotiation, status, close);

    assert_eq!(negotiated.unwrap_err().code, ErrorCode::Unavailable);
    assert_eq!(received.unwrap_err().code, ErrorCode::Unavailable);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(4),
        "neither waited for its deadline"
    );
}
