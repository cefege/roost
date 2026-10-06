//! Attachment-peer signaling against an in-memory grant port: separate grant
//! ownership, exact worker-generation correlation, the fixed worker failure
//! vocabulary, and cancellation at the answer deadline.
//! Ports `apps/coord/tests/attachments/attachment-peer-negotiations.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod attachments_direct_support;
mod db_support;

use std::sync::Arc;

use attachments_direct_support::{
    DEVICE_FP, PEER_ID, TAB_ID, TestAttachmentGrants, TestWorker, WORKER_FP, install_peer_worker,
    lease_for, offer_of, peer_answer, peer_caller, peer_request, peers_enabled, valid_sdp,
};
use connectrpc::{ConnectError, ErrorCode};
use roost_coord::attachments::grant_state::AttachmentGrantPort;
use roost_coord::attachments::peer::AttachmentPeerNegotiations;
use roost_coord::attachments::peer_state::AttachmentPeerCaller;
use roost_coord::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};
use roost_proto::{
    DLocalAttachmentPeerOffer, SessionsNegotiateAttachmentPeerRequest,
    SessionsNegotiateAttachmentPeerResponse, WLocalAttachmentPeerError,
};
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use tokio::task::JoinHandle;

type Negotiation = JoinHandle<Result<SessionsNegotiateAttachmentPeerResponse, ConnectError>>;

struct PeerFixture {
    workers: Arc<WorkerRegistry>,
    grants: Arc<TestAttachmentGrants>,
    owner: Arc<AttachmentPeerNegotiations>,
}

impl PeerFixture {
    fn new() -> Self {
        let workers = Arc::new(WorkerRegistry::new());
        let grants = Arc::new(TestAttachmentGrants::default());
        let port = Arc::clone(&grants) as Arc<dyn AttachmentGrantPort>;
        let owner = AttachmentPeerNegotiations::new(Arc::clone(&workers), port);
        Self {
            workers,
            grants,
            owner,
        }
    }

    fn worker(&self, epoch: &str) -> TestWorker {
        let worker = install_peer_worker(&self.workers, WORKER_FP, epoch);
        self.grants.install(lease_for(&worker.handle));
        worker
    }

    async fn negotiate(
        &self,
        caller: AttachmentPeerCaller,
        request: SessionsNegotiateAttachmentPeerRequest,
    ) -> Result<SessionsNegotiateAttachmentPeerResponse, ConnectError> {
        self.owner
            .negotiate(&caller, &request, &peers_enabled())
            .await
    }

    /// Start one negotiation and return it with the offer its worker saw.
    async fn begin(
        &self,
        worker: &TestWorker,
        peer_id: &str,
    ) -> (Negotiation, DLocalAttachmentPeerOffer) {
        let owner = Arc::clone(&self.owner);
        let request = peer_request(&worker.handle, peer_id);
        let task = tokio::spawn(async move {
            owner
                .negotiate(&peer_caller(), &request, &peers_enabled())
                .await
        });
        let offer = worker
            .wait_for(|frame| offer_of(frame).filter(|offer| offer.peer_id == peer_id))
            .await;
        (task, offer)
    }
}

fn code_of(result: Result<SessionsNegotiateAttachmentPeerResponse, ConnectError>) -> ErrorCode {
    result.expect_err("the negotiation is refused").code
}

// v2 "requires the exact separate grant, device, tab, worker epoch, and handle".
#[tokio::test]
async fn admission_requires_the_exact_grant_device_tab_epoch_and_handle() {
    let fixture = PeerFixture::new();
    let worker = fixture.worker("attachment-worker-epoch");
    let unknown_grant = SessionsNegotiateAttachmentPeerRequest {
        grant_id: "unknown".to_owned(),
        ..peer_request(&worker.handle, PEER_ID)
    };
    assert_eq!(
        code_of(fixture.negotiate(peer_caller(), unknown_grant).await),
        ErrorCode::PermissionDenied
    );
    let other_tab = SessionsNegotiateAttachmentPeerRequest {
        tab_id: "other-tab".to_owned(),
        ..peer_request(&worker.handle, PEER_ID)
    };
    assert_eq!(
        code_of(fixture.negotiate(peer_caller(), other_tab).await),
        ErrorCode::PermissionDenied
    );
    let other_device = AttachmentPeerCaller {
        owner_key: format!("account-device:test-account:{}", "e".repeat(64)),
        device_fingerprint: "e".repeat(64),
        tab_id: Some(TAB_ID.to_owned()),
    };
    let request = peer_request(&worker.handle, PEER_ID);
    assert_eq!(
        code_of(fixture.negotiate(other_device, request).await),
        ErrorCode::PermissionDenied
    );
    let stale_epoch = SessionsNegotiateAttachmentPeerRequest {
        worker_epoch: "stale-epoch".to_owned(),
        ..peer_request(&worker.handle, PEER_ID)
    };
    assert_eq!(
        code_of(fixture.negotiate(peer_caller(), stale_epoch).await),
        ErrorCode::Unavailable
    );

    assert!(
        worker.frames().is_empty(),
        "no refused negotiation reached the worker"
    );
}

