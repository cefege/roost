//! The shapes a fleet test builds: a registry row, a keeper that has actually
//! reported itself, and a journaled keeper decision derived from both.
//!
//! Every fixture here is built through the SAME constructors production reads,
//! and the keeper ones are only accepted by the protocol when they are internally
//! consistent — a channel count that disagrees with a binding digest classifies
//! as unproven, which would let a test pass for the wrong reason.

#![allow(dead_code)]

pub mod fake_world;

use std::collections::BTreeSet;

use roost_protocol::keeper_update::{
    JournaledKeeperUpdateV1, KEEPER_EMPTY_BINDING_DIGEST, KeeperContractV1,
    KeeperRuntimeObservationV1, keeper_update_admission,
};
use sha2::{Digest, Sha256};

use roost_cli::push::admission::FleetRolloutWorker;
use roost_cli::status::report::WorkerStatus;

/// A full 64-hex identity, derived from `seed` so two fixtures never collide and
/// one can be told from another by its seed alone.
pub fn digest(seed: &str) -> String {
    let hashed = Sha256::digest(seed.as_bytes());
    hex::encode(hashed)
}

/// The keeper contract the release named `seed` ships.
pub fn contract(seed: &str) -> KeeperContractV1 {
    KeeperContractV1 {
        protocol_version: 1,
        supported_features: vec!["pty".to_string()],
        required_features: Vec::new(),
        implementation_digest: Some(digest(seed)),
        platform: "linux".to_string(),
        arch: "x86_64".to_string(),
        build_sha: "b".repeat(40),
    }
}

/// A running keeper reporting itself, with `channels` live PTYs.
pub fn observation(seed: &str, channels: u32) -> KeeperRuntimeObservationV1 {
    let binding_digest = if channels == 0 {
        KEEPER_EMPTY_BINDING_DIGEST.to_string()
    } else {
        digest(&format!("{seed}-bindings"))
    };
    KeeperRuntimeObservationV1 {
        schema_version: 1,
        running_contract: contract(seed),
        keeper_pid: 4242,
        keeper_epoch: "6f1c2d3e-4a5b-4c6d-8e9f-0a1b2c3d4e5f".to_string(),
        channel_count: channels,
        binding_digest,
        reconciled_at_ms: 1_700_000_000_000,
    }
}

/// A registry row for a machine that is fresh and running a keeper holding
/// `channels` live PTYs, on `git_sha`.
pub fn worker(label: &str, host: &str, git_sha: Option<&str>, channels: u32) -> WorkerStatus {
    let keeper_runtime = observation("keeper-a", channels);
    let open: Vec<String> = (0..channels)
        .map(|index| format!("s-{label}-{index}"))
        .collect();
    WorkerStatus {
        fingerprint: digest(&format!("fingerprint-{label}")),
        label: label.to_string(),
        os: "linux".to_string(),
        reachable_addr: Some(host.to_string()),
        git_sha: git_sha.map(str::to_string),
        keeper_runtime: Some(keeper_runtime),
        terminal_core_capacity: None,
        coordinator_open_session_ids: open,
        last_seen_ms: 1_700_000_000_000,
        age_ms: 0,
        stale: false,
    }
}

/// The same row with the fields a rollback or a deferral needs changed.
pub fn with(mut row: WorkerStatus, git_sha: Option<&str>, stale: bool) -> WorkerStatus {
    row.git_sha = git_sha.map(str::to_string);
    row.stale = stale;
    if stale {
        row.age_ms = 600_000;
    }
    row
}

/// A journaled keeper decision built by the shared admission rule, so a fixture
/// is a decision production would actually have made.
pub fn journaled_update(
    running: &KeeperRuntimeObservationV1,
    target: &KeeperContractV1,
    open_sessions: &BTreeSet<String>,
) -> JournaledKeeperUpdateV1 {
    let admission = keeper_update_admission(target, Some(running), open_sessions)
        .unwrap_or_else(|| panic!("the fixture must be an admission the shared rule accepts"));
    JournaledKeeperUpdateV1 {
        admission,
        source_contract: running.running_contract.clone(),
        target_contract: target.clone(),
    }
}

/// A rollout participant carrying `update` as its keeper decision.
pub fn participant(label: &str, host: &str, update: JournaledKeeperUpdateV1) -> FleetRolloutWorker {
    FleetRolloutWorker {
        fingerprint: digest(&format!("fingerprint-{label}")),
        host: host.to_string(),
        keeper_update: update,
    }
}
