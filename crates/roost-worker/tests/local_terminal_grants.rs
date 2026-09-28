//! Grant-store lifecycle: renewal keeps one grant identity, scope growth stays
//! live, scope reduction is reported for carrier closure, expiry actively
//! removes the public scope, and a Hello's credential is checked in v2's order.
//! The test never observes a credential digest. Ports
//! `apps/worker/tests/local-door/local-terminal-grants.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use roost_proto::DLocalTerminalGrant;
use roost_worker::local_terminal::{
    GrantChange, GrantCredential, GrantRemovalReason, LocalTerminalGrantStore,
};
use sha2::{Digest, Sha256};

const DEVICE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TAB: &str = "grant-tab";
const WORKER_EPOCH: &str = "11111111-1111-4111-8111-111111111111";
const SESSION_A: &str = "11111111-1111-4111-8111-111111111111";
const SESSION_B: &str = "22222222-2222-4222-8222-222222222222";
const GRANT_ID: &str = "33333333-3333-4333-8333-333333333333";
const SECRET: &str = "direct-grant-secret";

fn frame(session_ids: &[&str], ttl_ms: u32) -> DLocalTerminalGrant {
    DLocalTerminalGrant {
        request_id: "install".to_owned(),
        grant_id: GRANT_ID.to_owned(),
        secret_sha256: Sha256::digest(SECRET.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        session_ids: session_ids.iter().map(|id| (*id).to_owned()).collect(),
        device_fingerprint: DEVICE.to_owned(),
        tab_id: TAB.to_owned(),
        ttl_ms,
        worker_epoch: WORKER_EPOCH.to_owned(),
        ..Default::default()
    }
}

fn recording(store: &LocalTerminalGrantStore) -> Arc<Mutex<Vec<GrantChange>>> {
    let changes = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&changes);
    store.subscribe(Arc::new(move |change: &GrantChange| {
        sink.lock().unwrap().push(change.clone())
    }));
    changes
}

fn credential<'a>(secret: &'a str, tab_id: &'a str, device: &'a str) -> GrantCredential<'a> {
    GrantCredential {
        grant_id: GRANT_ID,
        secret,
        tab_id,
        device_fingerprint: device,
    }
}

#[tokio::test(start_paused = true)]
async fn renewal_grows_in_place_reports_scope_reduction_and_actively_expires() {
    let store = LocalTerminalGrantStore::new(WORKER_EPOCH, tokio::runtime::Handle::current());
    let changes = recording(&store);

    let initial = store.install(&frame(&[SESSION_A], 100)).unwrap();
    let grown = store.install(&frame(&[SESSION_A, SESSION_B], 100)).unwrap();
    store.install(&frame(&[SESSION_A], 100)).unwrap();

    assert_eq!(initial.grant_id, grown.grant_id);
    assert_eq!(store.current(GRANT_ID).unwrap().session_ids, [SESSION_A]);
    let seen = changes.lock().unwrap().clone();
    assert!(matches!(seen[0], GrantChange::Installed { .. }));
    assert!(
        matches!(&seen[1], GrantChange::Renewed { removed_session_ids, .. } if removed_session_ids.is_empty())
    );
    assert!(
        matches!(&seen[2], GrantChange::Renewed { removed_session_ids, .. } if removed_session_ids == &[SESSION_B])
    );

    tokio::time::sleep(Duration::from_millis(101)).await;
    tokio::task::yield_now().await;

    assert!(
        matches!(
            changes.lock().unwrap().last(),
            Some(GrantChange::Removed {
                reason: GrantRemovalReason::Expired,
                ..
            })
        ),
        "the timer removed it without anyone asking"
    );
    assert!(store.current(GRANT_ID).is_none());
    assert_eq!(
        store.verify(credential(SECRET, TAB, DEVICE)).unwrap_err(),
        "local terminal grant expired"
    );
    store.dispose();
}

#[tokio::test]
async fn a_hello_credential_is_checked_in_v2s_order() {
    let store = LocalTerminalGrantStore::new(WORKER_EPOCH, tokio::runtime::Handle::current());
    assert_eq!(
        store.verify(credential(SECRET, TAB, DEVICE)).unwrap_err(),
        "unknown local terminal grant"
    );
    store.install(&frame(&[SESSION_A], 60_000)).unwrap();

    let other_device = "b".repeat(64);
    assert_eq!(
        store
            .verify(credential("wrong", "other-tab", &other_device))
            .unwrap_err(),
        "grant is bound to another device"
    );
    assert_eq!(
        store
            .verify(credential("wrong", "other-tab", DEVICE))
            .unwrap_err(),
        "grant is bound to another tab"
    );
    assert_eq!(
        store.verify(credential("wrong", TAB, DEVICE)).unwrap_err(),
        "local terminal grant secret mismatch"
    );
    assert_eq!(
        store
            .verify(credential(SECRET, TAB, DEVICE))
            .unwrap()
            .session_ids,
        [SESSION_A]
    );
}

#[tokio::test]
async fn an_install_is_refused_with_v2s_reason() {
    let store = LocalTerminalGrantStore::new(WORKER_EPOCH, tokio::runtime::Handle::current());
    let refused = |mutate: fn(&mut DLocalTerminalGrant)| {
        let mut request = frame(&[SESSION_A], 60_000);
        mutate(&mut request);
        store.install(&request).unwrap_err()
    };
    assert_eq!(
        refused(|grant| grant.secret_sha256 = "ABC".to_owned()),
        "secret_sha256 must be a lowercase hex SHA-256 digest"
    );
    assert_eq!(
        refused(|grant| grant.session_ids.clear()),
        "session_ids is invalid"
    );
    assert_eq!(
        refused(|grant| grant.worker_epoch = "another-epoch".to_owned()),
        "worker_epoch does not match this worker"
    );
    assert_eq!(
        refused(|grant| grant.ttl_ms = 0),
        "ttl_ms must be within 1..86400000"
    );
    store.dispose();
    assert_eq!(
        store.install(&frame(&[SESSION_A], 60_000)).unwrap_err(),
        "local terminal grant store is disposed"
    );
}

#[tokio::test]
async fn revoking_a_device_removes_only_its_grants() {
    let store = LocalTerminalGrantStore::new(WORKER_EPOCH, tokio::runtime::Handle::current());
    let changes = recording(&store);
    store.install(&frame(&[SESSION_A], 60_000)).unwrap();

    store.revoke_device(&"e".repeat(64));
    assert!(
        store.current(GRANT_ID).is_some(),
        "another device's revocation leaves it"
    );
    store.revoke_device(DEVICE);

    assert!(store.current(GRANT_ID).is_none());
    assert!(matches!(
        changes.lock().unwrap().last(),
        Some(GrantChange::Removed {
            reason: GrantRemovalReason::Revoked,
            ..
        })
    ));
    assert_eq!(
        store.verify(credential(SECRET, TAB, DEVICE)).unwrap_err(),
        "unknown local terminal grant"
    );
}
