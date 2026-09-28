//! The keeper gate `roost update` passes before it is allowed to replace itself.
//!
//! The candidate is interrogated by RUNNING it, so the evidence here is real: a
//! copy of this crate's own binary is placed beside a copy of itself named
//! `roost-keeper`, and the candidate is asked for its keeper contract by argv.
//! What comes back is that binary's own answer, produced by the same code an
//! operator's machine runs — not a fixture that computes the expectation from
//! the function under test.
//!
//! The two outcomes are opposites and both are refusals of something. A keeper
//! that may be PRESERVED lets the swap through, because replacing `roost` does
//! not touch `roost-keeper` and the running keeper keeps its PTYs. A keeper that
//! would have to be REPLACED stops the update, because replacing a keeper needs
//! the coordinator's fence, a drain and a convergence proof, and `roost deploy`
//! is the command that has all three.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use roost_cli::update::journal::KeeperRecord;
use roost_cli::update::keeper::{
    KeeperGateError, RunningKeeper, admit_candidate, probe_candidate_contract,
};
use roost_cli::update::local_keeper::LocalKeeper;
use roost_protocol::keeper_update::{
    KEEPER_EMPTY_BINDING_DIGEST, KeeperContractV1, KeeperRuntimeObservationV1, PRESERVE,
    WORKER_ONLY_SAFE,
};
use serde_json::json;

const NOW: i64 = 1_781_900_000_000;
const EPOCH: &str = "6f1c2d3e-4a5b-4c6d-8e9f-0a1b2c3d4e5f";

/// A directory holding a copy of this crate's binary under both of the names a
/// release ships, so the candidate's contract probe finds a keeper beside it.
struct Release {
    root: PathBuf,
    candidate: PathBuf,
}

impl Release {
    fn new(case: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-keeper-gate-{}-{case}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&root).expect("the throwaway release dir is created");
        let bytes =
            std::fs::read(env!("CARGO_BIN_EXE_roost")).expect("this crate's binary is readable");
        let candidate = root.join("roost");
        std::fs::write(&candidate, &bytes).expect("the candidate is written");
        std::fs::write(root.join("roost-keeper"), &bytes).expect("the keeper is written");
        for program in [candidate.clone(), root.join("roost-keeper")] {
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
                .expect("the program is made executable");
        }
        Self { root, candidate }
    }
}

