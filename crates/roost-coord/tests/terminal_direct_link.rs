//! Terminal-peer signaling through the worker link: the hello acknowledges the
//! peer carrier only while it is enabled, a typed answer or refusal settles a
//! negotiation only through the exact current generation's dispatcher, a
//! revoked generation's offers fail at once, and deleting a worker retires its
//! direct grants while its generation still admits the retirement frame.
//! Ports `apps/coord/tests/terminal/direct/terminal-peer-worker-integration.test.ts`
//! (peer half) and the retirement call of `handlers-workers.ts:210-218`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
mod terminal_direct_core_support;
mod terminal_direct_support;

use std::collections::BTreeSet;
use std::sync::Arc;

use connectrpc::ErrorCode;
use roost_coord::coord_core::worker_lifecycle::LinkEnd;
use roost_coord::terminal_direct::peer_rpc::handle_sessions_negotiate_local_terminal_peer;
use roost_coord::worker_link::dispatch::{
    DispatchOutcome, FrameClass, FrameDispatch, InboundFrame,
};
use roost_proto::{
    SessionsNegotiateLocalTerminalPeerRequest, SessionsNegotiateLocalTerminalPeerResponse,
    WLocalTerminalPeerError,
};
use roost_protocol::versioning::{
    CAPABILITY_TERMINAL_INPUT_ROUTE_V1, CAPABILITY_TERMINAL_PEER_WEBRTC_V1,
};
use roost_protocol::wire::coord_worker::{CoordWorkerDownstream, CoordWorkerUpstream as Up};
use terminal_direct_core_support::{DirectCore, LOCAL_EPOCH, LOCAL_TAB, LOCAL_WORKER};
use terminal_direct_support::{
    TestWorker, install_worker, peer_answer, settle_until, synthetic_peer_id, valid_sdp,
};
use tokio::task::JoinHandle;

const BOTH: [&str; 2] = [
    CAPABILITY_TERMINAL_PEER_WEBRTC_V1,
    CAPABILITY_TERMINAL_INPUT_ROUTE_V1,
];

type Negotiation =
    JoinHandle<Result<SessionsNegotiateLocalTerminalPeerResponse, connectrpc::ConnectError>>;

/// A booted coordinator with a peer-capable local worker holding a grant.
async fn granted(label: &str) -> (DirectCore, TestWorker, String) {
    let direct = DirectCore::new(label, true).await;
    let worker = install_worker(
        &direct.core.services.workers,
        LOCAL_WORKER,
        Some(LOCAL_EPOCH),
        &BOTH,
    );
    let session = direct.insert_session(LOCAL_WORKER, "open").await;
    let grant = direct.grant_with_ack(&worker, vec![session]).await;
    (direct, worker, grant.grant_id)
}

/// Run the negotiate handler on its own task and wait for its offer.
async fn negotiate(
    direct: &DirectCore,
    worker: &TestWorker,
    grant_id: &str,
    peer: usize,
) -> Negotiation {
    let before = worker.frames().len();
    let (core, caller) = (direct.core.clone(), direct.caller.clone());
    let request = SessionsNegotiateLocalTerminalPeerRequest {
        worker_fp: LOCAL_WORKER.to_owned(),
        grant_id: grant_id.to_owned(),
        tab_id: LOCAL_TAB.to_owned(),
        peer_id: synthetic_peer_id(peer),
        offer_sdp: valid_sdp(),
        worker_epoch: LOCAL_EPOCH.to_owned(),
        ..Default::default()
    };
    let negotiation = tokio::spawn(async move {
        handle_sessions_negotiate_local_terminal_peer(&core, &caller, request)
            .await
            .map(|response| response.body)
    });
    settle_until(|| worker.frames().len() > before).await;
    negotiation
}

fn rpc(frame: Up) -> InboundFrame {
    InboundFrame {
        class: FrameClass::Rpc,
        channel: 0,
        frame,
    }
}

// v2 "acknowledges peer capability only with its result owner and route
// capability only with its owner" and "acknowledges input route only when its
// typed result owner is installed": the peer owner answers only while enabled.
#[tokio::test]
async fn a_hello_is_acknowledged_the_peer_carrier_only_while_it_is_enabled() {
    let advertised: BTreeSet<String> = BOTH
        .iter()
        .map(|capability| (*capability).to_owned())
        .collect();
    let enabled = DirectCore::new("ack-on", true).await;
    let disabled = DirectCore::new("ack-off", false).await;

    assert_eq!(
        enabled
            .core
            .services
            .worker_lifecycle
            .acknowledged_capabilities(&advertised),
        vec![
            CAPABILITY_TERMINAL_INPUT_ROUTE_V1,
            CAPABILITY_TERMINAL_PEER_WEBRTC_V1
        ]
    );
    assert_eq!(
        disabled
            .core
            .services
            .worker_lifecycle
            .acknowledged_capabilities(&advertised),
        vec![CAPABILITY_TERMINAL_INPUT_ROUTE_V1]
    );
}

