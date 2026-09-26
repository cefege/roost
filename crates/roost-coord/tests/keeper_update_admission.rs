//! The keeper-update preparation: the drain it holds, the runtime it refuses,
//! and the reasons it gives.
//!
//! Drives the real handler over a real database and a real write gate, with a
//! worker socket that records what it was handed and answers the preparation
//! the way the link's frame dispatcher answers `rpc-ok`. Every assertion is
//! about the DECISION this RPC makes, because that is the only thing standing
//! between an operator and a fleet's live PTYs.

#[path = "workers_support/mod.rs"]
mod support;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use connectrpc::ErrorCode;
use roost_coord::coord_core::worker_handle::WorkerHandle;
use roost_coord::deploy::keeper_update::{
    MAINTENANCE_ACTION, KeeperUpdateAction, KeeperUpdateRefusal, KeeperUpdateRequest,
    decide_keeper_update, handle_workers_prepare_keeper_update,
};
use roost_coord::workers::registry::{claim_generation, mark_generation_ready};
use roost_coord::write_gate::WriteGate;
use roost_proto::WorkersPrepareKeeperUpdateRequest as PrepareRequest;
use roost_protocol::keeper_update::KEEPER_EMPTY_BINDING_DIGEST;
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use serde_json::{Value, json};
use support::{DEVICE_FP, WORKER_FP, WorkersFixture, device_caller, worker_caller};

/// A keeper epoch the shared contract accepts: a version-4 uuid.
const EPOCH: &str = "0f9a5c1e-3b2d-4a7f-9c11-5d6e7f801122";

/// A digest the contract accepts: 64 lowercase hex characters.
fn digest(byte: &str) -> String {
    format!("{byte}{}", "0".repeat(63))
}

/// A journal the shared admission contract accepts for the action asked for.
///
/// `worker-only-safe` carries equal source and target digests, which is what
/// the contract demands of a `preserve`; `keeper-restart-required` names the
/// canonical empty binding digest, which is what it demands of a
/// `replace-empty`. Both halves are load-bearing, so a test that wants an
/// inadmissible journal changes one of them on purpose.
fn journal(action: &str) -> String {
    let (classification, target, binding) = if action == "preserve" {
        ("worker-only-safe", "a", digest("a"))
    } else {
        ("keeper-restart-required", "b", KEEPER_EMPTY_BINDING_DIGEST.to_owned())
    };
    let contract = |implementation: &str| {
        json!({
            "protocol_version": 1,
            "supported_features": ["events-v1"],
            "required_features": [],
            "implementation_digest": implementation,
            "bun_abi": "1.2.3",
            "platform": "linux",
            "arch": "x64",
            "build_sha": "deadbeef",
        })
    };
    let source = digest("a");
    json!({
        "admission": {
            "classification": classification,
            "source_contract_digest": source,
            "target_contract_digest": digest(&target),
            "expected_keeper_pid": 4242,
            "expected_keeper_epoch": EPOCH,
            "expected_binding_digest": binding,
            "required_action": action,
        },
        "source_contract": contract(&source),
        "target_contract": contract(&digest(&target)),
    })
    .to_string()
}

fn prepare(worker_fp: &str, body: PrepareRequest) -> PrepareRequest {
    PrepareRequest {
        worker_fp: worker_fp.to_owned(),
        ..body
    }
}

fn maintenance() -> PrepareRequest {
    PrepareRequest {
        maintenance: true,
        ..PrepareRequest::default()
    }
}

fn journaled(action: &str, direction: &str) -> PrepareRequest {
    PrepareRequest {
        journaled_update_json: Some(journal(action)),
        direction: direction.to_owned(),
        ..PrepareRequest::default()
    }
}

/// The operator device the handler re-authorizes inside the drain.
async fn enroll_device(fixture: &WorkersFixture) {
    let key = "00".repeat(32);
    fixture
        .exec(&format!(
            "INSERT INTO authorized_keys (fingerprint, public_key, label, added_at) \
             VALUES ('{DEVICE_FP}', X'{key}', 'operator', 1)"
        ))
        .await;
    fixture
        .exec(&format!(
            "INSERT INTO account_devices (fingerprint, account_id, added_at_ms, last_seen_at_ms) \
             SELECT '{DEVICE_FP}', id, 1, 1 FROM accounts LIMIT 1"
        ))
        .await;
}

