//! Whether a machine's keeper may be carried across, decided from one
//! coordinator snapshot before anything is touched.
//!
//! The property under test is the one that costs a person their shells: the
//! decision compares the implementation digest the RUNNING keeper reports
//! against the one the release ships, and a machine whose keeper cannot be
//! carried is left alone rather than pushed. A test that only checked the
//! returned action would pass against a classifier that had decided to replace
//! everything, so every assertion here is about the pair — the machine is
//! either a participant with a keeper decision, or it is deferred with the
//! operator's own way out — never about the action string alone.

// A test asserts with `expect`: the message IS the hypothesis about the setup,
// and the workspace lint table denies `expect_used` in every target. The
// production half of this slice contains no `expect` at all.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod push_fixture;

use std::collections::BTreeMap;

use push_fixture::{contract, digest, worker, with};

use roost_cli::push::admission::{classify_fleet_keeper_updates, rollback_keeper_update};
use roost_cli::push::plan::FleetRolloutTarget;

const PRIOR: &str = "1111111111111111111111111111111111111111";

fn candidate(label: &str) -> FleetRolloutTarget {
    FleetRolloutTarget {
        fingerprint: digest(&format!("fingerprint-{label}")),
        host: format!("{label}.example.test"),
    }
}

fn contracts_for(
    label: &str,
    target_seed: &str,
) -> BTreeMap<String, roost_protocol::keeper_update::KeeperContractV1> {
    let mut contracts = BTreeMap::new();
    contracts.insert(digest(&format!("fingerprint-{label}")), contract(target_seed));
    contracts
}

#[test]
fn the_same_keeper_binary_is_carried_across_and_the_machine_is_a_participant() {
    let row = worker("studio", "studio.example.test", Some(PRIOR), 0);
    let admitted = classify_fleet_keeper_updates(
        &[candidate("studio")],
        std::slice::from_ref(&row),
        &contracts_for("studio", "keeper-a"),
    );

    assert!(admitted.deferred.is_empty(), "{:?}", admitted.deferred);
    assert_eq!(admitted.workers.len(), 1);
    let recorded = &admitted.workers[0].keeper_update;
    assert_eq!(recorded.admission.required_action, "preserve");
    assert_eq!(
        recorded.admission.source_contract_digest,
        recorded.admission.target_contract_digest,
        "a preserved keeper is proved against the digest the RUNNING keeper reported"
    );
    assert_eq!(
        recorded.source_contract.implementation_digest,
        row.keeper_runtime.as_ref().and_then(|r| r.running_contract.implementation_digest.clone()),
        "the journal names the keeper that is running now, not the one the release ships"
    );
}

#[test]
fn a_different_keeper_binary_with_live_sessions_is_deferred_with_the_way_out() {
    let row = worker("studio", "studio.example.test", Some(PRIOR), 2);
    let admitted = classify_fleet_keeper_updates(
        &[candidate("studio")],
        std::slice::from_ref(&row),
        &contracts_for("studio", "keeper-b"),
    );

    assert!(
        admitted.workers.is_empty(),
        "a machine holding two live PTYs must never be a participant: {:?}",
        admitted.workers
    );
    assert_eq!(admitted.deferred.len(), 1);
    let reason = &admitted.deferred[0].reason;
    assert_eq!(admitted.deferred[0].label, "studio");
    assert!(
        reason.contains("roost keeper-refresh <host> --yes"),
        "a keeper deferral does not clear itself on the machine's next attach, so the reason \
         must name the command: {reason}"
    );
}

#[test]
fn a_different_keeper_binary_on_a_provably_empty_keeper_is_a_replace_not_a_refusal() {
    let row = worker("studio", "studio.example.test", Some(PRIOR), 0);
    let admitted = classify_fleet_keeper_updates(
        &[candidate("studio")],
        std::slice::from_ref(&row),
        &contracts_for("studio", "keeper-b"),
    );

    assert!(admitted.deferred.is_empty(), "{:?}", admitted.deferred);
    assert_eq!(admitted.workers.len(), 1);
    let recorded = &admitted.workers[0].keeper_update;
    assert_eq!(recorded.admission.required_action, "replace-empty");
    assert_ne!(
        recorded.admission.source_contract_digest, recorded.admission.target_contract_digest,
        "a replace-empty is only meaningful against a DIFFERENT binary"
    );
}

