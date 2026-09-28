//! The composition-owned direct-terminal grant registry: per-worker leases,
//! stable-id renewal, coalesced refreshes, ACK and post-ACK fencing, device
//! revocation, retirement ordering, and expiry, over fake worker generations
//! and the production pending-RPC correlation.
//! Ports `apps/coord/tests/terminal/direct/terminal-grant-owner.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_direct_grants_support;
mod terminal_direct_support;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use connectrpc::ErrorCode;
use roost_coord::terminal_direct::TerminalDirectRetireReason;
use roost_coord::terminal_direct::grant_owner::TerminalGrantOwner;
use roost_coord::terminal_direct::grant_refresh::PendingTerminalGrant;
use roost_coord::terminal_direct::grant_state::{
    TerminalGrantAuthorization, TerminalGrantInvalidation, TerminalGrantInvalidationKind,
};
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use terminal_direct_grants_support::{Harness, counting, sessions_of};
use terminal_direct_support::{
    DEVICE_FP, OWNER_KEY, SESSION_A, SESSION_B, TAB_ID, WORKER_A, WORKER_B, allow_all, fp,
    grant_request, install_worker, settle_until,
};

// v2 "keeps simultaneous same-tab leases separate by worker".
#[tokio::test]
async fn keeps_simultaneous_same_tab_leases_separate_by_worker() {
    let harness = Harness::new();
    let first = harness.grant_with_ack(&harness.a, &[SESSION_A]).await;
    let second = harness.grant_with_ack(&harness.b, &[SESSION_B]).await;

    assert_ne!(first.lease.grant_id, second.lease.grant_id);
    let leases = harness.owner.list();
    let shape: Vec<(&str, &str, Vec<String>)> = leases
        .iter()
        .map(|lease| {
            (
                lease.worker_fp.as_str(),
                lease.tab_id.as_str(),
                lease.session_ids.clone(),
            )
        })
        .collect();
    assert_eq!(
        shape,
        vec![
            (WORKER_A, TAB_ID, vec![SESSION_A.to_owned()]),
            (WORKER_B, TAB_ID, vec![SESSION_B.to_owned()]),
        ]
    );
}

// v2 "renews on one exact worker with a stable id and rotated browser secret".
#[tokio::test]
async fn renews_on_one_exact_worker_with_a_stable_id_and_rotated_secret() {
    let harness = Harness::new();
    let initial = harness.grant_with_ack(&harness.a, &[SESSION_A]).await;
    let renewal = harness
        .grant_with_ack(&harness.a, &[SESSION_A, SESSION_B])
        .await;

    assert_eq!(renewal.lease.grant_id, initial.lease.grant_id);
    assert_ne!(renewal.secret, initial.secret);
    let frames = harness.a.grants();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[1].grant_id, frames[0].grant_id);
    assert_ne!(frames[1].secret_sha256, frames[0].secret_sha256);
    assert_eq!(
        harness.owner.list()[0].session_ids,
        vec![SESSION_A, SESSION_B]
    );
}

// v2 "coalesces a slow renewal flood into one pending install".
#[tokio::test]
async fn coalesces_a_slow_renewal_flood_into_one_pending_install() {
    let harness = Harness::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let (started, frame) = harness
        .start(
            &harness.a,
            grant_request(WORKER_A, &[SESSION_A], counting(&calls)),
        )
        .await;
    let joined: Vec<PendingTerminalGrant> = (0..512)
        .map(|_| {
            harness
                .owner
                .grant(grant_request(WORKER_A, &[SESSION_A], counting(&calls)))
        })
        .collect();

    assert_eq!(harness.a.grants().len(), 1);
    harness.ack(&harness.a, &frame);
    let result = started.result().await.unwrap();
    for follower in joined {
        let shared = follower.result().await.unwrap();
        assert_eq!(
            shared.secret, result.secret,
            "every caller reads the one install"
        );
    }
    assert_eq!(sessions_of(&result), vec![SESSION_A]);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(harness.owner.list().len(), 1);
}

// v2 "sends one union follow-up when demand grows during an install".
#[tokio::test]
async fn sends_one_union_follow_up_when_demand_grows_during_an_install() {
    let harness = Harness::new();
    let seen: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    let recorder = Arc::clone(&seen);
    let authorize: TerminalGrantAuthorization = Arc::new(move |sessions| {
        recorder.lock().unwrap().push(sessions);
        Box::pin(async { Ok(()) })
    });
    let (started, first) = harness
        .start(
            &harness.a,
            grant_request(WORKER_A, &[SESSION_A], Arc::clone(&authorize)),
        )
        .await;
    let joined = harness
        .owner
        .grant(grant_request(WORKER_A, &[SESSION_B], authorize));

    harness.ack(&harness.a, &first);
    settle_until(|| harness.a.grants().len() == 2).await;
    let follow_up = harness.a.grants()[1].clone();
    assert_eq!(follow_up.grant_id, first.grant_id);
    assert_eq!(follow_up.session_ids, vec![SESSION_A, SESSION_B]);
    harness.ack(&harness.a, &follow_up);
    let result = started.result().await.unwrap();
    assert_eq!(sessions_of(&result), vec![SESSION_A, SESSION_B]);
    assert_eq!(joined.result().await.unwrap().secret, result.secret);
    assert!(
        seen.lock()
            .unwrap()
            .contains(&vec![SESSION_A.to_owned(), SESSION_B.to_owned()])
    );
}