/// What the socket saw at the instant the frame was handed to it.
#[derive(Debug, Clone, Default)]
struct Observed {
    exclusive_held: Vec<bool>,
    write_refused: Vec<bool>,
    frames: Vec<CoordWorkerDownstream>,
}


/// Claim a worker generation whose socket records the drain's state and answers
/// the preparation.
fn connect_observer(fixture: &WorkersFixture, reply: Value, observed: Arc<Mutex<Observed>>) {
    let pending = Arc::clone(&fixture.core.services.scrollback.pending());
    let gate = fixture.core.services.write_gate();
    let sender: Arc<dyn Fn(CoordWorkerDownstream) -> i64 + Send + Sync> =
        Arc::new(move |frame: CoordWorkerDownstream| {
            let mut seen = observed.lock().unwrap_or_else(|error| error.into_inner());
            // Sampled HERE, inside the send, because this is the instant the
            // handover happens: a mutation refused now is a PTY that cannot
            // appear between the decision and the worker's shutdown.
            seen.exclusive_held.push(gate.exclusive_held());
            seen.write_refused.push(gate.acquire_shared().is_err());
            seen.frames.push(frame.clone());
            drop(seen);
            if let CoordWorkerDownstream::KeeperUpdatePrepare(body) = &frame {
                pending.resolve(&body.request_id, reply.clone(), Some(WORKER_FP));
            }
            1
        });
    let handle = Arc::new(WorkerHandle::new(
        WorkerFp::try_from(WORKER_FP).expect("a fingerprint the brand accepts"),
        Some("epoch-1".to_owned()),
        "gen-1".to_owned(),
        BTreeSet::new(),
        sender,
    ));
    claim_generation(
        &fixture.core.services.buses,
        &fixture.core.services.workers,
        Arc::clone(&handle),
    );
    mark_generation_ready(
        &fixture.core.services.buses,
        &fixture.core.services.workers,
        &handle,
    );
}

/// The recorded frames and the gate's state at each of them.
fn observed_by(observed: &Arc<Mutex<Observed>>) -> Observed {
    observed.lock().unwrap_or_else(|error| error.into_inner()).clone()
}

#[tokio::test]
async fn a_replace_empty_across_a_live_session_is_refused_with_an_actionable_reason() {
    let fixture = WorkersFixture::new("keeper-refusal").await;
    enroll_device(&fixture).await;
    fixture.enroll_worker(WORKER_FP, "refusal-worker", 1_000).await;
    // A live PTY the coordinator can see is what makes a replace-empty
    // inadmissible rather than merely unusual.
    fixture.enroll_session(WORKER_FP, support::SESSION_ID).await;

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
        "the reason names the obstacle, not the code"
    );
    assert_eq!(refusal.code, ErrorCode::FailedPrecondition);
}

#[tokio::test]
async fn a_maintenance_shutdown_names_a_different_obstacle_than_a_replacement() {
    let fixture = WorkersFixture::new("keeper-maint").await;
    enroll_device(&fixture).await;
    fixture.enroll_worker(WORKER_FP, "maint-worker", 1_000).await;
    fixture.enroll_session(WORKER_FP, support::SESSION_ID).await;

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
        "an operator reading this knows the ask was a maintenance, not a release"
    );
    assert_eq!(refusal.code, ErrorCode::FailedPrecondition);
}

#[tokio::test]
async fn a_journal_naming_an_unprovably_empty_keeper_is_refused() {
    let fixture = WorkersFixture::new("keeper-badjournal").await;
    enroll_device(&fixture).await;
    fixture.enroll_worker(WORKER_FP, "badjournal-worker", 1_000).await;

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
        Some("journaled keeper update is malformed")
    );
    assert_eq!(refusal.code, ErrorCode::InvalidArgument);
}

