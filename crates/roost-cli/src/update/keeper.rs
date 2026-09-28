//! Whether a self-replace may proceed with a keeper running on this machine.
//! Called by `update::mod` before anything is written; depends on
//! `roost-protocol`'s keeper admission contract, on the update group's journal
//! for the record it produces, and on nothing else in this crate.
//!
//! v3 ships the keeper as a SEPARATE BINARY and a SEPARATE PROCESS, and that
//! single fact is the whole reason this check is smaller than the deploy path's.
//! `roost update` replaces the `roost` executable and nothing else: it does not
//! write `roost-keeper`, it does not signal a keeper, and it does not restart a
//! worker. A running keeper holding live PTYs is therefore not disturbed by the
//! swap mechanically, whatever the outcome of this decision.
//!
//! What the decision is for is narrower and it is a real hazard. The candidate
//! `roost` is a NEW program that will go on to spawn and supervise keepers, and
//! the keeper running right now was spawned by the OLD one. If the candidate's
//! keeper contract is one the running keeper cannot live under, then an update
//! that installs it leaves a machine whose `roost` and whose running keeper
//! disagree — and the disagreement surfaces later, at a worker restart, as a
//! refusal with the operator holding a binary they did not choose. So the
//! candidate is interrogated BEFORE it is installed, by running it.
//!
//! Two answers are refusals, and they are refusals for different reasons.
//!
//! - A keeper that would have to be REPLACED is refused. Replacing a keeper is a
//!   keeper mutation: it needs the coordinator's fence, a drain, and a
//!   convergence proof, and `roost deploy` is the command that has all three. A
//!   self-update that did it would destroy PTYs with none of them.
//! - A keeper that may only be PRESERVED is the one answer that means the swap
//!   is safe, and it is the answer a v3 self-update expects, because the keeper
//!   binary this command does not touch is still the one the candidate will use.
//!
//! The candidate is asked by running it, never by asking this process. A keeper
//! contract read from the binary that happens to be installed while the decision
//! is being made is a contract for the wrong program, and every admission made
//! from it would be about bytes nobody is about to install.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use roost_protocol::keeper_update::{
    KEEPER_RESTART_REQUIRED, KEEPER_UPDATE_CLASSIFICATIONS, KeeperContractV1,
    KeeperRuntimeObservationV1, PRESERVE, WORKER_ONLY_SAFE, keeper_update_admission,
};

use crate::update::journal::KeeperRecord;

/// The argv a release's keeper contract is read with. It is the same hidden
/// probe `roost __keeper-contract` answers, addressed on the candidate rather
/// than on this process, which is the entire point of running it.
pub const CANDIDATE_PROBE: &str = "__keeper-contract";

/// Every way the keeper check can refuse a swap.
#[derive(Debug, thiserror::Error)]
pub enum KeeperGateError {
    #[error("the candidate keeper contract probe at {path} failed: {reason}")]
    ProbeFailed { path: String, reason: String },
    #[error("the candidate keeper contract is not a shape this build can read: {0}")]
    ContractMalformed(String),
    #[error(
        "the keeper on this machine {running}, and the candidate {summary}, so this update is refused and nothing is replaced"
    )]
    NotAdmissible { running: String, summary: String },
    #[error(
        "a keeper is running on this machine but the coordinator has no proof of it, so this update cannot be shown to be safe and is refused"
    )]
    KeeperUnproven,
}

/// What the candidate says about itself, as answered by running it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateContract {
    /// The keeper ABI the candidate would run, from the candidate's own bytes.
    pub contract: KeeperContractV1,
}

/// What the coordinator knows about the keeper running on this machine.
#[derive(Debug, Clone)]
pub struct RunningKeeper {
    /// The worker this machine is enrolled as, which is the identity a keeper
    /// action would be addressed to.
    pub worker_fingerprint: String,
    /// The coordinator's own observation of the keeper, or `None` when the
    /// coordinator has no row that carries one.
    pub observation: Option<KeeperRuntimeObservationV1>,
    /// The coordinator's open-session list for that worker, which is what
    /// decides whether a keeper may be replaced at all.
    pub open_session_ids: Vec<String>,
}

