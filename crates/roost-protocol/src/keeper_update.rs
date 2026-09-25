//! The keeper contract: what a running keeper reports about itself, and whether
//! a new binary may replace it.
//!
//! Keeper update admission is the shared fail-closed contract for worker,
//! coordinator and CLI rollout code: it validates runtime proof and classifies
//! whether a worker-only restart can preserve the keeper's live PTYs or must
//! replace it empty. Called by the deploy path and by the upgrade gate, so a
//! divergence here is a lost PTY. The wire shapes and their validators live in
//! `contract`; this module owns the comparisons and the classification.

mod contract;

use std::collections::BTreeSet;

pub use contract::{
    JournaledKeeperUpdateV1, KeeperContractV1, KeeperRuntimeObservationV1, KeeperUpdateAdmissionV1,
};

use crate::ProtocolResult;
use crate::error::ProtocolError;
use crate::validate::uuid;

/// The binding digest a keeper reports when it holds no channels at all. A
/// replace-empty admission is only meaningful against this exact value, which
/// is what makes "the keeper is provably empty" checkable without trusting the
/// host.
pub const KEEPER_EMPTY_BINDING_DIGEST: &str =
    "74eb8cfffb89f155db2201d8c1b13202c29d91be6cc3d4fec6b465c9a9ede627";
/// The first bytes of every binding digest input. Part of the digest, so it
/// must match the running keeper's byte for byte.
pub const KEEPER_BINDING_DIGEST_PREAMBLE: &str = "keeper-bindings-v1\n";

pub(crate) const SHA256_DIGEST_LENGTH: usize = 64;

/// The target is the same keeper binary: restart the worker, keep the PTYs.
pub const WORKER_ONLY_SAFE: &str = "worker-only-safe";
/// A different binary, and the running keeper provably holds no channels.
pub const KEEPER_RESTART_REQUIRED: &str = "keeper-restart-required";
/// A different binary while sessions are live: replacing it would lose them.
pub const INCOMPATIBLE_WITH_LIVE_SESSIONS: &str = "incompatible-with-live-sessions";
/// The proof is missing or internally inconsistent; nothing may proceed.
pub const UNPROVEN: &str = "unproven";
/// Every classification, in the order the contract lists them.
pub const KEEPER_UPDATE_CLASSIFICATIONS: [&str; 4] = [
    WORKER_ONLY_SAFE,
    KEEPER_RESTART_REQUIRED,
    INCOMPATIBLE_WITH_LIVE_SESSIONS,
    UNPROVEN,
];
/// Keep the running keeper and its channels.
pub const PRESERVE: &str = "preserve";
/// Shut the keeper down; only a provably empty one may be replaced.
pub const REPLACE_EMPTY: &str = "replace-empty";
pub const KEEPER_UPDATE_REQUIRED_ACTIONS: [&str; 2] = [PRESERVE, REPLACE_EMPTY];
/// The keeper was preserved across a worker-only restart.
pub const OUTCOME_PRESERVED: &str = "preserved";
/// The keeper already runs the target binary.
pub const OUTCOME_ALREADY_CONVERGED: &str = "already-converged";
/// The keeper was shut down; the target's own boot adopts the rest.
pub const OUTCOME_SHUTDOWN: &str = "shutdown";
/// The keeper was already gone, which satisfies a shutdown.
pub const OUTCOME_ALREADY_ABSENT: &str = "already-absent";
pub const KEEPER_UPDATE_OUTCOMES: [&str; 4] = [
    OUTCOME_PRESERVED,
    OUTCOME_ALREADY_CONVERGED,
    OUTCOME_SHUTDOWN,
    OUTCOME_ALREADY_ABSENT,
];
/// A maintenance action, which shuts a keeper down but admits no replacement.
const MAINTENANCE: &str = "maintenance";
const KEEPER_COORDINATOR_OPEN_SESSION_IDS_MAX: usize = 65_535;

/// One channel's keeper process, as the binding digest counts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeeperBinding {
    pub channel_id: u32,
    pub pid: i64,
}

/// The canonical bytes a keeper's binding digest is taken over.
///
/// Active bindings sort by channel, spawning channels sort numerically, and the
/// two groups are emitted in that fixed order: a digest that reordered them
/// would read as a different set of live PTYs.
pub fn keeper_binding_digest_input(
    bindings: &[KeeperBinding],
    spawning_channels: &[u32],
) -> String {
    let mut active = bindings.to_vec();
    active.sort_by_key(|binding| binding.channel_id);
    let mut spawning: Vec<u32> = spawning_channels.to_vec();
    spawning.sort_unstable();
    let mut input = String::from(KEEPER_BINDING_DIGEST_PREAMBLE);
    for KeeperBinding { channel_id, pid } in &active {
        input.push_str(&format!("b:{channel_id}:{pid}\n"));
    }
    for channel_id in spawning {
        input.push_str(&format!("s:{channel_id}\n"));
    }
    input
}

/// Whether the two keepers can talk to each other at all.
pub fn keeper_contracts_protocol_compatible(
    target: &KeeperContractV1,
    running: &KeeperContractV1,
) -> bool {
    if target.protocol_version != running.protocol_version {
        return false;
    }
    // Each side's requirements must be met by the other's advertised support; a
    // feature neither side implements is not a compatibility problem.
    target
        .required_features
        .iter()
        .all(|feature| running.supported_features.contains(feature))
        && running
            .required_features
            .iter()
            .all(|feature| target.supported_features.contains(feature))
}

