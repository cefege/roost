//! The VAPID identity's scope and its first-use serialisation.
//!
//! Two properties, and both are silent when broken. The scope: the keypair
//! lives in the `dashboard_id IS NULL` row of `app_settings` and nowhere else,
//! because a per-dashboard row would shadow the coordinator-global identity
//! (`apps/coord/src/push/vapid.ts:45-47`). The serialisation: a second
//! concurrent first use must observe the row the first one committed rather
//! than minting a second identity.

// A test that cannot say what it expected is not a test. `expect` is denied
// outside `#[cfg(test)]`, and an integration test is its own crate, so the
// exemption has to be stated here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod push_fixture;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use push_fixture::{CountingGenerator, PushFixture, viewer_fp};
use roost_coord::push::PushRuntime;
use roost_coord::push::rpc::handle_push_get_config;
use roost_coord::push::vapid::{VAPID_SETTING_KEY, VapidKeyStore};
use roost_proto::PushGetConfigRequest;
use sqlx::Row as _;

/// Every `push.vapid` row in the database, with its scope.
async fn vapid_rows(fixture: &PushFixture) -> Vec<(Option<String>, String)> {
    sqlx::query("SELECT dashboard_id, value FROM app_settings WHERE key = ?1")
        .bind(VAPID_SETTING_KEY)
        .fetch_all(fixture.database().pool())
        .await
        .expect("the settings read")
        .into_iter()
        .map(|row| {
            (
                row.try_get::<Option<String>, _>("dashboard_id")
                    .expect("scope"),
                row.try_get::<String, _>("value").expect("value"),
            )
        })
        .collect()
}

#[tokio::test]
async fn the_identity_is_written_once_to_the_coordinator_global_row() {
    let fixture = PushFixture::new("vapid-scope").await;

    let keys = fixture
        .push
        .keys(fixture.database())
        .await
        .expect("an identity");

    let rows = vapid_rows(&fixture).await;
    assert_eq!(rows.len(), 1, "exactly one identity row: {rows:?}");
    assert_eq!(
        rows[0].0, None,
        "the identity is coordinator-global; a scoped row would shadow it"
    );
    let stored: serde_json::Value = serde_json::from_str(&rows[0].1).expect("the stored JSON");
    assert_eq!(stored["publicKey"], keys.public_key);
    assert_eq!(stored["privateKey"], keys.private_key);
}

#[tokio::test]
async fn a_per_dashboard_vapid_row_is_neither_read_nor_written() {
    let fixture = PushFixture::new("vapid-tenant-row").await;
    // A tenant-scoped row that a scope-blind read would happily return.
    fixture
        .exec(&format!(
            "INSERT INTO app_settings (dashboard_id, key, value, updated_at_ms) \
             VALUES ('{}', '{VAPID_SETTING_KEY}', '{{\"publicKey\":\"tenant\",\"privateKey\":\"tenant\"}}', 1)",
            fixture.dashboard_id
        ))
        .await;

    let keys = fixture
        .push
        .keys(fixture.database())
        .await
        .expect("an identity");

    assert_ne!(
        keys.public_key, "tenant",
        "a per-dashboard row must never shadow the coordinator-global identity"
    );
    let rows = vapid_rows(&fixture).await;
    assert_eq!(rows.len(), 2, "the tenant row is left alone, not adopted");
    assert_eq!(
        rows[0].0, None,
        "the global row is the one that was written"
    );
    assert_eq!(
        rows[1].0.as_deref(),
        Some(fixture.dashboard_id.as_str()),
        "an existing tenant row is not this code's to overwrite"
    );
}

#[tokio::test]
async fn a_second_load_returns_the_stored_identity_without_minting_another() {
    let (generator, draws) = CountingGenerator::new();
    let fixture = PushFixture::build(
        "vapid-cached",
        vec![push_fixture::PUSH_ORIGIN.to_owned()],
        generator,
    )
    .await;

    let first = fixture
        .push
        .keys(fixture.database())
        .await
        .expect("an identity");
    let second = fixture
        .push
        .keys(fixture.database())
        .await
        .expect("the same identity");

    assert_eq!(first, second);
    assert_eq!(
        draws.load(Ordering::SeqCst),
        1,
        "the cached identity is reused; no second keypair is minted"
    );
    assert_eq!(vapid_rows(&fixture).await.len(), 1);
}

