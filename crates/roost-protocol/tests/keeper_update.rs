//! Keeper update admission: the four fail-closed outcomes, the build-provenance
//! exception, runtime-consistency checks, and the journal envelope.
//!
//! These are the cases a deploy's PTY continuity rests on. Every classification
//! a keeper can be rolled through is pinned here, including the one that must
//! never be reachable by accident.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use roost_protocol::keeper_update::{
    INCOMPATIBLE_WITH_LIVE_SESSIONS, JournaledKeeperUpdateV1, KEEPER_BINDING_DIGEST_PREAMBLE,
    KEEPER_EMPTY_BINDING_DIGEST, KEEPER_RESTART_REQUIRED, KeeperBinding, KeeperContractV1,
    KeeperRuntimeObservationV1, PRESERVE, REPLACE_EMPTY, UNPROVEN, WORKER_ONLY_SAFE,
    classify_keeper_update, keeper_binding_digest_input, keeper_contracts_exactly_equal,
    keeper_contracts_protocol_compatible, keeper_contracts_same_implementation,
    keeper_update_admission, keeper_update_outcome_matches_action,
    validate_keeper_coordinator_open_session_ids,
};

const SOURCE_DIGEST: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const TARGET_DIGEST: &str = "2222222222222222222222222222222222222222222222222222222222222222";
const LIVE_BINDING_DIGEST: &str =
    "3333333333333333333333333333333333333333333333333333333333333333";
const SESSION_ID: &str = "00000000-0000-4000-8000-000000000001";
const KEEPER_EPOCH: &str = "00000000-0000-4000-8000-000000000002";

fn contract() -> KeeperContractV1 {
    KeeperContractV1 {
        protocol_version: 2,
        supported_features: vec!["keeper-contract-v1".to_owned()],
        required_features: vec!["keeper-contract-v1".to_owned()],
        implementation_digest: Some(SOURCE_DIGEST.to_owned()),
        platform: String::from("linux"),
        arch: String::from("x64"),
        build_sha: "a".repeat(40),
    }
}

/// An observation the worker would have reported: the binding digest always
/// agrees with the channel count, which is the cross-field rule that decides
/// whether the proof counts at all.
fn observation(running: &KeeperContractV1, live: bool) -> KeeperRuntimeObservationV1 {
    KeeperRuntimeObservationV1 {
        schema_version: 1,
        running_contract: running.clone(),
        keeper_pid: 41,
        keeper_epoch: KEEPER_EPOCH.to_owned(),
        channel_count: u32::from(live),
        binding_digest: if live {
            LIVE_BINDING_DIGEST.to_owned()
        } else {
            KEEPER_EMPTY_BINDING_DIGEST.to_owned()
        },
        reconciled_at_ms: 100,
    }
}

fn no_open_sessions() -> BTreeSet<String> {
    BTreeSet::new()
}

fn one_open_session() -> BTreeSet<String> {
    BTreeSet::from([SESSION_ID.to_owned()])
}

#[test]
fn the_same_keeper_binary_is_preservable_across_a_worker_restart() {
    let mut rebuilt = contract();
    rebuilt.build_sha = "b".repeat(40);
    let running = observation(&contract(), false);

    assert_eq!(
        classify_keeper_update(&rebuilt, Some(&running), &no_open_sessions()),
        WORKER_ONLY_SAFE
    );
    assert!(keeper_contracts_same_implementation(
        &rebuilt,
        &running.running_contract
    ));
    // Provenance is not identity: a rebuild of the same bytes is the same keeper.
    assert!(!keeper_contracts_exactly_equal(
        &rebuilt,
        &running.running_contract
    ));
    assert!(keeper_contracts_exactly_equal(
        &contract(),
        &running.running_contract
    ));
}

