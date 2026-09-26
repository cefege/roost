//! What the keeper-update preparation refuses, and the reason it gives.
//!
//! Each refusal is a fact an operator can act on, and each code keeps a DECISION
//! apart from a TRANSPORT FAULT: `Unavailable` is a worker that never answered,
//! `DataLoss` is a proof the coordinator could not place, and neither is ever an
//! unprovable keeper. Drives the real handler over a real database.

#[path = "keeper_update_support/mod.rs"]
mod keeper_support;
#[path = "workers_support/mod.rs"]
mod workers_support;

use keeper_support::{
    PrepareRequest, digest, enroll_device, journal, journaled, maintenance, prepare,
};
use roost_coord::deploy::keeper_update::handle_workers_prepare_keeper_update;
use serde_json::Value;
use workers_support::{WORKER_FP, WorkersFixture, device_caller, worker_caller};

use connectrpc::ErrorCode;

#[tokio::test]
async fn a_replace_empty_across_a_live_session_is_refused_with_an_actionable_reason() {
    let fixture = WorkersFixture::new("keeper-refusal").await;
    enroll_device(&fixture).await;
    fixture
        .enroll_worker(WORKER_FP, "refusal-worker", 1_000)
        .await;
    // A live PTY the coordinator can see is what makes a replace-empty
    // inadmissible rather than merely unusual.
    fixture
        .enroll_session(WORKER_FP, workers_support::SESSION_ID)
        .await;

    let refusal = handle_workers_prepare_keeper_update(
        &fixture.core,
        &device_caller(),
        prepare(WORKER_FP, journaled("replace-empty", "target")),
    )
    .await
    .expect_err("a replace-empty across a live session is never admissible");

    assert_eq!(
        refusal.message.as_deref(),
        Some("keeper replacement blocked by live sessions"),
        "the reason names the obstacle, not the code",
    );
    assert_eq!(refusal.code, ErrorCode::FailedPrecondition);
}

#[tokio::test]
async fn a_maintenance_shutdown_names_a_different_obstacle_than_a_replacement() {
    let fixture = WorkersFixture::new("keeper-maint").await;
    enroll_device(&fixture).await;
    fixture
        .enroll_worker(WORKER_FP, "maint-worker", 1_000)
        .await;
    fixture
        .enroll_session(WORKER_FP, workers_support::SESSION_ID)
        .await;

    let refusal = handle_workers_prepare_keeper_update(
        &fixture.core,
        &device_caller(),
        prepare(WORKER_FP, maintenance()),
    )
    .await
    .expect_err("an unauthorized maintenance shutdown crosses live sessions");

    assert_eq!(
        refusal.message.as_deref(),
        Some("keeper maintenance blocked by live sessions"),
        "an operator reading this knows the ask was a maintenance, not a release",
    );
    assert_eq!(refusal.code, ErrorCode::FailedPrecondition);
}

#[tokio::test]
async fn a_journal_naming_an_unprovably_empty_keeper_is_refused() {
    let fixture = WorkersFixture::new("keeper-badjournal").await;
    enroll_device(&fixture).await;
    fixture
        .enroll_worker(WORKER_FP, "badjournal-worker", 1_000)
        .await;

    // A `replace-empty` naming a binding digest other than the canonical empty
    // one: the keeper is not provably empty, so the admission asserts something
    // the host never proved.
    let mut value: Value = serde_json::from_str(&journal("replace-empty")).expect("a journal");
    value["admission"]["expected_binding_digest"] = Value::String(digest("d"));
    let refusal = handle_workers_prepare_keeper_update(
        &fixture.core,
        &device_caller(),
        prepare(
            WORKER_FP,
            PrepareRequest {
                journaled_update_json: Some(value.to_string()),
                direction: "target".to_owned(),
                ..PrepareRequest::default()
            },
        ),
    )
    .await
    .expect_err("a keeper that is not provably empty may not be replaced");

    assert_eq!(
        refusal.message.as_deref(),
        Some("journaled keeper update is malformed"),
    );
    assert_eq!(refusal.code, ErrorCode::InvalidArgument);
}

#[tokio::test]
async fn a_journal_whose_contracts_contradict_its_classification_is_refused() {
    let fixture = WorkersFixture::new("keeper-class").await;
    enroll_device(&fixture).await;
    fixture
        .enroll_worker(WORKER_FP, "class-worker", 1_000)
        .await;

    // A journal that claims `worker-only-safe` while carrying a different target
    // binary is a journal edited to skip the replacement it actually needs.
    let mut value: Value = serde_json::from_str(&journal("preserve")).expect("a journal");
    value["target_contract"]["implementation_digest"] = Value::String(digest("e"));
    let refusal = handle_workers_prepare_keeper_update(
        &fixture.core,
        &device_caller(),
        prepare(
            WORKER_FP,
            PrepareRequest {
                journaled_update_json: Some(value.to_string()),
                direction: "source".to_owned(),
                ..PrepareRequest::default()
            },
        ),
    )
    .await
    .expect_err("a classification its own contracts contradict proves nothing");

    assert_eq!(
        refusal.message.as_deref(),
        Some("journaled keeper update is malformed"),
    );
}