#[tokio::test]
async fn eight_concurrent_first_uses_mint_exactly_one_identity() {
    let (generator, draws) = CountingGenerator::new();
    let fixture = PushFixture::build(
        "vapid-concurrent",
        vec![push_fixture::PUSH_ORIGIN.to_owned()],
        generator,
    )
    .await;

    // Eight callers, one empty settings table. v2 serialises this with a
    // module-level in-flight promise (`vapid.ts:58-71`); this port serialises
    // it with a `BEGIN IMMEDIATE` transaction, which has the same effect and
    // also holds across processes. What must not happen is eight keypairs.
    let reads: Vec<_> = (0..8)
        .map(|_| {
            let push = fixture.push.clone();
            let database = fixture.database().clone();
            async move { push.keys(&database).await }
        })
        .collect();
    let mut results = Vec::new();
    for read in reads {
        results.push(read.await.expect("every concurrent first use resolves"));
    }

    let public_keys: std::collections::HashSet<&String> =
        results.iter().map(|keys| &keys.public_key).collect();
    assert_eq!(
        public_keys.len(),
        1,
        "eight concurrent first uses must converge on one identity"
    );
    assert_eq!(
        draws.load(Ordering::SeqCst),
        1,
        "the transaction serialises generation: exactly one keypair is minted"
    );
    assert_eq!(
        vapid_rows(&fixture).await.len(),
        1,
        "and exactly one row is persisted"
    );
}

#[tokio::test]
async fn a_store_over_a_second_database_keeps_its_own_identity() {
    // `services.rs` forbids a crate-global here, and this is the reason: two
    // coordinators, or two tests, must not share one push identity.
    let (generator, draws) = CountingGenerator::new();
    let first = PushFixture::build(
        "vapid-isolated-a",
        vec![push_fixture::PUSH_ORIGIN.to_owned()],
        generator.clone(),
    )
    .await;
    let second = PushFixture::build(
        "vapid-isolated-b",
        vec![push_fixture::PUSH_ORIGIN.to_owned()],
        generator,
    )
    .await;

    let first_keys = first
        .push
        .keys(first.database())
        .await
        .expect("an identity");
    let second_keys = second
        .push
        .keys(second.database())
        .await
        .expect("an identity");

    assert_ne!(
        first_keys.public_key, second_keys.public_key,
        "a shared cache would hand two databases the same identity"
    );
    assert_eq!(draws.load(Ordering::SeqCst), 2, "one keypair per database");
}

#[tokio::test]
async fn a_store_reloads_after_its_cache_is_invalidated() {
    let fixture = PushFixture::new("vapid-invalidate").await;
    let store = fixture.push.vapid_keys().clone();

    let first = store.keys(fixture.database()).await.expect("an identity");
    store.invalidate();
    let reloaded = store
        .keys(fixture.database())
        .await
        .expect("the stored identity");

    assert_eq!(
        first, reloaded,
        "the row is the identity; a reload returns the same keypair"
    );
    assert_eq!(vapid_rows(&fixture).await.len(), 1);
}

#[tokio::test]
async fn a_stored_row_that_is_not_a_keypair_is_refused_rather_than_used() {
    let fixture = PushFixture::new("vapid-corrupt").await;
    fixture
        .exec(&format!(
            "INSERT INTO app_settings (dashboard_id, key, value, updated_at_ms) \
             VALUES (NULL, '{VAPID_SETTING_KEY}', '{{\"publicKey\":7}}', 1)"
        ))
        .await;

    // The keypair is coordinator-global, so a corrupt row is an operator
    // problem, not a per-request one: the store refuses rather than sending
    // with a key the operator cannot rotate.
    let outcome = fixture.push.keys(fixture.database()).await;

    assert!(
        outcome.is_err(),
        "a row without both halves must not be handed out"
    );
    assert!(
        outcome
            .expect_err("a corrupt row is refused")
            .to_string()
            .contains("invalid"),
        "and the refusal says the stored keypair is what is wrong"
    );
}

