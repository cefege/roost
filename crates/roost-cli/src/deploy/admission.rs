//! Whether a deploy may stage a release onto a target, and the evidence that
//! decides it. Called by the deploy command; depends on the coordinator's own
//! admission contract in `roost-protocol` and on nothing else in the deploy
//! group.
//!
//! There are two questions here and they have to be kept apart, because
//! collapsing them is how a deploy stops being able to repair the machines it
//! exists to repair.
//!
//! The first is about the COORDINATOR: it holds one row per worker, and a row it
//! stopped refreshing describes what the coordinator last heard rather than what
//! the host is running. A worker that died an hour ago and a healthy worker
//! behind a broken tailnet hop produce the identical row. Gating on that row
//! refuses exactly the machine that needs repair, and the remedy such a refusal
//! prints — "start the worker so it can prove admission" — is unreachable for a
//! host that is down.
//!
//! The second is about the TARGET, and it is the only one that can decide. The
//! probe below runs one command on the host and stages only on positive proof
//! of emptiness: the service definition is absent, or the service manager itself
//! answered AND reports the worker not running AND no keeper process is parenting
//! a channel process. Every unknown fails closed, because an unreachable service
//! manager reads exactly like a stopped one in its own output. A keeper SOCKET
//! FILE is deliberately not evidence — it outlives the keeper that created it, so
//! it can neither prove nor disprove anything the process counts do not.
//!
//! Live PTYs keep every refusal they had: a running worker, or a keeper holding
//! channels, still refuses.

use roost_protocol::keeper_update::{
    JournaledKeeperUpdateV1, KeeperContractV1, keeper_update_admission,
};

use crate::command_error::CommandFailure;
use crate::deploy::codes;
use crate::status::report::WorkerStatus;

/// What the coordinator's own admission produced for this target.
///
/// The three unproven cases are separate values rather than one `None` so a
/// deploy driver can tell "no proof can ever exist for this worker" apart from
/// "proof was refused" — a refusal throws with exit 5 and never becomes a value
/// here, which is what keeps a refused machine from being staged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionOutcome {
    /// The keeper may be carried across this deploy under a recorded envelope.
    Admitted {
        /// The worker the envelope is about, which is the identity a keeper
        /// action has to be addressed to.
        worker_fingerprint: String,
        /// The immutable record the target's worker will consume at boot.
        keeper_update: Box<JournaledKeeperUpdateV1>,
    },
    /// The coordinator has no row for this host.
    Unregistered,
    /// A row the coordinator stopped refreshing.
    ProofStale {
        /// The label that refusal will name.
        worker_label: String,
    },
    /// A live row from a build that predates keeper-runtime reporting, so no
    /// proof can exist for it.
    RuntimeUnreported {
        /// The label that refusal will name.
        worker_label: String,
    },
}

/// What a deploy driver is allowed to stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionStaging {
    /// The keeper update the release carries, or `None` for a deploy that
    /// performs no keeper mutation at all.
    pub keeper_update: Option<JournaledKeeperUpdateV1>,
    /// The worker the update belongs to, which travels with it.
    pub worker_fingerprint: Option<String>,
    /// A claim about the REGISTRY that the target's own evidence then has to
    /// confirm. `None` is the bootstrap allowance: a worker that has never
    /// reported a keeper runtime can never earn admission for the update that
    /// teaches it to report, so demanding proof would pin it on the build that
    /// cannot produce it.
    pub installed_service_refusal: Option<String>,
}

/// The refusal an unproven install carries, before the target is asked.
pub fn unproven_installed_service_refusal(platform_label: &str) -> String {
    format!("existing {platform_label} worker requires keeper update admission before staging")
}

/// Collapse one admission outcome into what may be staged.
pub fn keeper_admission_staging(
    platform_label: &str,
    resolved: AdmissionOutcome,
) -> AdmissionStaging {
    match resolved {
        AdmissionOutcome::Admitted {
            worker_fingerprint,
            keeper_update,
        } => AdmissionStaging {
            keeper_update: Some(*keeper_update),
            worker_fingerprint: Some(worker_fingerprint),
            installed_service_refusal: None,
        },
        AdmissionOutcome::RuntimeUnreported { .. } => {
            // The staged release carries no journaled keeper update, so this
            // deploy performs no keeper mutation at all; the worker's own boot
            // admission still refuses to replace a keeper holding live channels.
            // Demanding proof here instead just pins the worker on the build
            // that cannot produce it.
            AdmissionStaging {
                keeper_update: None,
                worker_fingerprint: None,
                installed_service_refusal: None,
            }
        }
        AdmissionOutcome::ProofStale { worker_label } => AdmissionStaging {
            keeper_update: None,
            worker_fingerprint: None,
            installed_service_refusal: Some(format!(
                "existing {platform_label} worker {worker_label} has a stale keeper update proof"
            )),
        },
        AdmissionOutcome::Unregistered => AdmissionStaging {
            keeper_update: None,
            worker_fingerprint: None,
            installed_service_refusal: Some(unproven_installed_service_refusal(platform_label)),
        },
    }
}

