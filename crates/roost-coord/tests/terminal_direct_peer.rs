//! Terminal-peer signaling: admission fences, exact worker identity for typed
//! answers, cancellation on abort, revocation and replacement, duplicate
//! floods, and the answer deadline, over a fake grant port and fake worker
//! generations; SDP is fixture input only.
//! Ports `apps/coord/tests/terminal/direct/terminal-peer-negotiations.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_direct_support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use connectrpc::ErrorCode;
use roost_coord::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};
use roost_coord::terminal_direct::grant_state::{
    TerminalGrantInvalidation, TerminalGrantInvalidationKind, TerminalGrantLeaseSnapshot,
};
use roost_coord::terminal_direct::peer_negotiations::{
    PendingPeerNegotiation, TerminalPeerNegotiations,
};
use roost_proto::DLocalTerminalPeerOffer;
use terminal_direct_support::{
    PEER_ID, PeerOptions, TestTerminalGrants, TestWorker, WORKER_A, install_lease, install_worker,
    negotiations, peer_answer, peer_request, peer_worker, settle_until, test_caller, valid_sdp,
    yield_many,
};
use tokio_util::sync::CancellationToken;

const TAB: &str = "terminal-peer-test-tab";
const EPOCH: &str = "worker-epoch";

struct Harness {
    registry: Arc<WorkerRegistry>,
    grants: Arc<TestTerminalGrants>,
    owner: Arc<TerminalPeerNegotiations>,
    worker: TestWorker,
    lease: TerminalGrantLeaseSnapshot,
}

impl Harness {
    fn new(options: PeerOptions) -> Self {
        let registry = Arc::new(WorkerRegistry::new());
        let grants = Arc::new(TestTerminalGrants::default());
        let worker = peer_worker(&registry, WORKER_A, EPOCH);
        let lease = install_lease(&grants, &worker.handle, &test_caller(), TAB);
        let owner = negotiations(&registry, &grants, options);
        Self {
            registry,
            grants,
            owner,
            worker,
            lease,
        }
    }

    fn negotiate(
        &self,
        worker: &WorkerHandle,
        peer_id: &str,
        abort: CancellationToken,
    ) -> PendingPeerNegotiation {
        self.owner.negotiate(
            test_caller(),
            Some(TAB),
            peer_request(worker, TAB, peer_id),
            abort,
        )
    }

    /// Start a negotiation and wait until its offer reaches `worker`.
    async fn begin(
        &self,
        worker: &TestWorker,
        peer_id: &str,
        abort: CancellationToken,
    ) -> (PendingPeerNegotiation, DLocalTerminalPeerOffer) {
        let before = worker.frames().len();
        let pending = self.negotiate(&worker.handle, peer_id, abort);
        settle_until(|| worker.frames().len() > before).await;
        (pending, worker.last_offer())
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.owner.dispose();
    }
}

async fn code_of(pending: PendingPeerNegotiation) -> ErrorCode {
    pending
        .response()
        .await
        .expect_err("the negotiation is refused")
        .code
}

// v2 "rejects tab, grant, SDP, epoch, and capability failures before an offer".
#[tokio::test]
async fn rejects_tab_grant_sdp_epoch_and_capability_failures_before_an_offer() {
    let harness = Harness::new(PeerOptions::default());
    let worker = &harness.worker.handle;
    let refused = |request| {
        harness
            .owner
            .negotiate(test_caller(), Some(TAB), request, CancellationToken::new())
    };

    let mut other_tab = peer_request(worker, "different-tab", PEER_ID);
    other_tab.tab_id = "different-tab".to_owned();
    assert_eq!(
        code_of(refused(other_tab)).await,
        ErrorCode::PermissionDenied
    );
    let mut unknown_grant = peer_request(worker, TAB, PEER_ID);
    unknown_grant.grant_id = "unknown-grant".to_owned();
    assert_eq!(
        code_of(refused(unknown_grant)).await,
        ErrorCode::PermissionDenied
    );
    let mut not_sdp = peer_request(worker, TAB, PEER_ID);
    not_sdp.offer_sdp = "not-sdp".to_owned();
    assert_eq!(code_of(refused(not_sdp)).await, ErrorCode::InvalidArgument);
    let mut other_epoch = peer_request(worker, TAB, PEER_ID);
    other_epoch.worker_epoch = "different-epoch".to_owned();
    assert_eq!(code_of(refused(other_epoch)).await, ErrorCode::Unavailable);

    let unsupported = install_worker(
        &harness.registry,
        &"b".repeat(64),
        Some("unsupported-epoch"),
        &[],
    );
    install_lease(&harness.grants, &unsupported.handle, &test_caller(), TAB);
    let request = peer_request(&unsupported.handle, TAB, PEER_ID);
    assert_eq!(code_of(refused(request)).await, ErrorCode::Unavailable);
    assert!(harness.worker.frames().is_empty());
    assert!(unsupported.frames().is_empty());
}

