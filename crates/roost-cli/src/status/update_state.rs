//! The one classifier for "is this machine on the fleet's current release?",
//! and the one wording per state. Called by status/render.rs only.
//!
//! STOPGAP: `roost-protocol` is this classifier's real home — it classifies a
//! value that ARRIVES ON THE WIRE, so the web's MachineCard and the
//! coordinator's catch-up admission read the same five states, and Main moves
//! it there at integration. Ported from
//! `packages/protocol/src/fleet-update.ts`; do not add a second copy.

/// The fleet's desired release is the COORDINATOR's own SHA, and nothing else.
/// `roost push` activates the coordinator at the target commit before it
/// converges any worker, so a worker behind the coordinator is definitionally
/// behind the fleet. A second record of intent — a "current version" file, a
/// flag on the worker — is a value that can drift out of sync with the running
/// coordinator, and then every machine reads as current while the fleet runs
/// two releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerUpdateState {
    /// One of the two SHAs is missing, so no comparison is possible. This is
    /// the whole of "an unreachable coordinator cannot classify any worker":
    /// with no coordinator SHA there is no fleet to be behind.
    Unknown,
    UpToDate,
    /// A deploy to this host is in flight right now.
    Updating,
    UpdateAvailable,
    /// Behind AND unreachable. This state is the model: the machine is
    /// skipped now and updated when it returns, which is never a reason to
    /// refuse the rest of the fleet.
    UpdateDeferred,
}

#[derive(Debug, Clone, Copy)]
pub struct WorkerUpdateInputs<'a> {
    /// The SHA the worker last reported through its heartbeat.
    pub worker_git_sha: Option<&'a str>,
    /// The running coordinator's SHA — the fleet's desired release.
    pub coord_git_sha: Option<&'a str>,
    /// Is the worker reachable right now, not merely heartbeat-fresh?
    pub online: bool,
    /// Is a deploy to this worker's host in flight?
    pub deploy_in_flight: bool,
}

pub fn worker_update_state(inputs: WorkerUpdateInputs<'_>) -> WorkerUpdateState {
    if inputs.deploy_in_flight {
        return WorkerUpdateState::Updating;
    }
    let (Some(worker), Some(coord)) = (inputs.worker_git_sha, inputs.coord_git_sha) else {
        return WorkerUpdateState::Unknown;
    };
    if worker.is_empty() || coord.is_empty() {
        return WorkerUpdateState::Unknown;
    }
    if worker == coord {
        return WorkerUpdateState::UpToDate;
    }
    if inputs.online {
        WorkerUpdateState::UpdateAvailable
    } else {
        WorkerUpdateState::UpdateDeferred
    }
}

/// One wording per state, so the CLI row and the web badge cannot diverge.
pub fn worker_update_label(state: WorkerUpdateState) -> &'static str {
    match state {
        WorkerUpdateState::Unknown => "Version unknown",
        WorkerUpdateState::UpToDate => "Up to date",
        WorkerUpdateState::Updating => "Updating…",
        WorkerUpdateState::UpdateAvailable => "Update available",
        WorkerUpdateState::UpdateDeferred => "Update pending — offline",
    }
}

#[cfg(test)]
mod tests {
    use super::{WorkerUpdateInputs, WorkerUpdateState, worker_update_label, worker_update_state};

    const COORD: &str = "b1d1836a9f4c2e1d7a0b5c6d8e9f0a1b2c3d4e5f";
    const BEHIND: &str = "0fa77c31de0e4b5a6c7d8e9f0a1b2c3d4e5f6071";

    fn inputs<'a>(worker: Option<&'a str>, online: bool) -> WorkerUpdateInputs<'a> {
        WorkerUpdateInputs {
            worker_git_sha: worker,
            coord_git_sha: Some(COORD),
            online,
            deploy_in_flight: false,
        }
    }

    #[test]
    fn matching_shas_are_up_to_date() {
        assert_eq!(
            worker_update_state(inputs(Some(COORD), true)),
            WorkerUpdateState::UpToDate
        );
    }

    #[test]
    fn a_reachable_machine_behind_the_coordinator_can_update() {
        assert_eq!(
            worker_update_state(inputs(Some(BEHIND), true)),
            WorkerUpdateState::UpdateAvailable
        );
    }

    #[test]
    fn an_unreachable_machine_behind_is_deferred_rather_than_broken() {
        assert_eq!(
            worker_update_state(inputs(Some(BEHIND), false)),
            WorkerUpdateState::UpdateDeferred
        );
    }

    #[test]
    fn a_missing_sha_is_unknown_not_behind() {
        // The reason `Version unknown` exists: the machine may well be current,
        // and printing "Update available" would send an operator to deploy at
        // a worker that is already on the release.
        assert_eq!(
            worker_update_state(inputs(None, true)),
            WorkerUpdateState::Unknown
        );
    }

    #[test]
    fn an_in_flight_deploy_outranks_every_sha_comparison() {
        assert_eq!(
            worker_update_state(WorkerUpdateInputs {
                deploy_in_flight: true,
                ..inputs(Some(COORD), true)
            }),
            WorkerUpdateState::Updating
        );
    }

    #[test]
    fn every_state_has_a_distinct_wording() {
        let labels = [
            worker_update_label(WorkerUpdateState::Unknown),
            worker_update_label(WorkerUpdateState::UpToDate),
            worker_update_label(WorkerUpdateState::Updating),
            worker_update_label(WorkerUpdateState::UpdateAvailable),
            worker_update_label(WorkerUpdateState::UpdateDeferred),
        ];
        for (index, label) in labels.iter().enumerate() {
            assert!(
                !labels[index + 1..].contains(label),
                "two states print the same wording: {label}"
            );
        }
    }
}
