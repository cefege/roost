//! The smoke harness's grant-store seams: advancing the grant clock fences a
//! lapsed grant at once while a grant installed afterwards still lives its
//! whole TTL, and shrinking a session out renews the same grant identity
//! without it — or removes a grant nothing would remain in. Pins
//! `local_terminal::grants::test_seams` (v2 `local-terminal-grants.ts`
//! `_sweepExpiredForTest`, `_shrinkSessionForTest`).
#![cfg(feature = "smoke")]
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
const OTHER_GRANT_ID: &str = "44444444-4444-4444-8444-444444444444";
const SECRET: &str = "direct-grant-secret";

fn frame(grant_id: &str, session_ids: &[&str], ttl_ms: u32) -> DLocalTerminalGrant {
    DLocalTerminalGrant {
        request_id: "install".to_owned(),
        grant_id: grant_id.to_owned(),
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

#[tokio::test(start_paused = true)]
async fn an_advanced_clock_fences_at_once_and_a_later_grant_keeps_its_ttl() {
    let store = LocalTerminalGrantStore::new(WORKER_EPOCH, tokio::runtime::Handle::current());
    let changes = recording(&store);
    store
        .install(&frame(GRANT_ID, &[SESSION_A], 60_000))
        .unwrap();

    store.advance_clock(60_001).unwrap();

    assert!(
        matches!(
            changes.lock().unwrap().last(),
            Some(GrantChange::Removed {
                reason: GrantRemovalReason::Expired,
                ..
            })
        ),
        "the advance itself swept the lapsed grant"
    );
    let credential = GrantCredential {
        grant_id: GRANT_ID,
        secret: SECRET,
        tab_id: TAB,
        device_fingerprint: DEVICE,
    };
    assert_eq!(
        store.verify(credential).unwrap_err(),
        "local terminal grant expired"
    );

    store
        .install(&frame(GRANT_ID, &[SESSION_A], 60_000))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(59_000)).await;
    assert!(
        store.current(GRANT_ID).is_some(),
        "a grant installed on the advanced clock lives its whole TTL"
    );
    tokio::time::sleep(Duration::from_millis(1_001)).await;
    tokio::task::yield_now().await;
    assert!(store.current(GRANT_ID).is_none());
    assert_eq!(
        store.advance_clock(0).unwrap_err(),
        "grant clock advance must be a positive safe integer"
    );
    store.dispose();
}

#[tokio::test(start_paused = true)]
async fn shrinking_a_session_renews_the_same_grant_or_clears_an_emptied_one() {
    let store = LocalTerminalGrantStore::new(WORKER_EPOCH, tokio::runtime::Handle::current());
    store
        .install(&frame(GRANT_ID, &[SESSION_A, SESSION_B], 60_000))
        .unwrap();
    store
        .install(&frame(OTHER_GRANT_ID, &[SESSION_A], 60_000))
        .unwrap();
    let changes = recording(&store);
    tokio::time::sleep(Duration::from_millis(10_000)).await;

    assert_eq!(store.shrink_session(SESSION_A), 2);

    let renewed = store.current(GRANT_ID).unwrap();
    assert_eq!(renewed.session_ids, [SESSION_B]);
    assert!(store.current(OTHER_GRANT_ID).is_none());
    let seen = changes.lock().unwrap().clone();
    assert!(seen.iter().any(|change| matches!(
        change,
        GrantChange::Renewed { grant, removed_session_ids }
            if grant.grant_id == GRANT_ID && removed_session_ids == &[SESSION_A]
    )));
    assert!(seen.iter().any(|change| matches!(
        change,
        GrantChange::Removed { grant, reason: GrantRemovalReason::Cleared }
            if grant.grant_id == OTHER_GRANT_ID
    )));
    let credential = GrantCredential {
        grant_id: GRANT_ID,
        secret: SECRET,
        tab_id: TAB,
        device_fingerprint: DEVICE,
    };
    assert!(store.verify(credential).is_ok(), "the secret survives");

    tokio::time::sleep(Duration::from_millis(50_001)).await;
    tokio::task::yield_now().await;
    assert!(
        store.current(GRANT_ID).is_none(),
        "the renewal keeps the remaining TTL, not a fresh one"
    );
    assert_eq!(store.shrink_session(SESSION_B), 0);
    store.dispose();
}