/// Ask a downloaded candidate what keeper it would run, by running it.
///
/// The probe is the hidden subcommand the release itself ships, so a candidate
/// that cannot answer it is a candidate this build cannot reason about — which
/// is a refusal, not a fallback to this process's own contract. Falling back
/// would produce exactly the wrong-program admission this module exists to
/// prevent, and it would do so silently.
pub fn probe_candidate_contract(candidate: &Path) -> Result<CandidateContract, KeeperGateError> {
    let output = Command::new(candidate)
        .arg(CANDIDATE_PROBE)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| KeeperGateError::ProbeFailed {
            path: candidate.display().to_string(),
            reason: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(KeeperGateError::ProbeFailed {
            path: candidate.display().to_string(),
            reason: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    let printed = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(printed.trim()).map_err(|error| {
        KeeperGateError::ContractMalformed(format!("the probe printed no JSON: {error}"))
    })?;
    let contract = KeeperContractV1::parse(&value)
        .map_err(|error| KeeperGateError::ContractMalformed(error.to_string()))?;
    Ok(CandidateContract { contract })
}

/// Decide whether a candidate may be installed over a running keeper.
///
/// The coordinator's own contract is the only thing allowed to answer this.
/// Re-deriving the classification here would be a second implementation of the
/// exact rule whose failure mode is a lost PTY, and the shared function exists
/// so that deploy, upgrade and self-update cannot disagree about it.
pub fn admit_candidate(
    candidate: &CandidateContract,
    running: Option<&RunningKeeper>,
) -> Result<KeeperRecord, KeeperGateError> {
    let Some(running) = running else {
        return Err(KeeperGateError::KeeperUnproven);
    };
    let open_sessions: BTreeSet<String> = running.open_session_ids.iter().cloned().collect();
    let Some(admission) = keeper_update_admission(
        &candidate.contract,
        running.observation.as_ref(),
        &open_sessions,
    ) else {
        return Err(KeeperGateError::NotAdmissible {
            running: describe_running(running),
            summary: format!(
                "cannot be classified as safe ({})",
                classification_of(candidate, running)
            ),
        });
    };
    if admission.required_action != PRESERVE {
        return Err(KeeperGateError::NotAdmissible {
            running: describe_running(running),
            summary: format!(
                "would require the keeper to be replaced ({}), which is a keeper mutation and \
                 belongs to `roost deploy` with its fence and convergence proof",
                if admission.classification == KEEPER_RESTART_REQUIRED {
                    "keeper restart required"
                } else {
                    "replace empty"
                }
            ),
        });
    }
    Ok(KeeperRecord::Admitted {
        worker_fingerprint: running.worker_fingerprint.clone(),
        classification: admission.classification,
        required_action: admission.required_action,
    })
}

/// The record for a machine with no keeper to disturb.
///
/// This is a real answer and not a shrug. `roost update` is the operator
/// command that runs on a laptop with no fleet, where refusing because a
/// coordinator database is absent would make the command useless for the
/// machine most likely to want it. The absence of any running keeper is what
/// makes the swap safe there, and the reason travels in the journal so an
/// operator reading a retained one can tell "nothing was at risk" from "nobody
/// checked".
pub fn no_running_keeper(reason: impl Into<String>) -> KeeperRecord {
    KeeperRecord::NoRunningKeeper {
        reason: reason.into(),
    }
}

fn describe_running(running: &RunningKeeper) -> String {
    match &running.observation {
        Some(observation) => format!(
            "is running under build {:?} holding {} channel(s)",
            observation.running_contract.build_sha, observation.channel_count
        ),
        None => String::from("has no coordinator observation"),
    }
}

/// The classification the shared contract reached, for a message that says
/// which of its four verdicts applied. Asking it a second time is safe and
/// cheap; it is a pure function of the same three inputs.
fn classification_of(candidate: &CandidateContract, running: &RunningKeeper) -> &'static str {
    let open_sessions: BTreeSet<String> = running.open_session_ids.iter().cloned().collect();
    roost_protocol::keeper_update::classify_keeper_update(
        &candidate.contract,
        running.observation.as_ref(),
        &open_sessions,
    )
}

/// The classifications a self-replace can report, so a caller naming one cannot
/// name a fifth. Exposed because a refusal message that quotes a verdict an
/// operator cannot look up is a worse message than one that names it exactly.
pub const ADMISSIBLE_CLASSIFICATION: &str = WORKER_ONLY_SAFE;

/// Every classification the shared contract can reach, re-exported so a caller
/// of this module does not have to import the protocol crate to check one.
pub const CLASSIFICATIONS: [&str; 4] = KEEPER_UPDATE_CLASSIFICATIONS;