// v2 "dispatches typed peer answer and error through the exact current handle".
#[tokio::test]
async fn a_typed_answer_and_error_settle_through_the_exact_current_dispatcher() {
    let (direct, worker, grant_id) = granted("dispatch").await;
    let dispatcher = direct
        .core
        .services
        .worker_dispatcher(Arc::clone(&worker.handle));

    let answered = negotiate(&direct, &worker, &grant_id, 20).await;
    let offer = worker.last_offer();
    let answer = Up::LocalTerminalPeerAnswer(peer_answer(&offer, &worker.handle));
    assert_eq!(
        dispatcher.handle_now(LOCAL_WORKER, rpc(answer)),
        DispatchOutcome::Handled
    );
    let response = answered.await.unwrap().expect("the worker's answer");
    assert_eq!(response.peer_id, offer.peer_id);
    assert_eq!(response.worker_epoch, LOCAL_EPOCH);

    let refused = negotiate(&direct, &worker, &grant_id, 21).await;
    let offer = worker.last_offer();
    let error = Up::LocalTerminalPeerError(WLocalTerminalPeerError {
        request_id: offer.request_id.clone(),
        connection_generation: worker.handle.connection_generation.clone(),
        worker_epoch: LOCAL_EPOCH.to_owned(),
        peer_id: offer.peer_id.clone(),
        reason: "ice_failed".to_owned(),
        ..Default::default()
    });
    assert_eq!(
        dispatcher.handle_now(LOCAL_WORKER, rpc(error)),
        DispatchOutcome::Handled
    );
    assert_eq!(
        refused.await.unwrap().unwrap_err().code,
        ErrorCode::Unavailable
    );
}

// v2 `worker-conn.ts` revoke/close: the generation's pending offers fail at
// once, and a late answer through its fenced dispatcher settles nothing.
#[tokio::test]
async fn a_revoked_generation_fails_its_pending_offer_and_its_late_answer_settles_nothing() {
    let (direct, worker, grant_id) = granted("revoked").await;
    let dispatcher = direct
        .core
        .services
        .worker_dispatcher(Arc::clone(&worker.handle));
    let pending = negotiate(&direct, &worker, &grant_id, 30).await;
    let offer = worker.last_offer();

    direct.core.services.workers.fence(&worker.handle.worker_fp);
    direct
        .core
        .services
        .worker_lifecycle
        .closed(&worker.handle, LinkEnd::Revoked);
    assert_eq!(
        pending.await.unwrap().unwrap_err().code,
        ErrorCode::Unavailable
    );
    let late = Up::LocalTerminalPeerAnswer(peer_answer(&offer, &worker.handle));
    assert_eq!(
        dispatcher.handle_now(LOCAL_WORKER, rpc(late)),
        DispatchOutcome::Refused
    );
}

// v2 `handlers-workers.ts:210-218`: retirement is sent while the deleted
// worker's generation still admits it, and every lease on it is dropped.
#[tokio::test]
async fn deleting_a_worker_retires_its_direct_grants_before_the_fence() {
    let (direct, worker, _) = granted("delete").await;
    let request = roost_proto::WorkersDeleteRequest {
        fp: LOCAL_WORKER.to_owned(),
        ..Default::default()
    };
    roost_coord::workers::rpc::handle_workers_delete(&direct.core, &direct.caller, request)
        .await
        .expect("the worker is deleted");

    let retired: Vec<(String, String)> = worker
        .frames()
        .into_iter()
        .filter_map(|frame| match frame {
            CoordWorkerDownstream::TerminalDirectRetire(retire) => {
                Some((retire.worker_epoch, retire.reason))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        retired,
        vec![(LOCAL_EPOCH.to_owned(), "worker_deleted".to_owned())]
    );
    assert!(
        direct
            .core
            .services
            .terminal_direct
            .grants()
            .list()
            .is_empty()
    );
    assert!(
        worker.handle.is_revoked(),
        "the fence follows the retirement"
    );
}