#[tokio::test]
async fn a_worker_proof_for_a_different_action_is_refused() {
    let fixture = WorkersFixture::new("keeper-mismatch").await;
    enroll_device(&fixture).await;
    fixture.enroll_worker(WORKER_FP, "mismatch-worker", 1_000).await;
    let seen = Arc::new(Mutex::new(Observed::default()));
    connect_observer(
        &fixture,
        json!({
            "outcome": "preserved",
            "keeper_pid": 4242,
            "keeper_epoch": EPOCH,
            "binding_digest": digest("c"),
        }),
        Arc::clone(&seen),
    );

    // A replace-empty asks the worker to shut the keeper down. A `preserved`
    // answer means no replacement happened, and reporting it as a success is
    // how a fleet's PTYs are lost.
    let refusal = handle_workers_prepare_keeper_update(
        &fixture.core,
        &device_caller(),
        prepare(WORKER_FP, journaled("replace-empty", "target")),
    )
    .await
    .expect_err("a preserved answer satisfies no replacement");

    assert_eq!(
        refusal.message.as_deref(),
        Some("worker returned keeper proof for a different action")
    );
    assert_eq!(refusal.code, ErrorCode::DataLoss);
}

#[tokio::test]
async fn a_preserved_keeper_must_name_the_process_holding_the_ptys() {
    let fixture = WorkersFixture::new("keeper-identity").await;
    enroll_device(&fixture).await;
    fixture.enroll_worker(WORKER_FP, "identity-worker", 1_000).await;
    let seen = Arc::new(Mutex::new(Observed::default()));
    connect_observer(
        &fixture,
        json!({ "outcome": "preserved", "keeper_pid": 4242 }),
        Arc::clone(&seen),
    );

    let refusal = handle_workers_prepare_keeper_update(
        &fixture.core,
        &device_caller(),
        prepare(WORKER_FP, journaled("preserve", "source")),
    )
    .await
    .expect_err("a pid with no epoch and no binding names no keeper");

    assert_eq!(
        refusal.message.as_deref(),
        Some("worker returned malformed keeper identity")
    );
    assert_eq!(refusal.code, ErrorCode::DataLoss);
}

#[tokio::test]
async fn a_preserved_keeper_is_reported_with_the_identity_that_holds_the_ptys() {
    let fixture = WorkersFixture::new("keeper-preserve").await;
    enroll_device(&fixture).await;
    fixture.enroll_worker(WORKER_FP, "preserve-worker", 1_000).await;
    let seen = Arc::new(Mutex::new(Observed::default()));
    connect_observer(
        &fixture,
        json!({
            "outcome": "preserved",
            "keeper_pid": 4242,
            "keeper_epoch": EPOCH,
            "binding_digest": digest("c"),
        }),
        Arc::clone(&seen),
    );

    let response = handle_workers_prepare_keeper_update(
        &fixture.core,
        &device_caller(),
        prepare(WORKER_FP, journaled("preserve", "source")),
    )
    .await
    .expect("an admissible preserve is admitted")
    .body;

    assert_eq!(response.outcome, "preserved");
    assert_eq!(response.keeper_pid, Some(4242));
    assert_eq!(response.keeper_epoch.as_deref(), Some(EPOCH));
    assert_eq!(response.binding_digest.as_deref(), Some(digest("c").as_str()));

    // The worker learns the open-session proof so it can check it against the
    // channel count it observes on the host.
    let observed = observed_by(&seen);
    match &observed.frames[0] {
        CoordWorkerDownstream::KeeperUpdatePrepare(body) => {
            assert_eq!(body.direction, "source");
            assert!(body.coordinator_open_session_ids.is_empty());
            assert!(body
                .journaled_update_json
                .as_deref()
                .is_some_and(|envelope| !envelope.is_empty()));
        }
        other => panic!("a keeper update goes out as its own frame, not {other:?}"),
    }
}

#[tokio::test]
async fn the_drain_is_held_across_the_decision_and_excludes_a_write() {
    let fixture = WorkersFixture::new("keeper-drain").await;
    enroll_device(&fixture).await;
    fixture.enroll_worker(WORKER_FP, "drain-worker", 1_000).await;
    let seen = Arc::new(Mutex::new(Observed::default()));
    connect_observer(
        &fixture,
        json!({ "outcome": "already-absent" }),
        Arc::clone(&seen),
    );

    let response = handle_workers_prepare_keeper_update(
        &fixture.core,
        &device_caller(),
        prepare(WORKER_FP, maintenance()),
    )
    .await
    .expect("an empty keeper shuts down")
    .body;
    assert_eq!(response.outcome, "already-absent");

    let observed = observed_by(&seen);
    assert_eq!(
        observed.exclusive_held,
        vec![true],
        "the frame reaches the worker inside the drain, not before it"
    );
    assert_eq!(
        observed.write_refused,
        vec![true],
        "a durable mutation is excluded while the keeper update decides"
    );
    // And the gate reopens once the decision is done, or the coordinator would
    // never accept another mutation again.
    assert!(
        fixture
            .core
            .services
            .write_gate()
            .acquire_shared()
            .is_ok(),
        "the drain is released when the preparation finishes"
    );
}