// v2 "sends and accepts only the exact current handle, epoch, peer, and request correlation".
#[tokio::test]
async fn only_the_exact_generation_epoch_peer_and_request_settle_an_offer() {
    let fixture = PeerFixture::new();
    let worker = fixture.worker("attachment-worker-epoch");
    let (operation, offer) = fixture.begin(&worker, PEER_ID).await;
    let answer = peer_answer(&offer.request_id, &offer.peer_id, &worker.handle);
    let rogue = Arc::new(WorkerHandle::clone(&worker.handle));

    assert_eq!(offer.grant_id, "attachment-peer-test-grant");
    assert_eq!(
        offer.connection_generation,
        worker.handle.connection_generation
    );
    assert_eq!(
        Some(offer.worker_epoch.as_str()),
        worker.handle.process_epoch.as_deref()
    );
    assert_eq!(offer.device_fingerprint, DEVICE_FP);
    assert_eq!(offer.tab_id, TAB_ID);
    assert!(!fixture.owner.accept_answer(&rogue, &answer));
    let wrong_epoch = roost_proto::WLocalAttachmentPeerAnswer {
        worker_epoch: "wrong-epoch".to_owned(),
        ..answer.clone()
    };
    assert!(!fixture.owner.accept_answer(&worker.handle, &wrong_epoch));
    let wrong_peer = roost_proto::WLocalAttachmentPeerAnswer {
        peer_id: "00000000-0000-4000-8000-000000000099".to_owned(),
        ..answer.clone()
    };
    assert!(!fixture.owner.accept_answer(&worker.handle, &wrong_peer));
    assert!(fixture.owner.accept_answer(&worker.handle, &answer));
    let response = operation.await.unwrap().expect("the exact answer");
    assert_eq!(response.peer_id, offer.peer_id);
    assert_eq!(response.answer_sdp, valid_sdp());
    assert_eq!(
        Some(response.worker_epoch.as_str()),
        worker.handle.process_epoch.as_deref()
    );
}

// v2 "does not let a stale replacement answer or cancellation settle another negotiation".
#[tokio::test]
async fn a_replaced_generation_neither_answers_nor_cancels_its_successors_offer() {
    let fixture = PeerFixture::new();
    let first = fixture.worker("attachment-worker-epoch");
    let (original, original_offer) = fixture.begin(&first, PEER_ID).await;
    let replacement = fixture.worker("attachment-worker-epoch");
    let stale = peer_answer(
        &original_offer.request_id,
        &original_offer.peer_id,
        &first.handle,
    );

    assert!(!fixture.owner.accept_answer(&first.handle, &stale));
    fixture
        .owner
        .cancel_for_worker_handle(&first.handle, "connection_superseded");
    assert_eq!(code_of(original.await.unwrap()), ErrorCode::Unavailable);

    let (current, current_offer) = fixture
        .begin(&replacement, "00000000-0000-4000-8000-000000000011")
        .await;
    fixture
        .owner
        .cancel_for_worker_handle(&first.handle, "late_cancel");
    assert!(!fixture.owner.accept_answer(&first.handle, &stale));
    let answer = peer_answer(
        &current_offer.request_id,
        &current_offer.peer_id,
        &replacement.handle,
    );
    assert!(fixture.owner.accept_answer(&replacement.handle, &answer));
    assert_eq!(
        current.await.unwrap().expect("the current answer").peer_id,
        current_offer.peer_id
    );
}

// v2 "sends a cancel at timeout and maps only fixed worker errors".
#[tokio::test(start_paused = true)]
async fn the_deadline_cancels_on_the_worker_and_only_fixed_reasons_map() {
    let fixture = PeerFixture::new();
    let worker = fixture.worker("attachment-worker-epoch");
    let timed_out = fixture
        .negotiate(peer_caller(), peer_request(&worker.handle, PEER_ID))
        .await;
    assert_eq!(code_of(timed_out), ErrorCode::DeadlineExceeded);
    assert!(matches!(
        worker.frames().last(),
        Some(CoordWorkerDownstream::LocalAttachmentPeerCancel(_))
    ));

    for (reason, code, peer_id) in [
        (
            "grant_unavailable",
            ErrorCode::PermissionDenied,
            "00000000-0000-4000-8000-000000000012",
        ),
        (
            "capacity",
            ErrorCode::ResourceExhausted,
            "00000000-0000-4000-8000-000000000013",
        ),
        (
            "ice_failed",
            ErrorCode::Unavailable,
            "00000000-0000-4000-8000-000000000014",
        ),
    ] {
        let (operation, offer) = fixture.begin(&worker, peer_id).await;
        let error = WLocalAttachmentPeerError {
            request_id: offer.request_id.clone(),
            connection_generation: worker.handle.connection_generation.clone(),
            worker_epoch: offer.worker_epoch.clone(),
            peer_id: offer.peer_id.clone(),
            reason: reason.to_owned(),
            ..Default::default()
        };
        assert!(fixture.owner.accept_error(&worker.handle, &error));
        assert_eq!(code_of(operation.await.unwrap()), code, "{reason}");
    }
}

// v2 `claimAdmission`: one negotiation per document and worker; a second offer
// is `AlreadyExists`, and reusing the in-flight peer id with a different offer
// is `InvalidArgument`. Neither reaches the worker.
#[tokio::test]
async fn a_documents_second_offer_to_the_same_worker_is_refused() {
    let fixture = PeerFixture::new();
    let worker = fixture.worker("attachment-worker-epoch");
    let (_pending, _offer) = fixture.begin(&worker, PEER_ID).await;
    let offers = worker.frames().len();

    let second = peer_request(&worker.handle, "00000000-0000-4000-8000-000000000011");
    assert_eq!(
        code_of(fixture.negotiate(peer_caller(), second).await),
        ErrorCode::AlreadyExists
    );
    let conflicting = SessionsNegotiateAttachmentPeerRequest {
        offer_sdp: valid_sdp().replace("attachment-offer", "attachment-other"),
        ..peer_request(&worker.handle, PEER_ID)
    };
    assert_eq!(
        code_of(fixture.negotiate(peer_caller(), conflicting).await),
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        worker.frames().len(),
        offers,
        "no refused offer reached the worker"
    );
}