#[test]
fn a_machine_with_no_keeper_observation_is_deferred_as_unproven_and_never_guessed_at() {
    let mut row = worker("studio", "studio.example.test", Some(PRIOR), 0);
    row.keeper_runtime = None;
    let admitted = classify_fleet_keeper_updates(
        &[candidate("studio")],
        std::slice::from_ref(&row),
        &contracts_for("studio", "keeper-a"),
    );

    assert!(admitted.workers.is_empty());
    assert_eq!(admitted.deferred.len(), 1);
    assert_eq!(
        admitted.deferred[0].reason, "keeper update admission is unproven",
        "an absent observation is a missing proof, not a safe keeper"
    );
}

#[test]
fn a_machine_whose_target_contract_could_not_be_proved_is_deferred_before_it_is_touched() {
    let row = worker("studio", "studio.example.test", Some(PRIOR), 0);
    let admitted = classify_fleet_keeper_updates(
        &[candidate("studio")],
        std::slice::from_ref(&row),
        &BTreeMap::new(),
    );

    assert!(
        admitted.workers.is_empty(),
        "without the contract the release ships there is no decision to act on"
    );
    assert_eq!(
        admitted.deferred[0].reason, "target keeper runtime proof is unavailable"
    );
}

#[test]
fn a_registry_row_that_no_longer_resolves_to_one_machine_is_deferred_rather_than_matched() {
    let first = worker("studio", "studio.example.test", Some(PRIOR), 0);
    let mut second = worker("attic", "attic.example.test", Some(PRIOR), 0);
    second.fingerprint = first.fingerprint.clone();
    let admitted = classify_fleet_keeper_updates(
        &[candidate("studio")],
        &[first, second],
        &contracts_for("studio", "keeper-a"),
    );

    assert!(admitted.workers.is_empty());
    assert_eq!(admitted.deferred.len(), 1);
    assert_eq!(
        admitted.deferred[0].reason, "update admission cannot resolve one worker"
    );
}

#[test]
fn a_rollback_is_proved_against_the_keeper_that_is_running_now() {
    let row = worker("studio", "studio.example.test", Some(PRIOR), 0);
    let update = rollback_keeper_update(&row).expect("an observed keeper admits a rollback");
    assert_eq!(update.admission.required_action, "preserve");
    assert_eq!(
        update.source_contract, update.target_contract,
        "a rollback ships no new keeper, so both sides of the decision are the one running"
    );

    let mut unobserved = row.clone();
    unobserved.keeper_runtime = None;
    assert!(
        rollback_keeper_update(&unobserved).is_none(),
        "with nothing observed there is no contract to restore to"
    );
}

#[test]
fn one_unadoptable_keeper_defers_only_its_own_machine_and_leaves_the_fleet_rolling() {
    let studio = worker("studio", "studio.example.test", Some(PRIOR), 0);
    let loft = worker("loft", "loft.example.test", Some(PRIOR), 2);
    // Read before the roster takes it: the assertion below is that the machine
    // holding live PTYs is the deferred one, which needs the count to outlive
    // the move into the roster.
    let loft_sessions = loft.coordinator_open_session_ids.len();
    let behind = with(
        worker("shed", "shed.example.test", Some(PRIOR), 0),
        Some(PRIOR),
        true,
    );
    let roster = vec![studio, loft, behind];
    let candidates = ["studio", "loft", "shed"]
        .iter()
        .map(|label| candidate(label))
        .collect::<Vec<_>>();

    let mut contracts = contracts_for("studio", "keeper-a");
    contracts.insert(digest("fingerprint-loft"), contract("keeper-b"));
    contracts.insert(digest("fingerprint-shed"), contract("keeper-a"));

    let admitted = classify_fleet_keeper_updates(&candidates, &roster, &contracts);

    assert_eq!(
        admitted
            .workers
            .iter()
            .map(|worker| worker.host.as_str())
            .collect::<Vec<_>>(),
        vec!["studio.example.test"],
        "one machine's live PTYs must not stop every other machine in the fleet"
    );
    assert_eq!(admitted.deferred.len(), 1);
    assert_eq!(admitted.deferred[0].label, "loft");
    assert_eq!(
        loft_sessions,
        2,
        "the deferred machine is the one holding live PTYs, and nobody else's"
    );
}
