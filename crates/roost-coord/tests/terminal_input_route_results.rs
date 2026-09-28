//! Typed input-route claim and probe correlation against a fake current worker
//! generation: outer-id fencing, socket cancellation, bounded admission, and
//! retirement across a same-epoch reconnection, with no coordinator running.
//! Ports `apps/coord/tests/terminal/input/terminal-input-route-results.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use connectrpc::ErrorCode;
use roost_coord::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};
use roost_coord::terminal_input::route_contract::{
    InputRouteClaimRequest, MAX_TERMINAL_ROUTE_CONTROLS_PER_SOCKET, RouteControlError,
    RouteControlRefusal, TransportProbeRequest,
};
use roost_coord::terminal_input::route_results::TerminalInputRouteResults;
use roost_coord::terminal_screen::pending_rpcs::PendingRpcs;
use roost_coord::workers::hop_deadline::HopDeadline;
use roost_proto::buffa::MessageField;
use roost_proto::{
    TerminalInputRouteResult, WTerminalInputRouteResult, WTerminalTransportProbeResult,
};
use roost_protocol::versioning::CAPABILITY_TERMINAL_INPUT_ROUTE_V1;
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;

const WORKER_FP: &str = "aa00000000000000000000000000000000000000000000000000000000000000";
const WORKER_EPOCH: &str = "worker-epoch-a";
const CONTROL_CONNECTION: &str = "sync-connection-a";

type SentFrames = Arc<Mutex<Vec<CoordWorkerDownstream>>>;

struct Harness {
    workers: Arc<WorkerRegistry>,
    results: Arc<TerminalInputRouteResults>,
}

impl Harness {
    fn new() -> Self {
        let workers = Arc::new(WorkerRegistry::new());
        let results = Arc::new(TerminalInputRouteResults::new(
            Arc::clone(&workers),
            Arc::new(PendingRpcs::new()),
        ));
        Self { workers, results }
    }

    /// A ready, route-capable generation made current for the worker.
    fn attach_worker(&self, generation: &str, sent: &SentFrames) -> Arc<WorkerHandle> {
        let handle = Arc::new(route_worker(generation, sent));
        self.workers.insert(Arc::clone(&handle));
        handle
    }
}

fn route_worker(generation: &str, sent: &SentFrames) -> WorkerHandle {
    let sent = Arc::clone(sent);
    let handle = WorkerHandle::new(
        WorkerFp::try_from(WORKER_FP).unwrap(),
        Some(WORKER_EPOCH.to_owned()),
        generation.to_owned(),
        BTreeSet::from([CAPABILITY_TERMINAL_INPUT_ROUTE_V1.to_owned()]),
        Arc::new(move |frame| {
            sent.lock().unwrap().push(frame);
            1
        }),
    );
    handle.mark_ready();
    handle
}

fn route_claim(worker: &Arc<WorkerHandle>, request_id: &str) -> InputRouteClaimRequest {
    InputRouteClaimRequest {
        browser_request_id: request_id.to_owned(),
        session_id: "session-route-a".to_owned(),
        revision: 1,
        device_fingerprint: "device-route-a".to_owned(),
        tab_id: "tab-route-a".to_owned(),
        connection_id: CONTROL_CONNECTION.to_owned(),
        worker: Arc::clone(worker),
        worker_epoch: WORKER_EPOCH.to_owned(),
        deadline: HopDeadline::start(8_000),
    }
}

fn probe(worker: &Arc<WorkerHandle>, deadline: HopDeadline) -> TransportProbeRequest {
    TransportProbeRequest {
        browser_request_id: "browser-probe-request".to_owned(),
        connection_id: CONTROL_CONNECTION.to_owned(),
        worker_fp: WORKER_FP.to_owned(),
        worker: Arc::clone(worker),
        worker_epoch: WORKER_EPOCH.to_owned(),
        deadline,
    }
}

