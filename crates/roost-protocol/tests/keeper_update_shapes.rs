//! The keeper contract's wire shapes: what a decoded contract, observation and
//! admission must look like before any admission decision is taken.
//!
//! Kept apart from the classification cases so each file stays a single
//! question: this one is "is the proof a proof", the other is "what does it
//! permit".
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_protocol::keeper_update::{
    KEEPER_EMPTY_BINDING_DIGEST, KeeperContractV1, KeeperRuntimeObservationV1,
    KeeperUpdateAdmissionV1, REPLACE_EMPTY, UNPROVEN, keeper_update_admission,
};
use std::collections::BTreeSet;

const SOURCE_DIGEST: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const TARGET_DIGEST: &str = "2222222222222222222222222222222222222222222222222222222222222222";
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

fn empty_observation() -> KeeperRuntimeObservationV1 {
    KeeperRuntimeObservationV1 {
        schema_version: 1,
        running_contract: contract(),
        keeper_pid: 41,
        keeper_epoch: KEEPER_EPOCH.to_owned(),
        channel_count: 0,
        binding_digest: KEEPER_EMPTY_BINDING_DIGEST.to_owned(),
        reconciled_at_ms: 100,
    }
}

#[test]
fn a_contract_is_refused_for_a_shape_the_schema_would_have_refused() {
    let mut wire = serde_json::to_value(contract()).expect("a contract serializes");
    wire["bun_abi"] = serde_json::Value::String("1.2.3".to_owned());
    assert!(
        KeeperContractV1::parse(&wire).is_err(),
        "an unknown field must be refused"
    );

    let mut unsorted = serde_json::to_value(contract()).expect("a contract serializes");
    unsorted["supported_features"] = serde_json::json!(["b-feature", "a-feature"]);
    assert!(
        KeeperContractV1::parse(&unsorted).is_err(),
        "features must be sorted and unique"
    );

    let mut unknown_platform = serde_json::to_value(contract()).expect("a contract serializes");
    unknown_platform["platform"] = serde_json::json!("freebsd");
    assert!(KeeperContractV1::parse(&unknown_platform).is_err());

    let mut absent_digest = serde_json::to_value(contract()).expect("a contract serializes");
    absent_digest["implementation_digest"] = serde_json::Value::Null;
    assert!(
        KeeperContractV1::parse(&absent_digest).is_ok(),
        "a null digest is legal; it is the admission that then fails closed"
    );
}

#[test]
fn a_runtime_observation_is_refused_for_a_shape_the_schema_would_have_refused() {
    let wire = serde_json::to_value(empty_observation()).expect("an observation serializes");
    let parsed = KeeperRuntimeObservationV1::parse(&wire).expect("a coherent observation parses");
    assert_eq!(parsed.channel_count, 0);
    assert_eq!(parsed.binding_digest, KEEPER_EMPTY_BINDING_DIGEST);

    let mut unknown_field = wire.clone();
    unknown_field["extra"] = serde_json::json!(true);
    assert!(KeeperRuntimeObservationV1::parse(&unknown_field).is_err());

    let mut wrong_epoch = wire.clone();
    wrong_epoch["keeper_epoch"] = serde_json::json!("not-a-uuid");
    assert!(KeeperRuntimeObservationV1::parse(&wrong_epoch).is_err());

    // The running contract inside the observation is held to its own rules.
    let mut unsorted_inner = wire;
    unsorted_inner["running_contract"]["supported_features"] =
        serde_json::json!(["b-feature", "a-feature"]);
    assert!(KeeperRuntimeObservationV1::parse(&unsorted_inner).is_err());
}

#[test]
fn an_admission_whose_own_fields_disagree_is_refused() {
    let observation = empty_observation();
    let admitted = keeper_update_admission(&contract(), Some(&observation), &BTreeSet::new())
        .expect("the same binary is preservable");
    admitted
        .validate()
        .expect("a constructed admission is consistent");

    let mut wrong_action = admitted.clone();
    wrong_action.required_action = String::from(REPLACE_EMPTY);
    assert!(wrong_action.validate().is_err());

    let mut blocked = admitted.clone();
    blocked.classification = String::from(UNPROVEN);
    assert!(
        blocked.validate().is_err(),
        "a blocked classification is not an admission"
    );

    let mut unequal_digests = admitted.clone();
    unequal_digests.target_contract_digest = TARGET_DIGEST.to_owned();
    assert!(unequal_digests.validate().is_err());

    assert!(KeeperUpdateAdmissionV1::parse(&serde_json::json!({})).is_err());
}