// v2 "preserves the acknowledged predecessor when a renewal install fails".
#[tokio::test]
async fn preserves_the_acknowledged_predecessor_when_a_renewal_install_fails() {
    let harness = Harness::new();
    let initial = harness.grant_with_ack(&harness.a, &[SESSION_A]).await;
    let (renewal, frame) = harness
        .start(
            &harness.a,
            grant_request(WORKER_A, &[SESSION_A, SESSION_B], allow_all()),
        )
        .await;
    assert!(
        harness
            .pending
            .reject(&frame.request_id, "worker refused", Some(WORKER_A))
    );
    let error = renewal.result().await.unwrap_err();

    assert_eq!(error.code, ErrorCode::Internal);
    assert_eq!(frame.grant_id, initial.lease.grant_id);
    let leases = harness.owner.list();
    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0].grant_id, initial.lease.grant_id);
    assert_eq!(leases[0].session_ids, vec![SESSION_A]);
}

// v2 "rebinds a same-process reconnect and narrows scope without changing its id".
#[tokio::test]
async fn rebinds_a_same_process_reconnect_and_narrows_scope_without_changing_its_id() {
    let harness = Harness::new();
    let broad = harness
        .grant_with_ack(&harness.a, &[SESSION_A, SESSION_B])
        .await;
    let reconnected = install_worker(&harness.registry, WORKER_A, Some("epoch-a"), &[]);
    let rebound = harness
        .owner
        .owned_grant(OWNER_KEY, TAB_ID, WORKER_A, &broad.lease.grant_id)
        .expect("a rebound lease");

    assert!(Arc::ptr_eq(&rebound.worker_handle, &reconnected.handle));
    assert_eq!(rebound.session_ids, vec![SESSION_A, SESSION_B]);
    let narrowed = harness.grant_with_ack(&reconnected, &[SESSION_A]).await;
    assert_eq!(narrowed.lease.grant_id, broad.lease.grant_id);
    assert!(Arc::ptr_eq(
        &narrowed.lease.worker_handle,
        &reconnected.handle
    ));
    let frame = &reconnected.grants()[0];
    assert_eq!(frame.grant_id, broad.lease.grant_id);
    assert_eq!(frame.session_ids, vec![SESSION_A]);
    assert_eq!(harness.owner.list()[0].session_ids, vec![SESSION_A]);
}

// v2 "mints a new id only when the worker process epoch changes".
#[tokio::test]
async fn mints_a_new_id_only_when_the_worker_process_epoch_changes() {
    let harness = Harness::new();
    let initial = harness.grant_with_ack(&harness.a, &[SESSION_A]).await;
    let owned = |grant_id: &str| {
        harness
            .owner
            .owned_grant(OWNER_KEY, TAB_ID, WORKER_A, grant_id)
    };
    assert!(owned(&initial.lease.grant_id).is_some());
    let restarted = install_worker(&harness.registry, WORKER_A, Some("epoch-a-restarted"), &[]);
    assert!(owned(&initial.lease.grant_id).is_none());
    let replacement = harness.grant_with_ack(&restarted, &[SESSION_A]).await;

    assert_ne!(replacement.lease.grant_id, initial.lease.grant_id);
    assert_eq!(
        replacement.lease.worker_epoch.as_deref(),
        Some("epoch-a-restarted")
    );
    assert_eq!(restarted.grants()[0].worker_epoch, "epoch-a-restarted");
}

// v2 "rechecks live session authority after ACK before committing a lease".
#[tokio::test]
async fn rechecks_live_session_authority_after_ack_before_committing_a_lease() {
    let harness = Harness::new();
    let authorized = Arc::new(AtomicBool::new(true));
    let checks = Arc::new(AtomicUsize::new(0));
    let (gate, counter) = (Arc::clone(&authorized), Arc::clone(&checks));
    let authorize: TerminalGrantAuthorization = Arc::new(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
        let allowed = gate.load(Ordering::SeqCst);
        Box::pin(async move {
            if allowed {
                Ok(())
            } else {
                Err(connectrpc::ConnectError::new(
                    ErrorCode::NotFound,
                    "session unavailable",
                ))
            }
        })
    });
    let (started, frame) = harness
        .start(&harness.a, grant_request(WORKER_A, &[SESSION_A], authorize))
        .await;
    authorized.store(false, Ordering::SeqCst);
    harness.ack(&harness.a, &frame);
    let error = started.result().await.unwrap_err();

    assert_eq!(error.code, ErrorCode::NotFound);
    assert_eq!(checks.load(Ordering::SeqCst), 2);
    assert!(harness.owner.list().is_empty());
}