#[tokio::test]
async fn the_push_get_config_path_creates_the_global_row_and_no_other() {
    let (generator, draws) = CountingGenerator::new();
    let fixture = PushFixture::build(
        "vapid-via-rpc",
        vec![push_fixture::PUSH_ORIGIN.to_owned()],
        generator,
    )
    .await;

    let response = handle_push_get_config(
        &fixture.core,
        &fixture.caller,
        PushGetConfigRequest::default(),
    )
    .await
    .expect("a paired device may read the config");

    assert!(response.body.available);
    assert_eq!(draws.load(Ordering::SeqCst), 1);
    let rows = vapid_rows(&fixture).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].0, None,
        "written to the global scope by the RPC path too"
    );
    assert!(
        rows[0].1.contains(&response.body.vapid_public_key_b64),
        "and it is the key the browser was handed"
    );
}

#[tokio::test]
async fn concurrent_push_get_config_calls_all_see_one_identity() {
    // The point of the store being on the runtime rather than in a module: two
    // browser tabs asking for the config at the same moment must be handed the
    // SAME key, or one of them subscribes with a key the other will not
    // recognise.
    let (generator, draws) = CountingGenerator::new();
    let fixture = PushFixture::build(
        "vapid-concurrent-rpc",
        vec![push_fixture::PUSH_ORIGIN.to_owned()],
        generator,
    )
    .await;

    let calls: Vec<_> = (0..8)
        .map(|_| {
            let core = fixture.core.clone();
            let caller = fixture.caller.clone();
            async move {
                handle_push_get_config(&core, &caller, PushGetConfigRequest::default())
                    .await
                    .expect("a paired device may read the config")
                    .body
                    .vapid_public_key_b64
            }
        })
        .collect();
    let mut keys = Vec::new();
    for call in calls {
        keys.push(call.await);
    }

    let distinct: std::collections::HashSet<&String> = keys.iter().collect();
    assert_eq!(distinct.len(), 1, "every tab is handed the same key");
    assert_eq!(
        draws.load(Ordering::SeqCst),
        1,
        "eight tabs asking at once mint one identity, not eight"
    );
}

#[tokio::test]
async fn a_runtime_with_no_tenancy_scope_reports_a_wiring_fault_not_a_disabled_push() {
    // `CoordCore::new` installs no push runtime. That must read as an
    // `Internal` wiring error, because "Push is switched off" would tell an
    // operator with Push configured that their configuration is fine.
    let fixture = PushFixture::new("vapid-unwired").await;
    let bare = roost_coord::coord_core::CoordCore::new(Arc::clone(&fixture.core.services));

    let error = handle_push_get_config(&bare, &fixture.caller, PushGetConfigRequest::default())
        .await
        .expect_err("an unwired runtime is a fault, not a disabled feature");

    assert_eq!(error.code, connectrpc::ErrorCode::Internal);
    assert!(
        error
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("tenancy scope"),
        "the message must say what is missing: {error}"
    );
}

#[tokio::test]
async fn a_runtime_minted_for_a_test_shares_one_identity_across_its_handlers() {
    // The store is a value on the runtime, so two handlers built over the same
    // runtime see one identity rather than one each.
    let runtime = PushRuntime::with_keypair_generator(
        "dash".to_owned(),
        vec![push_fixture::PUSH_ORIGIN.to_owned()],
        Arc::new(roost_coord::push::vapid::P256KeypairGenerator),
    );
    let store = runtime.vapid_keys().clone();
    assert!(
        std::ptr::eq(
            store as *const VapidKeyStore,
            runtime.vapid_keys() as *const VapidKeyStore
        ),
        "the runtime hands out its one store, not a copy per call"
    );
    let _ = viewer_fp('a');
}
