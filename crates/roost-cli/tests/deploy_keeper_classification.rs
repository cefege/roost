//! How a target's keeper classifies against the release being deployed, and what
//! that classification permits. The companion to
//! `deploy_keeper_admission.rs`, which owns the refusal decisions; this file owns
//! the protocol's own classifications, because they are `roost-protocol`'s rules
//! and the only thing worth asserting here is that a deploy acts on all of them.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_protocol::keeper_update::{
    INCOMPATIBLE_WITH_LIVE_SESSIONS, KEEPER_EMPTY_BINDING_DIGEST, KEEPER_RESTART_REQUIRED,
    KeeperContractV1, KeeperRuntimeObservationV1, UNPROVEN, WORKER_ONLY_SAFE,
    classify_keeper_update,
};
use serde_json::json;

const NOW: i64 = 1_781_900_000_000;
const RUNNING_DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// The one digest a provably empty keeper reports, from the constant that owns it.
const EMPTY_DIGEST: &str = KEEPER_EMPTY_BINDING_DIGEST;
const TARGET_DIGEST: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn keeper(channel_count: u32, digest: &str) -> KeeperRuntimeObservationV1 {
    KeeperRuntimeObservationV1::parse(&json!({
        "schema_version": 1,
        "running_contract": {
            "protocol_version": 3,
            "supported_features": [],
            "required_features": [],
            "implementation_digest": RUNNING_DIGEST,
            "platform": "linux",
            "arch": "x86_64",
            "build_sha": "b1d1836a"
        },
        "keeper_pid": 4242,
        "keeper_epoch": "6f1c2d3e-4a5b-4c6d-8e9f-0a1b2c3d4e5f",
        "channel_count": channel_count,
        "binding_digest": digest,
        "reconciled_at_ms": NOW - 3_000
    }))
    .unwrap()
}

fn contract(digest: &str) -> KeeperContractV1 {
    KeeperContractV1::parse(&json!({
        "protocol_version": 3,
        "supported_features": [],
        "required_features": [],
        "implementation_digest": digest,
        "platform": "linux",
        "arch": "x86_64",
        "build_sha": "b1d1836a"
    }))
    .unwrap()
}

/// A keeper binary that is not the one this release ships is unadoptable WHILE
/// IT HOLDS CHANNELS, and replaceable the moment it is provably empty. This is the
/// whole reason the admission asks about channels at all: a digest difference on
/// its own is not a refusal, and a deploy that treated it as one would leave a
/// machine holding nobody's shells permanently unupdatable.
#[test]
fn a_different_keeper_binary_is_unadoptable_only_while_it_holds_channels() {
    let target = contract(TARGET_DIGEST);
    let live = keeper(2, &"c".repeat(64));
    let sessions: std::collections::BTreeSet<String> = ["session-0", "session-1"]
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(
        classify_keeper_update(&target, Some(&live), &sessions),
        INCOMPATIBLE_WITH_LIVE_SESSIONS
    );

    let empty = keeper(0, EMPTY_DIGEST);
    assert_eq!(
        classify_keeper_update(&target, Some(&empty), &std::collections::BTreeSet::new()),
        KEEPER_RESTART_REQUIRED
    );
}

/// A keeper that cannot name its own binary is unproven whatever else it says. An
/// admission decided against it would be a decision about a program nobody has
/// identified.
#[test]
fn a_keeper_that_cannot_name_its_binary_is_unproven() {
    let mut anonymous = contract(RUNNING_DIGEST);
    anonymous.implementation_digest = None;
    let empty = keeper(0, EMPTY_DIGEST);
    assert_eq!(
        classify_keeper_update(&anonymous, Some(&empty), &std::collections::BTreeSet::new()),
        UNPROVEN
    );
}

/// A channel count and the coordinator's open-session list that disagree are
/// evidence of nothing, in either direction.
#[test]
fn a_channel_count_the_coordinator_disagrees_with_is_unproven() {
    let target = contract(RUNNING_DIGEST);
    let live = keeper(2, &"c".repeat(64));
    assert_eq!(
        classify_keeper_update(&target, Some(&live), &std::collections::BTreeSet::new()),
        UNPROVEN
    );
}

/// The same keeper binary is a worker-only-safe restart: the worker may be
/// replaced and the PTYs kept, which is the whole reason a redeploy of an
/// unchanged release is not destructive.
#[test]
fn the_same_keeper_binary_is_preservable() {
    let target = contract(RUNNING_DIGEST);
    let live = keeper(2, &"c".repeat(64));
    let sessions: std::collections::BTreeSet<String> = ["session-0", "session-1"]
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(
        classify_keeper_update(&target, Some(&live), &sessions),
        WORKER_ONLY_SAFE
    );
}
