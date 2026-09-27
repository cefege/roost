//! What the local coordinator knows about the keeper running on THIS machine,
//! and the decision that follows from it. Called by `update::mod`; depends on
//! `roost-protocol`'s admission contract, on `status`'s roster reader, and on
//! the update group's own keeper module, and on nothing else.
//!
//! "This machine" is resolved from the installed worker definition and matched
//! against the roster, not assumed. The alternative — treating every row in the
//! roster as local — is wrong in the direction that refuses valid updates, since
//! a fleet coordinator lists every machine and one remote keeper's PTYs are not
//! at risk from a swap on this box. It is also wrong in the direction that
//! matters more: a row that IS this machine and is not recognised is a keeper
//! whose state nobody checked, and the gate has to treat that as unproven
//! rather than absent.
//!
//! The roster is read through `status::inventory`, which is the same read-only
//! query `roost status` prints from. A second reader of the coordinator
//! database would be a second answer to "what does the fleet look like right
//! now", and the two would drift on a schema change.

use std::path::PathBuf;

use roost_host::{EnvSource, HostPlatform};
use roost_protocol::keeper_update::KeeperRuntimeObservationV1;

use crate::command_error::CommandFailure;
use crate::status::report::WorkerStatus;
use crate::update::journal::KeeperRecord;
use crate::update::keeper::{CandidateContract, RunningKeeper, admit_candidate, no_running_keeper};

/// What this machine's coordinator roster says about its own keeper.
#[derive(Debug, Clone)]
pub struct LocalKeeper {
    /// The worker this machine is enrolled as.
    pub worker_fingerprint: String,
    /// The coordinator's observation of the running keeper, if it has one.
    pub observation: Option<KeeperRuntimeObservationV1>,
    /// The coordinator's open-session list for this worker, which is what
    /// decides whether a keeper may be replaced at all.
    pub open_session_ids: Vec<String>,
}

/// What the coordinator last heard of the keeper on this machine, or `None` when
/// this machine keeps no coordinator roster naming itself.
pub async fn local_keeper(
    env: &dyn EnvSource,
    platform: HostPlatform,
) -> Result<Option<LocalKeeper>, CommandFailure> {
    let database = crate::ops::reset::coordinator_database(env, platform)?;
    if !database.exists() {
        return Ok(None);
    }
    let inventory = crate::status::inventory::worker_inventory(
        &database,
        crate::wall_clock::now_ms(),
    )
    .await?;
    let identity = installed_worker_identity(env, platform);
    let local: Vec<&WorkerStatus> = inventory
        .iter()
        .filter(|worker| is_this_machine(worker, identity.as_deref()))
        .collect();
    // Zero matches and two matches are the same answer for this command: this
    // process cannot tell which row is the machine it is standing on, so it
    // cannot claim a keeper is safe. The refusal lives in `admit_keeper`, which
    // turns an ambiguous lookup into a refusal rather than into a permission.
    let [worker] = local.as_slice() else {
        return Ok(None);
    };
    Ok(Some(LocalKeeper {
        worker_fingerprint: worker.fingerprint.clone(),
        observation: worker.keeper_runtime.clone(),
        open_session_ids: worker.coordinator_open_session_ids.clone(),
    }))
}

/// The name this machine's own installed worker definition declares.
///
/// Read from the definition rather than from the process environment, because
/// the definition is the install's own record and the shell that ran
/// `roost update` is a different shell from the one that installed the service.
fn installed_worker_identity(env: &dyn EnvSource, platform: HostPlatform) -> Option<String> {
    let definition_path = crate::services::service_spec::ServiceRole::Worker
        .definition_path(env, platform)
        .ok()?;
    let text = std::fs::read_to_string(definition_path).ok()?;
    let installed = crate::status::service_definition::parse_installed_environment(&text, platform);
    for key in [
        crate::services::service_environment::ENV_WORKER_LABEL,
        crate::services::service_environment::ENV_REACHABLE_ADDR,
    ] {
        if let Some(value) = crate::status::service_definition::declared_value(&installed, key) {
            return Some(value.to_string());
        }
    }
    None
}

/// Whether a roster row describes the machine this process is running on.
fn is_this_machine(worker: &WorkerStatus, identity: Option<&str>) -> bool {
    let Some(identity) = identity else {
        // With no installed definition there is nothing to match a row against,
        // and a row that cannot be placed is not a keeper this swap endangers.
        return false;
    };
    worker.fingerprint == identity
        || worker.label == identity
        || worker.reachable_addr.as_deref() == Some(identity)
}

/// Decide what a candidate may do to the keeper here, and record it.
///
/// A machine with no roster at all is the ordinary case on a laptop that has
/// installed the CLI and no fleet, and refusing there would make the command
/// useless for the machine most likely to want it. A machine WITH a roster gets
/// the coordinator's own verdict, and a verdict that is not "preserve" is a
/// refusal.
pub fn decide_keeper_action(
    candidate: &CandidateContract,
    local: Option<&LocalKeeper>,
) -> Result<KeeperRecord, CommandFailure> {
    let Some(local) = local else {
        return Ok(no_running_keeper(
            "no coordinator roster on this machine records a keeper here",
        ));
    };
    let running = RunningKeeper {
        worker_fingerprint: local.worker_fingerprint.clone(),
        observation: local.observation.clone(),
        open_session_ids: local.open_session_ids.clone(),
    };
    admit_candidate(candidate, Some(&running)).map_err(|error| {
        CommandFailure::generic(error.to_string())
    })
}

/// Where a self-update keeps its journal.
///
/// The service directory the deploy journal also uses, so a machine has one
/// place where its in-flight work lives and an operator looking for "did
/// something die here" finds both.
pub fn self_update_service_dir(
    env: &dyn EnvSource,
    platform: HostPlatform,
) -> Result<PathBuf, CommandFailure> {
    Ok(roost_host::roost_service_dir(env, platform)?)
}