impl Drop for Release {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A keeper observation over `contract`, holding `channels` channels.
///
/// Built through the protocol crate's own parser so the fixture cannot drift
/// into a shape the shared contract would refuse — an observation whose channel
/// count and binding digest disagree is `unproven` by design, and a fixture that
/// produced one by accident would be testing the wrong refusal.
fn observation(contract: &KeeperContractV1, channels: u32) -> KeeperRuntimeObservationV1 {
    let (channel_count, binding_digest) = if channels == 0 {
        (0, KEEPER_EMPTY_BINDING_DIGEST.to_string())
    } else {
        (channels, "c".repeat(64))
    };
    KeeperRuntimeObservationV1::parse(&json!({
        "schema_version": 1,
        "running_contract": contract,
        "keeper_pid": 4242,
        "keeper_epoch": EPOCH,
        "channel_count": channel_count,
        "binding_digest": binding_digest,
        "reconciled_at_ms": NOW,
    }))
    .expect("the observation is a shape the shared contract accepts")
}

fn local(contract: &KeeperContractV1, channels: u32, open_sessions: Vec<&str>) -> LocalKeeper {
    LocalKeeper {
        worker_fingerprint: "studio".to_string(),
        observation: Some(observation(contract, channels)),
        open_session_ids: open_sessions.into_iter().map(str::to_string).collect(),
    }
}

fn contract_with(digest: &str, build: &str) -> KeeperContractV1 {
    KeeperContractV1::parse(&json!({
        "protocol_version": 3,
        "supported_features": [],
        "required_features": [],
        "implementation_digest": digest,
        "bun_abi": "rust",
        "platform": "linux",
        "arch": "x86_64",
        "build_sha": build,
    }))
    .expect("the contract is a shape the shared contract accepts")
}

/// The candidate is asked by running it, and the answer describes the keeper
/// binary that ships BESIDE it — which is the one this command does not replace.
#[test]
fn the_candidate_is_interrogated_by_running_it_not_ask_the_installed_one() {
    let release = Release::new("probe");
    let probed =
        probe_candidate_contract(&release.candidate).expect("a real binary answers the probe");

    assert_eq!(
        probed.contract.implementation_digest,
        roost_keeper::keeper::implementation_digest_of(&release.root.join("roost-keeper")),
        "the contract names the keeper binary beside the candidate, not the candidate itself"
    );
    assert!(
        !probed.contract.build_sha.is_empty(),
        "the contract carries release provenance, read from the candidate's own output"
    );
}

/// The ordinary case: the keeper running here is the keeper the candidate will
/// use, so the swap is safe and the keeper is preserved with its channels.
#[test]
fn a_keeper_the_candidate_can_preserve_lets_the_swap_through() {
    let release = Release::new("preserve");
    let probed = probe_candidate_contract(&release.candidate).expect("the candidate answers");
    // The running keeper IS the keeper binary beside the candidate, so its
    // contract is that binary's own. Only the provenance is restated: a build
    // stamp is not part of admission, and a fixture that invented one is
    // asserting on a field the shared rule deliberately ignores.
    let mut running_contract = probed.contract.clone();
    running_contract.build_sha = "0f0c0a09".to_string();
    let keeper = local(
        &running_contract,
        2,
        vec![EPOCH, "11111111-1111-4111-8111-111111111111"],
    );

    let record = admit_candidate(
        &probed,
        Some(&RunningKeeper {
            worker_fingerprint: keeper.worker_fingerprint.clone(),
            observation: keeper.observation.clone(),
            open_session_ids: keeper.open_session_ids.clone(),
        }),
    )
    .expect("a preservable keeper admits the swap");

    assert_eq!(
        record,
        KeeperRecord::Admitted {
            worker_fingerprint: "studio".to_string(),
            classification: WORKER_ONLY_SAFE.to_string(),
            required_action: PRESERVE.to_string(),
        },
        "the swap is recorded as preserving the keeper, with the worker it belongs to"
    );
}

/// The refusal that matters: the candidate speaks a keeper ABI the running
/// keeper is not, and it is holding live channels. Replacing `roost` then would
/// leave a machine whose CLI and whose running keeper disagree.
#[test]
fn a_keeper_that_would_have_to_be_replaced_stops_the_update() {
    let release = Release::new("replace-empty");
    let probed = probe_candidate_contract(&release.candidate).expect("the candidate answers");
    // A different keeper binary than the one running, on a keeper with no
    // channels: the contract classifies this KEEPER_RESTART_REQUIRED, and
    // acting on it is a keeper mutation this command may not perform.
    let other = contract_with(&"d".repeat(64), "0f0c0a09");
    let keeper = local(&other, 0, Vec::new());

    let failure = admit_candidate(
        &probed,
        Some(&RunningKeeper {
            worker_fingerprint: keeper.worker_fingerprint.clone(),
            observation: keeper.observation.clone(),
            open_session_ids: keeper.open_session_ids.clone(),
        }),
    )
    .expect_err("a keeper replacement is not a self-replace");

    assert!(
        matches!(failure, KeeperGateError::NotAdmissible { .. }),
        "the refusal is the admission, not a probe failure: {failure:?}"
    );
    assert!(
        failure.to_string().contains("roost deploy"),
        "the refusal names the command that does own a keeper mutation: {failure}"
    );
}

/// A keeper holding live channels against a different keeper binary cannot be
/// admitted at all, and the update is refused.
#[test]
fn a_live_keeper_the_candidate_cannot_preserve_stops_the_update() {
    let release = Release::new("live");
    let probed = probe_candidate_contract(&release.candidate).expect("the candidate answers");
    let other = contract_with(&"e".repeat(64), "0f0c0a09");
    let session = "22222222-2222-4222-8222-222222222222";
    let keeper = local(&other, 1, vec![session]);

    let failure = admit_candidate(
        &probed,
        Some(&RunningKeeper {
            worker_fingerprint: keeper.worker_fingerprint.clone(),
            observation: keeper.observation.clone(),
            open_session_ids: keeper.open_session_ids.clone(),
        }),
    )
    .expect_err("a live keeper that cannot be preserved stops the update");

    assert!(
        matches!(failure, KeeperGateError::NotAdmissible { .. }),
        "the refusal is the admission, not a probe failure: {failure:?}"
    );
    assert!(
        failure.to_string().contains("channel"),
        "the refusal says what was at risk: {failure}"
    );
}

/// A machine the coordinator has no proof of a keeper for cannot be shown to be
/// safe, so the update is refused rather than proceeding on an assumption.
#[test]
fn a_keeper_the_coordinator_cannot_prove_stops_the_update() {
    let release = Release::new("unproven");
    let probed = probe_candidate_contract(&release.candidate).expect("the candidate answers");

    let failure = admit_candidate(
        &probed,
        Some(&RunningKeeper {
            worker_fingerprint: "studio".to_string(),
            observation: None,
            open_session_ids: Vec::new(),
        }),
    )
    .expect_err("an unproven keeper is not a safe keeper");

    assert!(
        matches!(failure, KeeperGateError::NotAdmissible { .. }),
        "the refusal is the admission, not a probe failure: {failure:?}"
    );
}

/// A candidate that cannot be asked is refused. Falling back to this process's
/// own contract would produce an admission about the wrong program, and it would
/// do so silently.
#[test]
fn a_candidate_that_cannot_be_asked_is_refused_rather_than_assumed() {
    let failure = probe_candidate_contract(Path::new("/nonexistent/roost"))
        .expect_err("a path that is not a program cannot answer");

    assert!(
        matches!(failure, KeeperGateError::ProbeFailed { .. }),
        "the refusal names the probe, not the admission: {failure:?}"
    );
}

/// The contract a candidate printed is read through the protocol crate's own
/// parser, so a malformed answer is a refusal rather than a default.
#[test]
fn a_candidate_contract_of_the_wrong_shape_is_refused() {
    let release = Release::new("malformed");
    // A real program that answers the probe, with a stand-in for the candidate
    // that prints something the contract cannot read.
    let stand_in = release.root.join("not-a-roost");
    std::fs::write(&stand_in, "#!/bin/sh\necho 'not json'\n").expect("the stand-in is written");
    std::fs::set_permissions(&stand_in, std::fs::Permissions::from_mode(0o755))
        .expect("the stand-in is made executable");

    let failure = probe_candidate_contract(&stand_in)
        .expect_err("an answer the contract cannot read is refused");

    assert!(
        matches!(failure, KeeperGateError::ContractMalformed(_)),
        "the refusal names the shape, not the program: {failure:?}"
    );
}
