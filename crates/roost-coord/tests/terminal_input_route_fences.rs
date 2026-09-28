//! A route claim is never sent under a coordinator request id that equals the
//! browser's own nonce: the typed result restores that nonce, so the two ids
//! must be distinguishable or a result could be correlated to the wrong waiter.
//! Ports the `installWorkerRequest` fence of
//! `apps/coord/src/terminal/input/terminal-input-route-results.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use roost_coord::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};
use roost_coord::terminal_input::route_contract::{
    InputRouteClaimRequest, RouteControlError, RouteControlRefusal,
};
use roost_coord::terminal_input::route_results::TerminalInputRouteResults;
use roost_coord::terminal_screen::pending_rpcs::PendingRpcs;
use roost_coord::workers::hop_deadline::HopDeadline;
use roost_protocol::versioning::CAPABILITY_TERMINAL_INPUT_ROUTE_V1;
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;

const WORKER_FP: &str = "aa00000000000000000000000000000000000000000000000000000000000000";

// v2 installWorkerRequest: `created.requestId === pending.slot.browserRequestId` throws.
#[tokio::test]
async fn a_claim_whose_outer_id_would_equal_the_browser_nonce_is_refused_unsent() {
    let workers = Arc::new(WorkerRegistry::new());
    let sent: Arc<Mutex<Vec<CoordWorkerDownstream>>> = Arc::default();
    let log = Arc::clone(&sent);
    let worker = WorkerHandle::new(
        WorkerFp::try_from(WORKER_FP).unwrap(),
        Some("worker-epoch-a".to_owned()),
        "worker-connection-a".to_owned(),
        BTreeSet::from([CAPABILITY_TERMINAL_INPUT_ROUTE_V1.to_owned()]),
        Arc::new(move |frame| {
            log.lock().unwrap().push(frame);
            1
        }),
    );
    worker.mark_ready();
    let worker = Arc::new(worker);
    workers.insert(Arc::clone(&worker));
    // A fresh table mints the same first id as a probe table does, so the
    // browser nonce below is exactly the id the claim would be sent under.
    let colliding = PendingRpcs::new().next_request_id();
    let results =
        TerminalInputRouteResults::new(Arc::clone(&workers), Arc::new(PendingRpcs::new()));

    let outcome = results
        .claim(
            InputRouteClaimRequest {
                browser_request_id: colliding,
                session_id: "session-route-a".to_owned(),
                revision: 1,
                device_fingerprint: "device-route-a".to_owned(),
                tab_id: "tab-route-a".to_owned(),
                connection_id: "sync-connection-a".to_owned(),
                worker: Arc::clone(&worker),
                worker_epoch: "worker-epoch-a".to_owned(),
                deadline: HopDeadline::start(8_000),
            },
            None,
        )
        .await;

    assert!(matches!(
        outcome,
        Err(RouteControlError::Refused(
            RouteControlRefusal::InputRouteUnavailable
        ))
    ));
    assert!(
        sent.lock().unwrap().is_empty(),
        "nothing reached the worker"
    );
}