fn route_result(outer: &str, inner: &str) -> WTerminalInputRouteResult {
    WTerminalInputRouteResult {
        request_id: outer.to_owned(),
        result: MessageField::some(TerminalInputRouteResult {
            request_id: inner.to_owned(),
            session_id: "session-route-a".to_owned(),
            revision: 1,
            accepted: true,
            latest_revision: 1,
            input_route_epoch: "route-epoch-a".to_owned(),
            worker_epoch: WORKER_EPOCH.to_owned(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Let spawned controls run up to their first real wait.
async fn settle_tasks() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}

fn frames(sent: &SentFrames) -> Vec<CoordWorkerDownstream> {
    sent.lock().unwrap().clone()
}

fn failure_code(outcome: Result<impl Sized, RouteControlError>) -> ErrorCode {
    match outcome {
        Err(RouteControlError::Failed(error)) => error.code,
        Err(RouteControlError::Refused(refusal)) => panic!("expected a failure, got {refusal:?}"),
        Ok(_) => panic!("expected a failure, got a result"),
    }
}

// v2: "installs typed correlation before send and restores only the validated browser nonce"
#[tokio::test]
async fn a_claim_is_settled_only_by_its_generation_and_restores_the_browser_nonce() {
    let harness = Harness::new();
    let sent = SentFrames::default();
    let worker = harness.attach_worker("worker-connection-a", &sent);
    let results = Arc::clone(&harness.results);
    let claim = route_claim(&worker, "browser-route-request");
    let operation = tokio::spawn(async move { results.claim(claim, None).await });
    settle_tasks().await;

    let CoordWorkerDownstream::TerminalInputRouteClaim(sent_claim) = &frames(&sent)[0] else {
        panic!("expected a terminal input route claim");
    };
    let outer = sent_claim.request_id.clone();
    assert_ne!(outer, "browser-route-request");
    assert_eq!(sent_claim.device_fingerprint, "device-route-a");
    assert_eq!(sent_claim.tab_id, "tab-route-a");
    assert_eq!(sent_claim.browser_connection_id, CONTROL_CONNECTION);

    let impostor = Arc::new(route_worker("worker-connection-b", &sent));
    assert!(
        !harness
            .results
            .accept_input_route_result(&impostor, &route_result(&outer, &outer))
    );
    assert!(
        !harness
            .results
            .accept_input_route_result(&worker, &route_result(&outer, "wrong-inner-id"))
    );
    assert!(
        harness
            .results
            .accept_input_route_result(&worker, &route_result(&outer, &outer))
    );

    let result = operation
        .await
        .unwrap()
        .expect("the validated claim result");
    assert_eq!(result.request_id, "browser-route-request");
    assert_eq!(result.session_id, "session-route-a");
    assert_eq!(result.input_route_epoch, "route-epoch-a");
    assert_eq!(result.worker_epoch, WORKER_EPOCH);

    harness
        .results
        .retire_browser_connection(CONTROL_CONNECTION);
    let CoordWorkerDownstream::TerminalViewSocketClosed(closed) = &frames(&sent)[1] else {
        panic!("expected the exact worker route retirement");
    };
    assert_eq!(closed.socket_id, CONTROL_CONNECTION);
}

// v2: "does not settle a typed result after its captured worker handle is replaced"
#[tokio::test]
async fn a_replaced_generation_cannot_settle_the_claim_it_was_sent() {
    let harness = Harness::new();
    let sent = SentFrames::default();
    let worker = harness.attach_worker("worker-connection-a", &sent);
    let results = Arc::clone(&harness.results);
    let claim = route_claim(&worker, "browser-route-replaced");
    let operation = tokio::spawn(async move { results.claim(claim, None).await });
    settle_tasks().await;
    let CoordWorkerDownstream::TerminalInputRouteClaim(sent_claim) = &frames(&sent)[0] else {
        panic!("expected a terminal input route claim");
    };
    let outer = sent_claim.request_id.clone();

    harness.attach_worker("worker-connection-replacement", &sent);

    assert!(
        !harness
            .results
            .accept_input_route_result(&worker, &route_result(&outer, &outer))
    );
    harness
        .results
        .cancel_for_worker_handle(&worker, "connection_superseded");
    assert_eq!(failure_code(operation.await.unwrap()), ErrorCode::Canceled);
}

// v2: "retires a claimed route through a same-epoch worker reconnection"
#[tokio::test]
async fn a_retirement_retained_while_unroutable_reaches_the_same_epoch_reconnection() {
    let harness = Harness::new();
    let first_sent = SentFrames::default();
    let worker = harness.attach_worker("worker-connection-a", &first_sent);
    let results = Arc::clone(&harness.results);
    let claim = route_claim(&worker, "browser-route-reconnect");
    let operation = tokio::spawn(async move { results.claim(claim, None).await });
    settle_tasks().await;
    let CoordWorkerDownstream::TerminalInputRouteClaim(sent_claim) = &frames(&first_sent)[0] else {
        panic!("expected a terminal input route claim");
    };
    let outer = sent_claim.request_id.clone();
    assert!(
        harness
            .results
            .accept_input_route_result(&worker, &route_result(&outer, &outer))
    );
    assert_eq!(
        operation.await.unwrap().unwrap().request_id,
        "browser-route-reconnect"
    );

    harness.workers.retire(&worker.worker_fp);
    harness
        .results
        .retire_browser_connection(CONTROL_CONNECTION);
    assert_eq!(
        frames(&first_sent).len(),
        1,
        "no worker was routable to hear it"
    );

    let reconnected_sent = SentFrames::default();
    harness.attach_worker("worker-connection-reconnected", &reconnected_sent);
    harness.results.flush_worker_retirements(&worker.worker_fp);

    let reconnected = frames(&reconnected_sent);
    assert_eq!(reconnected.len(), 1);
    assert!(matches!(
        &reconnected[0],
        CoordWorkerDownstream::TerminalViewSocketClosed(closed) if closed.socket_id == CONTROL_CONNECTION
    ));
}

// v2: "does not send a claim after its pre-lookup socket reservation is retired"
#[tokio::test]
async fn a_reservation_retired_before_the_lookup_sends_nothing() {
    let harness = Harness::new();
    let sent = SentFrames::default();
    let worker = harness.attach_worker("worker-connection-a", &sent);
    let slot = harness
        .results
        .reserve_control(CONTROL_CONNECTION, "browser-route-before-lookup")
        .unwrap();

    harness
        .results
        .retire_browser_connection(CONTROL_CONNECTION);

    let outcome = harness
        .results
        .claim(
            route_claim(&worker, "browser-route-before-lookup"),
            Some(slot),
        )
        .await;
    assert!(matches!(
        outcome,
        Err(RouteControlError::Refused(
            RouteControlRefusal::InputRouteUnavailable
        ))
    ));
    assert!(frames(&sent).is_empty());
}

// v2: "bounds one Sync socket and releases its waiters on close"
#[tokio::test]
async fn one_socket_holds_a_bounded_number_of_controls_and_close_releases_them() {
    let harness = Harness::new();
    let sent = SentFrames::default();
    let worker = harness.attach_worker("worker-connection-a", &sent);
    let operations: Vec<_> = (0..MAX_TERMINAL_ROUTE_CONTROLS_PER_SOCKET)
        .map(|index| {
            let results = Arc::clone(&harness.results);
            let claim = route_claim(&worker, &format!("browser-route-{index}"));
            tokio::spawn(async move { results.claim(claim, None).await })
        })
        .collect();
    settle_tasks().await;

    let over_limit = harness
        .results
        .claim(route_claim(&worker, "browser-route-over-limit"), None)
        .await;
    assert!(matches!(
        over_limit,
        Err(RouteControlError::Refused(
            RouteControlRefusal::RouteClaimBusy
        ))
    ));
    harness
        .results
        .retire_browser_connection(CONTROL_CONNECTION);
    for operation in operations {
        assert!(
            operation.await.unwrap().is_err(),
            "every waiter is released"
        );
    }
    let after_close = frames(&sent);
    assert_eq!(
        after_close.len(),
        MAX_TERMINAL_ROUTE_CONTROLS_PER_SOCKET + 1
    );
    assert!(matches!(
        after_close.last(),
        Some(CoordWorkerDownstream::TerminalViewSocketClosed(_))
    ));

    let results = Arc::clone(&harness.results);
    let claim = route_claim(&worker, "browser-route-replacement");
    let replacement = tokio::spawn(async move { results.claim(claim, None).await });
    settle_tasks().await;
    assert_eq!(
        frames(&sent).len(),
        MAX_TERMINAL_ROUTE_CONTROLS_PER_SOCKET + 2
    );
    replacement.abort();
}

// v2: "times out a typed probe through the same pending-RPC deadline"
#[tokio::test(start_paused = true)]
async fn a_probe_with_no_reply_fails_at_its_hop_deadline() {
    let harness = Harness::new();
    let sent = SentFrames::default();
    let worker = harness.attach_worker("worker-connection-a", &sent);

    let outcome = harness
        .results
        .probe(probe(&worker, HopDeadline::start(1_000)), None)
        .await;

    assert_eq!(failure_code(outcome), ErrorCode::DeadlineExceeded);
    let sent = frames(&sent);
    assert_eq!(sent.len(), 1);
    assert!(matches!(
        sent[0],
        CoordWorkerDownstream::TerminalTransportProbe(_)
    ));
}

// v2: "drops a mismatched probe epoch without settling its browser waiter"
#[tokio::test]
async fn a_probe_result_for_another_epoch_leaves_the_waiter_pending() {
    let harness = Harness::new();
    let sent = SentFrames::default();
    let worker = harness.attach_worker("worker-connection-a", &sent);
    let results = Arc::clone(&harness.results);
    let request = probe(&worker, HopDeadline::start(8_000));
    let operation = tokio::spawn(async move { results.probe(request, None).await });
    settle_tasks().await;
    let CoordWorkerDownstream::TerminalTransportProbe(sent_probe) = &frames(&sent)[0] else {
        panic!("expected a terminal transport probe");
    };

    let wrong_epoch = WTerminalTransportProbeResult {
        request_id: sent_probe.request_id.clone(),
        worker_epoch: "wrong-epoch".to_owned(),
        ..Default::default()
    };
    assert!(
        !harness
            .results
            .accept_transport_probe_result(&worker, &wrong_epoch)
    );
    harness
        .results
        .retire_browser_connection(CONTROL_CONNECTION);
    assert_eq!(failure_code(operation.await.unwrap()), ErrorCode::Canceled);
}