// v2 "accepts only an answer from the exact current worker handle".
#[tokio::test]
async fn accepts_only_an_answer_from_the_exact_current_worker_handle() {
    let harness = Harness::new(PeerOptions::default());
    let (started, offer) = harness
        .begin(&harness.worker, PEER_ID, CancellationToken::new())
        .await;
    let answer = peer_answer(&offer, &harness.worker.handle);
    let rogue = Arc::new(WorkerHandle::clone(&harness.worker.handle));

    assert!(!harness.owner.accept_answer(&rogue, &answer));
    assert!(harness.owner.accept_answer(&harness.worker.handle, &answer));
    let response = started.response().await.unwrap();
    assert_eq!(response.peer_id, offer.peer_id);
    assert_eq!(response.answer_sdp, valid_sdp());
    assert_eq!(response.worker_epoch, EPOCH);
}

// v2 "ignores a late old-handle answer after replacement and cancels its pending offer".
#[tokio::test]
async fn ignores_a_late_old_handle_answer_after_replacement_and_cancels_its_pending_offer() {
    let harness = Harness::new(PeerOptions::default());
    let first = harness.worker.clone();
    let (started, offer) = harness
        .begin(&first, PEER_ID, CancellationToken::new())
        .await;
    let replacement = peer_worker(&harness.registry, WORKER_A, EPOCH);

    assert!(
        !harness
            .owner
            .accept_answer(&first.handle, &peer_answer(&offer, &first.handle))
    );
    harness
        .owner
        .cancel_for_worker_handle(&first.handle, "connection_superseded");
    assert_eq!(code_of(started).await, ErrorCode::Unavailable);
    assert!(replacement.frames().is_empty());
}

// v2 "sends cancellation on browser abort and grant revocation".
#[tokio::test]
async fn sends_cancellation_on_browser_abort_and_grant_revocation() {
    let harness = Harness::new(PeerOptions::default());
    let abort = CancellationToken::new();
    let (aborted, _) = harness.begin(&harness.worker, PEER_ID, abort.clone()).await;
    abort.cancel();
    assert_eq!(code_of(aborted).await, ErrorCode::Canceled);
    assert_eq!(
        harness.worker.last_kind(),
        Some("local-terminal-peer-cancel")
    );

    let lease = harness.lease.clone();
    let second_peer = "00000000-0000-4000-8000-000000000011";
    let (revoked, _) = harness
        .begin(&harness.worker, second_peer, CancellationToken::new())
        .await;
    harness
        .grants
        .invalidate(&TerminalGrantInvalidation::of_lease(
            TerminalGrantInvalidationKind::DeviceRevoked,
            &lease,
            lease.session_ids.clone(),
            None,
        ));
    assert_eq!(code_of(revoked).await, ErrorCode::PermissionDenied);
    assert_eq!(
        harness.worker.last_kind(),
        Some("local-terminal-peer-cancel")
    );
}

// v2 "rejects a duplicate in-flight flood before extra authorization or offers".
#[tokio::test]
async fn rejects_a_duplicate_in_flight_flood_before_extra_authorization_or_offers() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let harness = Harness::new(PeerOptions {
        authorize: Arc::new(move |_, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }),
        ..PeerOptions::default()
    });
    let worker = &harness.worker.handle;
    let first = harness.negotiate(worker, PEER_ID, CancellationToken::new());
    let mut conflicting = peer_request(worker, TAB, PEER_ID);
    conflicting.offer_sdp = valid_sdp().replace("peer-offer", "different-offer");
    let conflict = harness.owner.negotiate(
        test_caller(),
        Some(TAB),
        conflicting,
        CancellationToken::new(),
    );
    assert_eq!(code_of(conflict).await, ErrorCode::InvalidArgument);
    for _ in 0..96 {
        let duplicate = harness.negotiate(worker, PEER_ID, CancellationToken::new());
        assert_eq!(code_of(duplicate).await, ErrorCode::AlreadyExists);
    }
    settle_until(|| !harness.worker.frames().is_empty()).await;
    yield_many().await;

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.worker.frames().len(), 1);
    harness
        .owner
        .cancel_for_worker_handle(worker, "test_cleanup");
    assert_eq!(code_of(first).await, ErrorCode::Unavailable);
}

// v2 "times out and disposes pending offers without retaining them".
#[tokio::test(start_paused = true)]
async fn times_out_and_disposes_pending_offers_without_retaining_them() {
    let harness = Harness::new(PeerOptions {
        answer_timeout_ms: 8_000,
        ..PeerOptions::default()
    });
    let (timed_out, offer) = harness
        .begin(&harness.worker, PEER_ID, CancellationToken::new())
        .await;
    assert!(
        offer.budget_ms > 0 && offer.budget_ms <= 8_000,
        "the worker gets the remaining budget"
    );
    tokio::time::advance(std::time::Duration::from_millis(8_000)).await;
    assert_eq!(code_of(timed_out).await, ErrorCode::DeadlineExceeded);
    assert_eq!(
        harness.worker.last_kind(),
        Some("local-terminal-peer-cancel")
    );

    let second_peer = "00000000-0000-4000-8000-000000000012";
    let (disposed, _) = harness
        .begin(&harness.worker, second_peer, CancellationToken::new())
        .await;
    harness.owner.dispose();
    assert_eq!(code_of(disposed).await, ErrorCode::Unavailable);
}