// v2 "rejects an ACK that arrives after its captured worker handle changes".
#[tokio::test]
async fn rejects_an_ack_that_arrives_after_its_captured_worker_handle_changes() {
    let harness = Harness::new();
    let (started, frame) = harness
        .start(
            &harness.a,
            grant_request(WORKER_A, &[SESSION_A], allow_all()),
        )
        .await;
    install_worker(
        &harness.registry,
        WORKER_A,
        Some("epoch-a-replaced-before-ack"),
        &[],
    );
    harness.ack(&harness.a, &frame);
    let error = started.result().await.unwrap_err();

    assert_eq!(error.code, ErrorCode::Unavailable);
    assert!(harness.owner.list().is_empty());
}

// v2 "broadcasts device revocation after coordinator lease state is restarted".
#[tokio::test]
async fn broadcasts_device_revocation_after_coordinator_lease_state_is_restarted() {
    let harness = Harness::new();
    harness.grant_with_ack(&harness.a, &[SESSION_A]).await;
    harness.owner.dispose();
    let restarted =
        TerminalGrantOwner::new(Arc::clone(&harness.registry), Arc::clone(&harness.pending));
    restarted.revoke_device(DEVICE_FP).unwrap();

    assert!(restarted.list().is_empty());
    for worker in [&harness.a, &harness.b] {
        let revoked: Vec<String> = worker
            .frames()
            .into_iter()
            .filter_map(|frame| match frame {
                CoordWorkerDownstream::LocalTerminalGrantRevoke(revoke) => {
                    Some(revoke.device_fingerprint)
                }
                _ => None,
            })
            .collect();
        assert_eq!(revoked, vec![DEVICE_FP]);
    }
}

// v2 `terminal-grant-owner.ts` `revokeDevice`: the revoked device's live lease
// is dropped from the coordinator's own record, not only broadcast.
#[tokio::test]
async fn device_revocation_drops_the_devices_live_lease() {
    let harness = Harness::new();
    harness.grant_with_ack(&harness.a, &[SESSION_A]).await;
    assert_eq!(harness.owner.list().len(), 1);

    assert_eq!(harness.owner.revoke_device(DEVICE_FP).unwrap(), 1);
    assert!(harness.owner.list().is_empty());
}

// v2 "retires direct transport before the current worker handle is fenced".
#[tokio::test]
async fn retires_direct_transport_before_the_current_worker_handle_is_fenced() {
    let harness = Harness::new();
    harness.grant_with_ack(&harness.a, &[SESSION_A]).await;
    harness.a.sent.lock().unwrap().clear();
    harness
        .owner
        .retire_worker(WORKER_A, TerminalDirectRetireReason::WorkerDeleted)
        .unwrap();

    let frames = harness.a.frames();
    assert_eq!(frames.len(), 1);
    let CoordWorkerDownstream::TerminalDirectRetire(retire) = &frames[0] else {
        panic!(
            "expected a terminal direct retirement, got {}",
            frames[0].kind()
        );
    };
    assert_eq!(retire.worker_epoch, "epoch-a");
    assert_eq!(retire.reason, "worker_deleted");
    assert!(harness.owner.list().is_empty());
    harness.registry.fence(&fp(WORKER_A));
    assert!(harness.registry.current_routable(&fp(WORKER_A)).is_none());
}

// v2 "expires only its coordinator lease record and notifies subscribers".
#[tokio::test]
async fn expires_only_its_coordinator_lease_record_and_notifies_subscribers() {
    let harness = Harness::new();
    let result = harness.grant_with_ack(&harness.a, &[SESSION_A]).await;
    let events: Arc<Mutex<Vec<TerminalGrantInvalidationKind>>> = Arc::default();
    let recorder = Arc::clone(&events);
    harness
        .owner
        .subscribe_invalidation(Arc::new(move |event: &TerminalGrantInvalidation| {
            recorder.lock().unwrap().push(event.kind);
        }))
        .unwrap();
    harness.owner.sweep(result.lease.expires_at_ms);

    assert!(harness.owner.list().is_empty());
    assert_eq!(
        *events.lock().unwrap(),
        vec![TerminalGrantInvalidationKind::GrantExpired]
    );
}