#[tokio::test]
async fn a_replacement_may_not_arrive_without_the_envelope_that_authorizes_it() {
    let fixture = WorkersFixture::new("keeper-nojournal").await;
    enroll_device(&fixture).await;
    fixture
        .enroll_worker(WORKER_FP, "nojournal-worker", 1_000)
        .await;

    let refusal = handle_workers_prepare_keeper_update(
        &fixture.core,
        &device_caller(),
        prepare(
            WORKER_FP,
            PrepareRequest {
                direction: "target".to_owned(),
                ..PrepareRequest::default()
            },
        ),
    )
    .await
    .expect_err("nothing proves what may be replaced");
    assert_eq!(
        refusal.message.as_deref(),
        Some("journaled keeper update is required"),
    );
}

#[tokio::test]
async fn a_maintenance_request_may_not_carry_a_journaled_update() {
    let fixture = WorkersFixture::new("keeper-both").await;
    enroll_device(&fixture).await;
    fixture.enroll_worker(WORKER_FP, "both-worker", 1_000).await;

    let refusal = handle_workers_prepare_keeper_update(
        &fixture.core,
        &device_caller(),
        prepare(
            WORKER_FP,
            PrepareRequest {
                maintenance: true,
                ..journaled("replace-empty", "target")
            },
        ),
    )
    .await
    .expect_err("a maintenance shutdown and a replacement are two different asks");
    assert_eq!(
        refusal.message.as_deref(),
        Some("keeper maintenance cannot carry a journaled update"),
    );
}

#[tokio::test]
async fn force_live_never_rides_a_journaled_envelope() {
    let fixture = WorkersFixture::new("keeper-forcejournal").await;
    enroll_device(&fixture).await;
    fixture
        .enroll_worker(WORKER_FP, "forcej-worker", 1_000)
        .await;

    // force_live authorizes destroying live PTYs, so it must never arrive on the
    // path that carries a replayed or hand-edited journal.
    let refusal = handle_workers_prepare_keeper_update(
        &fixture.core,
        &device_caller(),
        prepare(
            WORKER_FP,
            PrepareRequest {
                force_live: true,
                ..journaled("replace-empty", "target")
            },
        ),
    )
    .await
    .expect_err("force-live never rides a journaled envelope");
    assert_eq!(
        refusal.message.as_deref(),
        Some("keeper force-live requires the maintenance path"),
    );
}

#[tokio::test]
async fn a_worker_key_may_not_prepare_a_keeper_update() {
    let fixture = WorkersFixture::new("keeper-auth").await;
    fixture.enroll_worker(WORKER_FP, "auth-worker", 1_000).await;

    let refusal = handle_workers_prepare_keeper_update(
        &fixture.core,
        &worker_caller(WORKER_FP),
        prepare(WORKER_FP, maintenance()),
    )
    .await
    .expect_err("a machine may not destroy a fleet's terminals");

    assert_eq!(refusal.code, ErrorCode::Unauthenticated);
    assert_eq!(refusal.message.as_deref(), Some("authentication required"));
}

#[tokio::test]
async fn a_caller_the_coordinator_can_no_longer_resolve_may_not_proceed() {
    let fixture = WorkersFixture::new("keeper-stale").await;
    fixture
        .enroll_worker(WORKER_FP, "stale-worker", 1_000)
        .await;
    // The `Caller` claims an account device, but the re-read inside the drain
    // finds no key row: a credential revoked between the interceptor's resolve
    // and here must not proceed on the stale admission.
    let refusal = handle_workers_prepare_keeper_update(
        &fixture.core,
        &device_caller(),
        prepare(WORKER_FP, maintenance()),
    )
    .await
    .expect_err("a key with no row behind it is not a credential");

    assert_eq!(refusal.code, ErrorCode::Unauthenticated);
    assert_eq!(refusal.message.as_deref(), Some("authentication required"));
}

#[tokio::test]
async fn a_tombstoned_worker_is_not_preparable() {
    let fixture = WorkersFixture::new("keeper-tombstone").await;
    enroll_device(&fixture).await;
    fixture
        .enroll_worker(WORKER_FP, "tombstone-worker", 1_000)
        .await;
    fixture
        .exec(&format!(
            "UPDATE workers SET deleted_at_ms = 2 WHERE fp = '{WORKER_FP}'"
        ))
        .await;

    let refusal = handle_workers_prepare_keeper_update(
        &fixture.core,
        &device_caller(),
        prepare(WORKER_FP, maintenance()),
    )
    .await
    .expect_err("a deleted machine is not a keeper to replace");
    assert_eq!(refusal.message.as_deref(), Some("worker not found"));
    assert_eq!(refusal.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn throwaway_probe_why_the_fixture_journal_is_refused() {
    let raw = keeper_support::journal("replace-empty");
    let value: serde_json::Value = serde_json::from_str(&raw).expect("the fixture emits JSON");
    match roost_protocol::keeper_update::JournaledKeeperUpdateV1::parse(&value) {
        Ok(_) => println!("PROBE with_bun_abi: PARSES CLEAN"),
        Err(error) => println!("PROBE with_bun_abi: {error:?}"),
    }
    let mut stripped = value.clone();
    for key in ["source_contract", "target_contract"] {
        stripped[key]
            .as_object_mut()
            .expect("both contracts are objects")
            .remove("bun_abi");
    }
    match roost_protocol::keeper_update::JournaledKeeperUpdateV1::parse(&stripped) {
        Ok(_) => println!("PROBE without_bun_abi: PARSES CLEAN"),
        Err(error) => println!("PROBE without_bun_abi: {error:?}"),
    }
}