/// Whether a restart may keep the running keeper's channels.
///
/// This is the comparison a deploy's PTY continuity rests on, so it demands the
/// binary's own digest to match and never falls back to a version string.
pub fn keeper_contracts_same_implementation(
    target: &KeeperContractV1,
    running: &KeeperContractV1,
) -> bool {
    match (
        &target.implementation_digest,
        &running.implementation_digest,
    ) {
        (Some(target_digest), Some(running_digest)) => {
            target_digest == running_digest
                && target.platform == running.platform
                && target.arch == running.arch
                && target.supported_features == running.supported_features
                && target.required_features == running.required_features
                && target.protocol_version == running.protocol_version
        }
        _ => false,
    }
}

/// The same comparison plus release provenance, for a journal that must prove
/// which build produced the running keeper and not merely what it is.
pub fn keeper_contracts_exactly_equal(
    target: &KeeperContractV1,
    running: &KeeperContractV1,
) -> bool {
    keeper_contracts_same_implementation(target, running) && target.build_sha == running.build_sha
}

/// How a target contract may be rolled out over a running keeper.
///
/// Fail-closed at every step: a missing proof, a self-contradicting one, a
/// session count that disagrees with the channel count, and a keeper that
/// cannot name its own binary are all unproven rather than safe.
pub fn classify_keeper_update(
    target_contract: &KeeperContractV1,
    observation: Option<&KeeperRuntimeObservationV1>,
    coordinator_open_session_ids: &BTreeSet<String>,
) -> &'static str {
    let Some(runtime) = observation else {
        return UNPROVEN;
    };
    if runtime.validate().is_err()
        || target_contract.implementation_digest.is_none()
        || runtime.running_contract.implementation_digest.is_none()
        || coordinator_open_session_ids.len() != runtime.channel_count as usize
    {
        return UNPROVEN;
    }
    if keeper_contracts_same_implementation(target_contract, &runtime.running_contract) {
        return WORKER_ONLY_SAFE;
    }
    if runtime.channel_count == 0 && coordinator_open_session_ids.is_empty() {
        return KEEPER_RESTART_REQUIRED;
    }
    INCOMPATIBLE_WITH_LIVE_SESSIONS
}

/// The admission a deploy may act on, or `None` when it may not act at all.
///
/// The record this builds is self-consistent by construction: a preserved
/// keeper is one `keeper_contracts_same_implementation` already matched, and a
/// replace-empty is only reached through an observation whose empty channel
/// count forced the canonical empty binding digest.
pub fn keeper_update_admission(
    target_contract: &KeeperContractV1,
    observation: Option<&KeeperRuntimeObservationV1>,
    coordinator_open_session_ids: &BTreeSet<String>,
) -> Option<KeeperUpdateAdmissionV1> {
    let classification =
        classify_keeper_update(target_contract, observation, coordinator_open_session_ids);
    if classification != WORKER_ONLY_SAFE && classification != KEEPER_RESTART_REQUIRED {
        return None;
    }
    let runtime = observation?;
    runtime.validate().ok()?;
    Some(KeeperUpdateAdmissionV1 {
        classification: String::from(classification),
        source_contract_digest: runtime
            .running_contract
            .implementation_digest
            .as_deref()?
            .to_owned(),
        target_contract_digest: target_contract.implementation_digest.as_deref()?.to_owned(),
        expected_keeper_pid: runtime.keeper_pid,
        expected_keeper_epoch: runtime.keeper_epoch.clone(),
        expected_binding_digest: runtime.binding_digest.clone(),
        required_action: String::from(if classification == WORKER_ONLY_SAFE {
            PRESERVE
        } else {
            REPLACE_EMPTY
        }),
    })
}

/// Whether a recorded outcome satisfies the action that was recorded for it.
///
/// An action this build does not recognise falls into the shutdown branch, as it
/// did in the original: an unrecognised action must never be read as permission
/// to keep a keeper that was supposed to be replaced.
pub fn keeper_update_outcome_matches_action(action: &str, outcome: &str) -> bool {
    if action == PRESERVE {
        return outcome == OUTCOME_PRESERVED;
    }
    if action == MAINTENANCE {
        return matches!(outcome, OUTCOME_SHUTDOWN | OUTCOME_ALREADY_ABSENT);
    }
    matches!(
        outcome,
        OUTCOME_SHUTDOWN | OUTCOME_ALREADY_ABSENT | OUTCOME_ALREADY_CONVERGED
    )
}

/// The coordinator's own open-session list, which must be canonical so two
/// coordinators produce the same proof for the same fleet.
pub fn validate_keeper_coordinator_open_session_ids(
    field: &str,
    session_ids: &[String],
) -> ProtocolResult<()> {
    if session_ids.len() > KEEPER_COORDINATOR_OPEN_SESSION_IDS_MAX {
        return Err(ProtocolError::new(
            field,
            format!("must not exceed {KEEPER_COORDINATOR_OPEN_SESSION_IDS_MAX} session ids"),
        ));
    }
    for (index, session_id) in session_ids.iter().enumerate() {
        uuid(&format!("{field}[{index}]"), session_id)?;
        if index > 0 && session_ids[index - 1] >= *session_id {
            return Err(ProtocolError::new(
                format!("{field}[{index}]"),
                "coordinator session IDs must be sorted and unique",
            ));
        }
    }
    Ok(())
}