#[test]
fn a_different_keeper_binary_needs_an_empty_replacement() {
    let mut target = contract();
    target.implementation_digest = Some(TARGET_DIGEST.to_owned());
    let running = observation(&contract(), false);

    assert_eq!(
        classify_keeper_update(&target, Some(&running), &no_open_sessions()),
        KEEPER_RESTART_REQUIRED
    );
    let admission = keeper_update_admission(&target, Some(&running), &no_open_sessions())
        .expect("an empty keeper may be replaced");
    assert_eq!(admission.classification, KEEPER_RESTART_REQUIRED);
    assert_eq!(admission.required_action, REPLACE_EMPTY);
    assert_eq!(admission.source_contract_digest, SOURCE_DIGEST);
    assert_eq!(admission.target_contract_digest, TARGET_DIGEST);
    assert_eq!(
        admission.expected_binding_digest,
        KEEPER_EMPTY_BINDING_DIGEST
    );
    assert_eq!(admission.expected_keeper_epoch, KEEPER_EPOCH);
    admission
        .validate()
        .expect("a constructed admission is consistent");
}

#[test]
fn a_different_keeper_binary_is_refused_while_sessions_are_live() {
    let mut target = contract();
    target.implementation_digest = Some(TARGET_DIGEST.to_owned());
    let running = observation(&contract(), true);

    assert_eq!(
        classify_keeper_update(&target, Some(&running), &one_open_session()),
        INCOMPATIBLE_WITH_LIVE_SESSIONS
    );
    assert!(keeper_update_admission(&target, Some(&running), &one_open_session()).is_none());
}

#[test]
fn a_preserved_keeper_admits_preserve_and_nothing_else() {
    let running = observation(&contract(), false);
    let admission = keeper_update_admission(&contract(), Some(&running), &no_open_sessions())
        .expect("the same binary is preservable");
    assert_eq!(admission.classification, WORKER_ONLY_SAFE);
    assert_eq!(admission.required_action, PRESERVE);
    assert_eq!(
        admission.source_contract_digest,
        admission.target_contract_digest
    );
    admission
        .validate()
        .expect("a constructed admission is consistent");
}

#[test]
fn a_feature_or_protocol_difference_is_a_different_implementation() {
    let mut fewer_features = contract();
    fewer_features.supported_features = Vec::new();
    let mut newer_protocol = contract();
    newer_protocol.protocol_version = 3;
    let running = observation(&contract(), false);

    for target in [&fewer_features, &newer_protocol] {
        assert!(!keeper_contracts_same_implementation(
            target,
            &running.running_contract
        ));
        assert_eq!(
            classify_keeper_update(target, Some(&running), &no_open_sessions()),
            KEEPER_RESTART_REQUIRED
        );
    }
    // A protocol version difference is a compatibility failure, not a lesser
    // one: these keepers could not serve each other even at an empty fleet.
    assert!(!keeper_contracts_protocol_compatible(
        &newer_protocol,
        &running.running_contract
    ));
    assert!(keeper_contracts_protocol_compatible(
        &contract(),
        &running.running_contract
    ));
}

#[test]
fn an_unproven_keeper_is_never_touched() {
    let running = observation(&contract(), false);
    let mut indistinguishable = contract();
    indistinguishable.implementation_digest = None;
    let mut running_without_digest = running.clone();
    running_without_digest
        .running_contract
        .implementation_digest = None;
    // The channel count and the coordinator's open sessions must agree before a
    // single binding is worth reasoning about.
    let sessions_beyond_channels = BTreeSet::from([
        SESSION_ID.to_owned(),
        "00000000-0000-4000-8000-000000000009".to_owned(),
    ]);

    assert_eq!(
        classify_keeper_update(&contract(), None, &no_open_sessions()),
        UNPROVEN
    );
    assert_eq!(
        classify_keeper_update(&indistinguishable, Some(&running), &no_open_sessions()),
        UNPROVEN
    );
    assert_eq!(
        classify_keeper_update(
            &contract(),
            Some(&running_without_digest),
            &no_open_sessions()
        ),
        UNPROVEN
    );
    assert_eq!(
        classify_keeper_update(&contract(), Some(&running), &one_open_session()),
        UNPROVEN
    );
    assert_eq!(
        classify_keeper_update(&contract(), Some(&running), &sessions_beyond_channels),
        UNPROVEN
    );
}