#[tokio::test]
async fn a_decision_reached_outside_the_drain_is_refused() {
    let fixture = WorkersFixture::new("keeper-undrained").await;
    fixture.enroll_worker(WORKER_FP, "undrained-worker", 1_000).await;
    let request = KeeperUpdateRequest::parse(maintenance()).expect("a maintenance request parses");

    let refusal = decide_keeper_update(
        &WriteGate::new(),
        &fixture.database,
        &WorkerFp::try_from(WORKER_FP).expect("a fingerprint the brand accepts"),
        &request,
    )
    .await
    .expect_err("an emptiness proof reached with the drain released proves nothing");

    assert_eq!(refusal, KeeperUpdateRefusal::DrainNotHeld);
    assert_eq!(refusal.code, ErrorCode::Internal);
}

#[tokio::test]
async fn a_second_preparation_is_refused_while_the_drain_is_held() {
    let fixture = WorkersFixture::new("keeper-second").await;
    enroll_device(&fixture).await;
    fixture.enroll_worker(WORKER_FP, "second-worker", 1_000).await;
    let held = fixture.core.services.write_gate().acquire_exclusive();

    assert!(held.is_ok(), "the first preparation took the drain");
    let refusal = handle_workers_prepare_keeper_update(
        &fixture.core,
        &device_caller(),
        prepare(WORKER_FP, maintenance()),
    )
    .await
    .expect_err("a second handover may not overlap the first");

    assert_eq!(refusal.message.as_deref(), Some("coordinator keeper update preparation is held"));
    assert_eq!(refusal.code, ErrorCode::Unavailable);
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
    fixture.enroll_worker(WORKER_FP, "stale-worker", 1_000).await;
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
    fixture.enroll_worker(WORKER_FP, "tombstone-worker", 1_000).await;
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
async fn force_live_is_the_only_path_across_a_live_session() {
    let fixture = WorkersFixture::new("keeper-force").await;
    enroll_device(&fixture).await;
    fixture.enroll_worker(WORKER_FP, "force-worker", 1_000).await;
    fixture.enroll_session(WORKER_FP, support::SESSION_ID).await;
    let seen = Arc::new(Mutex::new(Observed::default()));
    connect_observer(&fixture, json!({ "outcome": "shutdown" }), Arc::clone(&seen));

    let response = handle_workers_prepare_keeper_update(
        &fixture.core,
        &device_caller(),
        prepare(
            WORKER_FP,
            PrepareRequest {
                force_live: true,
                ..maintenance()
            },
        ),
    )
    .await
    .expect("the operator authorized crossing live sessions")
    .body;
    assert_eq!(response.outcome, "shutdown");

    // The authorization is the maintenance path alone: a journaled request that
    // carries it is refused before the drain is even taken, so a replayed or
    // hand-edited journal can never arrive holding one.
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
        Some("keeper force-live requires the maintenance path")
    );
}

#[tokio::test]
async fn the_empty_keeper_requirement_follows_the_action_and_the_authorization() {
    // The predicate the whole slice rests on, at its boundary rather than
    // through a handler: a preserve never needs emptiness, a replace-empty
    // always does, and the maintenance path is the only one a flag moves.
    assert!(!KeeperUpdateAction::Preserve.requires_empty_keeper(false));
    assert!(KeeperUpdateAction::ReplaceEmpty.requires_empty_keeper(true));
    assert!(KeeperUpdateAction::Maintenance.requires_empty_keeper(false));
    assert!(!KeeperUpdateAction::Maintenance.requires_empty_keeper(true));
    assert_eq!(KeeperUpdateAction::Maintenance.as_str(), MAINTENANCE_ACTION);
    assert_eq!(KeeperUpdateAction::ReplaceEmpty.as_str(), "replace-empty");
    assert_eq!(KeeperUpdateAction::Preserve.as_str(), "preserve");
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
        Some("keeper maintenance cannot carry a journaled update")
    );
}
