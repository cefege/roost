//! Terminal-peer signaling capacity: four offers per worker, eight per device
//! and sixty-four in total, charged from admission (before authorization
//! resolves) and held by aborted work until its authorizer exits.
//! Ports `apps/coord/tests/terminal/direct/terminal-peer-negotiation-bounds.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_direct_support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use connectrpc::ErrorCode;
use roost_coord::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};
use roost_coord::terminal_direct::peer_negotiations::{
    PendingPeerNegotiation, TerminalPeerNegotiations,
};
use roost_coord::terminal_direct::peer_state::{
    TerminalGrantSessionAuthorizer, TerminalPeerCaller,
};
use terminal_direct_support::{
    PeerOptions, TestTerminalGrants, caller_for, install_lease, negotiations, peer_request,
    peer_worker, settle_until, synthetic_fingerprint, synthetic_peer_id, test_caller,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

const EPOCH: &str = "bounds-epoch";

fn reserve(
    owner: &TerminalPeerNegotiations,
    grants: &TestTerminalGrants,
    caller: &TerminalPeerCaller,
    worker: &Arc<WorkerHandle>,
    tab_id: &str,
    peer_id: &str,
) -> PendingPeerNegotiation {
    install_lease(grants, worker, caller, tab_id);
    let request = peer_request(worker, tab_id, peer_id);
    owner.negotiate(
        caller.clone(),
        Some(tab_id),
        request,
        CancellationToken::new(),
    )
}

fn device(index: usize) -> TerminalPeerCaller {
    caller_for(&synthetic_fingerprint(index, 'd'))
}

/// An authorizer that counts its calls and holds every one until released.
fn gated(calls: &Arc<AtomicUsize>) -> (TerminalGrantSessionAuthorizer, watch::Sender<bool>) {
    let (open, gate) = watch::channel(false);
    let counter = Arc::clone(calls);
    let authorize: TerminalGrantSessionAuthorizer = Arc::new(move |_, _| {
        counter.fetch_add(1, Ordering::SeqCst);
        let mut gate = gate.clone();
        Box::pin(async move {
            let _ = gate.wait_for(|opened| *opened).await;
            Ok(())
        })
    });
    (authorize, open)
}

async fn exhausted(pending: PendingPeerNegotiation) {
    let error = pending
        .response()
        .await
        .expect_err("capacity refuses the overflow");
    assert_eq!(error.code, ErrorCode::ResourceExhausted);
}

// v2 "bounds four worker, eight device, and sixty-four global peer negotiations".
#[tokio::test]
async fn bounds_four_worker_eight_device_and_sixty_four_global_peer_negotiations() {
    let registry = Arc::new(WorkerRegistry::new());
    let worker_grants = Arc::new(TestTerminalGrants::default());
    let worker_owner = negotiations(&registry, &worker_grants, PeerOptions::default());
    let worker = peer_worker(&registry, &synthetic_fingerprint(1, 'a'), EPOCH).handle;
    let caller = test_caller();
    let mut held = Vec::new();
    for index in 0..4 {
        let tab = format!("worker-tab-{index}");
        held.push(reserve(
            &worker_owner,
            &worker_grants,
            &caller,
            &worker,
            &tab,
            &synthetic_peer_id(index),
        ));
    }
    exhausted(reserve(
        &worker_owner,
        &worker_grants,
        &caller,
        &worker,
        "worker-tab-overflow",
        &synthetic_peer_id(4),
    ))
    .await;

    let device_grants = Arc::new(TestTerminalGrants::default());
    let device_owner = negotiations(&registry, &device_grants, PeerOptions::default());
    for index in 0..8 {
        let device_worker =
            peer_worker(&registry, &synthetic_fingerprint(10 + index, 'b'), EPOCH).handle;
        let tab = format!("device-tab-{index}");
        held.push(reserve(
            &device_owner,
            &device_grants,
            &caller,
            &device_worker,
            &tab,
            &synthetic_peer_id(10 + index),
        ));
    }
    let ninth = peer_worker(&registry, &synthetic_fingerprint(19, 'b'), EPOCH).handle;
    exhausted(reserve(
        &device_owner,
        &device_grants,
        &caller,
        &ninth,
        "device-tab-overflow",
        &synthetic_peer_id(19),
    ))
    .await;

    let global_grants = Arc::new(TestTerminalGrants::default());
    let global_owner = negotiations(&registry, &global_grants, PeerOptions::default());
    let workers: Vec<Arc<WorkerHandle>> = (0..16)
        .map(|index| peer_worker(&registry, &synthetic_fingerprint(30 + index, 'c'), EPOCH).handle)
        .collect();
    for index in 0..64 {
        let tab = format!("global-tab-{index}");
        let peer = synthetic_peer_id(30 + index);
        held.push(reserve(
            &global_owner,
            &global_grants,
            &device(30 + index / 8),
            &workers[index % 16],
            &tab,
            &peer,
        ));
    }
    exhausted(reserve(
        &global_owner,
        &global_grants,
        &device(39),
        &workers[0],
        "global-tab-overflow",
        &synthetic_peer_id(95),
    ))
    .await;

    for owner in [worker_owner, device_owner, global_owner] {
        owner.dispose();
    }
    for pending in held {
        let _ = pending.response().await;
    }
}

// v2 "counts multi-tab worker admissions before authorization resolves".
#[tokio::test]
async fn counts_multi_tab_worker_admissions_before_authorization_resolves() {
    let registry = Arc::new(WorkerRegistry::new());
    let grants = Arc::new(TestTerminalGrants::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let (authorize, open) = gated(&calls);
    let owner = negotiations(
        &registry,
        &grants,
        PeerOptions {
            authorize,
            ..PeerOptions::default()
        },
    );
    let workers: Vec<Arc<WorkerHandle>> = (0..16)
        .map(|index| peer_worker(&registry, &synthetic_fingerprint(130 + index, 'e'), EPOCH).handle)
        .collect();
    let held: Vec<PendingPeerNegotiation> = (0..64)
        .map(|index| {
            let tab = format!("admission-tab-{index}");
            let peer = synthetic_peer_id(130 + index);
            reserve(
                &owner,
                &grants,
                &device(50 + index / 8),
                &workers[index % 16],
                &tab,
                &peer,
            )
        })
        .collect();
    settle_until(|| calls.load(Ordering::SeqCst) == 64).await;

    exhausted(reserve(
        &owner,
        &grants,
        &device(59),
        &workers[0],
        "admission-tab-overflow",
        &synthetic_peer_id(194),
    ))
    .await;
    assert_eq!(calls.load(Ordering::SeqCst), 64);
    owner.dispose();
    open.send_replace(true);
    for pending in held {
        let _ = pending.response().await;
    }
}

// v2 "keeps aborted preauthorization work charged until its authorizer exits".
#[tokio::test]
async fn keeps_aborted_preauthorization_work_charged_until_its_authorizer_exits() {
    let registry = Arc::new(WorkerRegistry::new());
    let grants = Arc::new(TestTerminalGrants::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let (authorize, open) = gated(&calls);
    let owner = negotiations(
        &registry,
        &grants,
        PeerOptions {
            authorize,
            ..PeerOptions::default()
        },
    );
    let worker = peer_worker(&registry, &synthetic_fingerprint(200, 'f'), EPOCH).handle;
    let caller = test_caller();
    let aborts: Vec<CancellationToken> = (0..4).map(|_| CancellationToken::new()).collect();
    let held: Vec<PendingPeerNegotiation> = (0..4)
        .map(|index| {
            let tab = format!("aborted-admission-tab-{index}");
            install_lease(&grants, &worker, &caller, &tab);
            let request = peer_request(&worker, &tab, &synthetic_peer_id(200 + index));
            owner.negotiate(caller.clone(), Some(&tab), request, aborts[index].clone())
        })
        .collect();
    settle_until(|| calls.load(Ordering::SeqCst) == 4).await;
    aborts[0].cancel();

    let overflow = reserve(
        &owner,
        &grants,
        &caller,
        &worker,
        "aborted-admission-overflow",
        &synthetic_peer_id(204),
    );
    exhausted(overflow).await;
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    owner.dispose();
    open.send_replace(true);
    for pending in held {
        let _ = pending.response().await;
    }
}
