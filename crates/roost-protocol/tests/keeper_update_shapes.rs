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
    KEEPER_EMPTY_BINDING_DIGEST, KEEPER_RUNTIME_ABI, KeeperContractV1, KeeperRuntimeObservationV1,
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
        bun_abi: KEEPER_RUNTIME_ABI.to_owned(),
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
    wire["not_a_field"] = serde_json::Value::String("1.2.3".to_owned());
    assert!(
        KeeperContractV1::parse(&wire).is_err(),
        "an unknown field must be refused"
    );

    // v2 `packages/protocol/src/keeper-update.ts:22` declares `bun_abi` a
    // required field of the strict schema: a contract without it is no proof.
    let mut no_runtime = serde_json::to_value(contract()).expect("a contract serializes");
    no_runtime
        .as_object_mut()
        .expect("a contract is an object")
        .remove("bun_abi");
    assert!(
        KeeperContractV1::parse(&no_runtime).is_err(),
        "a contract without its runtime ABI must be refused"
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

/// v2 `packages/protocol/src/keeper-update.ts:22`: `bun_abi` is
/// `z.string().min(1).max(128)`. An empty runtime would compare equal to every
/// other empty runtime, so it must not be a contract at all.
#[test]
fn a_contract_whose_runtime_abi_is_empty_or_oversized_is_refused() {
    let empty = KeeperContractV1 {
        bun_abi: String::new(),
        ..contract()
    };
    assert_eq!(
        empty
            .validate()
            .expect_err("an empty runtime ABI is refused")
            .field,
        "keeper_contract.bun_abi"
    );
    let wire = serde_json::to_value(&empty).expect("a contract serializes");
    assert!(KeeperContractV1::parse(&wire).is_err());

    let oversized = KeeperContractV1 {
        bun_abi: "1".repeat(129),
        ..contract()
    };
    assert!(oversized.validate().is_err(), "129 bytes exceeds max(128)");
    let at_limit = KeeperContractV1 {
        bun_abi: "1".repeat(128),
        ..contract()
    };
    assert!(
        at_limit.validate().is_ok(),
        "128 bytes is the inclusive bound"
    );
}

/// The exact object v2's keeper builds (`apps/worker/src/keeper/keeper-stamp.ts:40-50`:
/// protocol 3, the sorted `protocol-io.ts` feature lists, `bun_abi: Bun.version`,
/// `arch: process.arch`), in `JSON.stringify`'s key order. A TS worker still in
/// the fleet reports this, and it must decode and re-encode byte for byte.
#[test]
fn a_v2_keeper_stamp_decodes_and_reencodes_unchanged() {
    let stamp = concat!(
        r#"{"protocol_version":3,"#,
        r#""supported_features":["acknowledged_input_v1","acknowledged_resize_v1","#,
        r#""ordered_history_v1","terminal_state_v1"],"#,
        r#""required_features":["acknowledged_input_v1","acknowledged_resize_v1","#,
        r#""ordered_history_v1"],"#,
        r#""implementation_digest":"1111111111111111111111111111111111111111111111111111111111111111","#,
        r#""bun_abi":"1.3.14","platform":"linux","arch":"x64","#,
        r#""build_sha":"0123456789abcdef0123456789abcdef01234567"}"#
    );
    let wire: serde_json::Value = serde_json::from_str(stamp).expect("the stamp is JSON");
    let parsed = KeeperContractV1::parse(&wire).expect("a v2 keeper's stamp is a contract");
    assert_eq!(parsed.bun_abi, "1.3.14");
    assert_eq!(serde_json::to_value(&parsed).expect("serializes"), wire);
    assert_eq!(serde_json::to_string(&parsed).expect("serializes"), stamp);
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
