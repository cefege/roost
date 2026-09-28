//! Keeper update action tests pin the authenticated live-worker boundary:
//! preserve proves keeper identity plus the current session/channel mapping,
//! and replacement accepts only an exact empty proof and waits for shutdown.
//! Ports `apps/worker/tests/keeper-update-action.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "keeper_update_support/mod.rs"]
mod keeper_update_support;

use keeper_update_support::{
    KEEPER_EPOCH, KEEPER_PID, SESSION_ID, SOURCE_DIGEST, ScriptedHost, binding, contract, digest_of,
    running, update,
};
use roost_protocol::keeper_update::{KEEPER_EMPTY_BINDING_DIGEST, KeeperBinding};
use roost_worker::keeper_pool::{
    EmptyKeeperShutdownExpectation, JournaledKeeperUpdateActionV1, KeeperRuntimeProbe,
    KeeperUpdateActionResult, UpdateDirection, apply_journaled_keeper_update_action,
};

const ACTIVE: [KeeperBinding; 1] = [KeeperBinding { channel_id: 7, pid: 5252 }];

/// v2 `action(requiredAction, coordinatorSessionIds)`: target direction, the
/// worker holding channel 7 exactly when the coordinator lists a session.
fn action(preserve: bool, sessions: &[&str]) -> JournaledKeeperUpdateActionV1 {
    JournaledKeeperUpdateActionV1 {
        schema_version: 1,
        update: update(preserve, &ACTIVE),
        direction: UpdateDirection::Target,
        coordinator_open_session_ids: sessions.iter().map(|id| (*id).to_owned()).collect(),
        worker_open_channel_ids: if sessions.is_empty() { Vec::new() } else { vec![7] },
    }
}

async fn apply(
    action: &JournaledKeeperUpdateActionV1,
    host: &ScriptedHost,
) -> Result<KeeperUpdateActionResult, String> {
    apply_journaled_keeper_update_action(action, host).await
}

#[tokio::test]
async fn preserve_proves_the_exact_live_identity_and_never_invokes_shutdown() {
    let host = ScriptedHost::new(running(contract(SOURCE_DIGEST, 'a'), &ACTIVE));
    let result = apply(&action(true, &[SESSION_ID]), &host).await.unwrap();
    assert_eq!(
        result,
        KeeperUpdateActionResult {
            outcome: "preserved",
            keeper_pid: Some(KEEPER_PID),
            keeper_epoch: Some(KEEPER_EPOCH.to_owned()),
            binding_digest: Some(digest_of(&ACTIVE)),
        }
    );
    assert_eq!(host.shutdown_calls(), 0);
}

#[tokio::test]
async fn preserve_rejects_changed_epoch_proof_without_invoking_shutdown() {
    let mut proof = running(contract(SOURCE_DIGEST, 'a'), &ACTIVE);
    proof.process_epoch = Some("30000000-0000-4000-8000-000000000001".to_owned());
    let host = ScriptedHost::new(proof);
    let error = apply(&action(true, &[SESSION_ID]), &host).await.unwrap_err();
    assert!(error.contains("preserve identity no longer matches"), "{error}");
    assert_eq!(host.shutdown_calls(), 0);
}

#[tokio::test]
async fn replace_empty_refuses_a_recorded_live_session_without_shutdown() {
    let host = ScriptedHost::new(running(contract(SOURCE_DIGEST, 'a'), &[]));
    let error = apply(&action(false, &[SESSION_ID]), &host).await.unwrap_err();
    assert!(error.contains("blocked by live sessions"), "{error}");
    assert_eq!(host.shutdown_calls(), 0);
}

#[tokio::test]
async fn replace_empty_shuts_down_only_the_exact_empty_admitted_keeper() {
    let host = ScriptedHost::new(running(contract(SOURCE_DIGEST, 'a'), &[]));
    let result = apply(&action(false, &[]), &host).await.unwrap();
    assert_eq!(result.outcome, "shutdown");
    assert_eq!(
        *host.empty_shutdowns.lock().unwrap(),
        vec![EmptyKeeperShutdownExpectation {
            keeper_pid: KEEPER_PID,
            process_epoch: KEEPER_EPOCH.to_owned(),
            binding_digest: KEEPER_EMPTY_BINDING_DIGEST.to_owned(),
        }]
    );
}

#[tokio::test]
async fn source_replacement_accepts_restored_implementation_with_different_provenance() {
    let host = ScriptedHost::new(running(contract(SOURCE_DIGEST, 'b'), &[]));
    let recorded = JournaledKeeperUpdateActionV1 {
        direction: UpdateDirection::Source,
        ..action(false, &[])
    };
    let result = apply(&recorded, &host).await.unwrap();
    assert_eq!(result.outcome, "already-converged");
    assert_eq!(host.shutdown_calls(), 0);
}

#[tokio::test]
async fn source_preserve_replay_accepts_exactly_mapped_binding_drift() {
    let drifted = [binding(9, 6262)];
    let host = ScriptedHost::new(KeeperRuntimeProbe {
        keeper_pid: Some(7171),
        process_epoch: Some("40000000-0000-4000-8000-000000000004".to_owned()),
        ..running(contract(SOURCE_DIGEST, 'b'), &drifted)
    });
    let recorded = JournaledKeeperUpdateActionV1 {
        direction: UpdateDirection::Source,
        worker_open_channel_ids: vec![9],
        ..action(true, &[SESSION_ID])
    };
    let result = apply(&recorded, &host).await.unwrap();
    assert_eq!(
        result,
        KeeperUpdateActionResult {
            outcome: "preserved",
            keeper_pid: Some(7171),
            keeper_epoch: Some("40000000-0000-4000-8000-000000000004".to_owned()),
            binding_digest: Some(digest_of(&drifted)),
        }
    );
    assert_eq!(host.shutdown_calls(), 0);
}

#[tokio::test]
async fn preserve_rejects_a_keeper_channel_absent_from_the_worker_session_map() {
    let host = ScriptedHost::new(running(contract(SOURCE_DIGEST, 'a'), &[binding(9, 6262)]));
    let error = apply(&action(true, &[SESSION_ID]), &host).await.unwrap_err();
    assert!(error.contains("worker sessions and keeper channels changed"), "{error}");
}

/// v2 passes a JS object carrying `force_live`; the Rust action cannot hold the
/// field, so the refusal is the strict decode every journaled action goes
/// through — and the same value without it decodes.
#[test]
fn rejects_a_journaled_action_that_tries_to_carry_force_live() {
    let mut value = serde_json::to_value(action(false, &[])).unwrap();
    assert!(JournaledKeeperUpdateActionV1::parse(&value).is_ok());
    value["force_live"] = serde_json::Value::Bool(true);
    assert!(JournaledKeeperUpdateActionV1::parse(&value).is_err());
}