#[test]
fn a_proof_whose_own_halves_disagree_is_unproven_not_empty() {
    // A zero channel count with a non-canonical binding digest is a keeper that
    // cannot describe itself, and must not read as "provably empty".
    let mut self_contradicting = observation(&contract(), false);
    self_contradicting.binding_digest = LIVE_BINDING_DIGEST.to_owned();

    assert!(self_contradicting.validate().is_err());
    assert_eq!(
        classify_keeper_update(&contract(), Some(&self_contradicting), &no_open_sessions()),
        UNPROVEN
    );
    assert!(
        keeper_update_admission(&contract(), Some(&self_contradicting), &no_open_sessions())
            .is_none()
    );
}

#[test]
fn a_journal_may_not_claim_a_classification_its_contracts_contradict() {
    let source = contract();
    let mut target = contract();
    // Same binary, different host platform: the admission is internally
    // consistent on its own, and only the journal's own contracts can catch
    // that it calls two different keepers the same keeper.
    target.platform = String::from("darwin");
    let lying = keeper_update_admission(
        &contract(),
        Some(&observation(&source, false)),
        &no_open_sessions(),
    )
    .expect("the same binary is preservable");
    lying
        .validate()
        .expect("the admission is consistent with itself");
    let journal = JournaledKeeperUpdateV1 {
        admission: lying,
        source_contract: source,
        target_contract: target,
    };
    assert!(journal.validate().is_err());
}

#[test]
fn a_journal_whose_digests_drift_from_its_admission_is_refused() {
    let source = contract();
    let running = observation(&source, false);
    let admission = keeper_update_admission(&contract(), Some(&running), &no_open_sessions())
        .expect("the same binary is preservable");
    let mut moved = source.clone();
    moved.implementation_digest = Some(TARGET_DIGEST.to_owned());

    let journal = JournaledKeeperUpdateV1 {
        admission,
        source_contract: source,
        target_contract: moved,
    };
    assert!(journal.validate().is_err());
}

#[test]
fn the_coordinator_session_proof_accepts_only_canonical_order() {
    let later = "00000000-0000-4000-8000-000000000002";
    let sorted = [SESSION_ID.to_owned(), later.to_owned()];
    assert!(validate_keeper_coordinator_open_session_ids("open_session_ids", &sorted).is_ok());
    assert!(
        validate_keeper_coordinator_open_session_ids(
            "open_session_ids",
            &[later.to_owned(), SESSION_ID.to_owned()]
        )
        .is_err()
    );
    assert!(
        validate_keeper_coordinator_open_session_ids(
            "open_session_ids",
            &[SESSION_ID.to_owned(), SESSION_ID.to_owned()]
        )
        .is_err()
    );
}

#[test]
fn a_keeper_outcome_is_accepted_only_for_its_recorded_action() {
    assert!(keeper_update_outcome_matches_action(PRESERVE, "preserved"));
    assert!(!keeper_update_outcome_matches_action(PRESERVE, "shutdown"));
    assert!(keeper_update_outcome_matches_action(
        REPLACE_EMPTY,
        "shutdown"
    ));
    assert!(keeper_update_outcome_matches_action(
        REPLACE_EMPTY,
        "already-absent"
    ));
    assert!(keeper_update_outcome_matches_action(
        REPLACE_EMPTY,
        "already-converged"
    ));
    assert!(!keeper_update_outcome_matches_action(
        REPLACE_EMPTY,
        "preserved"
    ));
    assert!(keeper_update_outcome_matches_action(
        "maintenance",
        "already-absent"
    ));
    assert!(!keeper_update_outcome_matches_action(
        "maintenance",
        "already-converged"
    ));
}

#[test]
fn an_empty_keeper_digests_the_preamble_and_nothing_else() {
    // The canonical empty digest is the SHA-256 of exactly these bytes.
    assert_eq!(
        keeper_binding_digest_input(&[], &[]),
        KEEPER_BINDING_DIGEST_PREAMBLE
    );
    assert_eq!(KEEPER_EMPTY_BINDING_DIGEST.len(), 64);
}

#[test]
fn the_binding_digest_input_sorts_both_groups_into_one_fixed_order() {
    let bindings = [
        KeeperBinding {
            channel_id: 9,
            pid: 2,
        },
        KeeperBinding {
            channel_id: 3,
            pid: 1,
        },
    ];
    assert_eq!(
        keeper_binding_digest_input(&bindings, &[7, 1]),
        "keeper-bindings-v1\nb:3:1\nb:9:2\ns:1\ns:7\n"
    );
}