/// Classify this target's keeper update against the coordinator's own contract.
///
/// `bootstrap_allowed` is the operator's standing answer to "may a machine with
/// no coordinator row be enrolled by this deploy" — an ambient bootstrap token,
/// or the localhost quickstart path. It never makes a stale or unreported
/// worker admissible; those are refused, because both describe a machine the
/// coordinator cannot currently act on.
pub fn direct_keeper_update_admission(
    host: &str,
    target_contract: &KeeperContractV1,
    bootstrap_allowed: bool,
    inventory: &[WorkerStatus],
) -> Result<AdmissionOutcome, CommandFailure> {
    let matching: Vec<&WorkerStatus> = inventory
        .iter()
        .filter(|worker| worker_matches_target(worker, host))
        .collect();
    if matching.is_empty() {
        if bootstrap_allowed {
            return Ok(AdmissionOutcome::Unregistered);
        }
        return Err(CommandFailure::generic(format!(
            "{host}: no registered worker matches this host, and this deploy is not authorized to \
             enroll one"
        )));
    }
    if matching.len() != 1 {
        return Err(CommandFailure::generic(format!(
            "{host}: keeper update admission cannot resolve exactly one worker ({} match)",
            matching.len()
        )));
    }
    let worker = matching[0];
    // A row the coordinator has not heard from can neither prove nor refresh
    // anything, and no keeper action could reach a disconnected worker anyway.
    if worker.stale {
        return Ok(AdmissionOutcome::ProofStale {
            worker_label: worker.label.clone(),
        });
    }
    // No observation at all is the one unprovable case: the running build
    // predates keeper-runtime reporting, so it can never earn admission for the
    // update that teaches it to report. A contradicted or refused observation
    // fails closed further down instead.
    let Some(observation) = worker.keeper_runtime.as_ref() else {
        return Ok(AdmissionOutcome::RuntimeUnreported {
            worker_label: worker.label.clone(),
        });
    };
    let sessions: std::collections::BTreeSet<String> = worker
        .coordinator_open_session_ids
        .iter()
        .cloned()
        .collect();
    let Some(admission) = keeper_update_admission(target_contract, Some(observation), &sessions)
    else {
        return Err(codes::refuse(
            codes::KEEPER_NOT_ADOPTABLE,
            format!(
                "{}: keeper update is blocked or unproven ({} channel(s) held, {} coordinator \
                 session(s) open)",
                worker.label,
                observation.channel_count,
                sessions.len()
            ),
        ));
    };
    let update = JournaledKeeperUpdateV1 {
        admission,
        source_contract: observation.running_contract.clone(),
        target_contract: target_contract.clone(),
    };
    update.validate().map_err(|error| {
        codes::refuse(
            codes::KEEPER_NOT_ADOPTABLE,
            format!(
                "{}: the keeper update this deploy would journal is not admissible: {error}",
                worker.label
            ),
        )
    })?;
    Ok(AdmissionOutcome::Admitted {
        worker_fingerprint: worker.fingerprint.clone(),
        keeper_update: Box::new(update),
    })
}

/// Whether a coordinator row is about `host`.
///
/// Matched on the three names a host can be addressed by, case-insensitively:
/// the fingerprint it enrolled under, the label an operator gave it, and the
/// address the rest of the fleet reaches it at. Any of the three matching is a
/// match, because an operator says `roost deploy <host>` with whichever of them
/// they have.
pub fn worker_matches_target(worker: &WorkerStatus, host: &str) -> bool {
    let wanted = host.trim().to_ascii_lowercase();
    [
        Some(&worker.fingerprint),
        Some(&worker.label),
        worker.reachable_addr.as_ref(),
    ]
    .into_iter()
    .flatten()
    .any(|value| value.trim().to_ascii_lowercase() == wanted)
}
