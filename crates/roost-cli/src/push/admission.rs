//! Keeper admission for every machine one fleet rollout converges, decided from
//! ONE coordinator snapshot before anything is touched. Called by the push
//! command body; depends on the shared keeper-update contract in
//! `roost-protocol` and on the roster, and on nothing else in the push group.
//!
//! A participant whose keeper cannot be updated safely is DEFERRED, not fatal.
//! Refusing the batch would let one unadoptable keeper on one laptop block every
//! other machine in the fleet, and forcing it would end live PTYs. The
//! classification is the shared one — the implementation digest the RUNNING
//! keeper reports compared against the one the release ships — so this module
//! holds no rule of its own and cannot drift from what a single-host deploy
//! would decide about the same machine.

use std::collections::{BTreeMap, BTreeSet};

use roost_protocol::keeper_update::{
    INCOMPATIBLE_WITH_LIVE_SESSIONS, JournaledKeeperUpdateV1, KEEPER_RESTART_REQUIRED, UNPROVEN,
    WORKER_ONLY_SAFE, KeeperContractV1, classify_keeper_update, keeper_update_admission,
};

use crate::push::plan::{DeferredFleetWorker, FleetRolloutTarget};
use crate::status::report::WorkerStatus;

/// The keeper decision a rollout participant carries for the whole transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetRolloutWorker {
    pub fingerprint: String,
    pub host: String,
    pub keeper_update: JournaledKeeperUpdateV1,
}

/// The participants, split from the machines this push cannot touch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FleetKeeperAdmission {
    pub workers: Vec<FleetRolloutWorker>,
    pub deferred: Vec<DeferredFleetWorker>,
}

/// Classify every candidate's keeper update against the contract its release
/// ships.
///
/// A machine with no admission is deferred with the reason the operator's own
/// way out, and a machine whose registry row no longer resolves to exactly one
/// worker is deferred rather than guessed at: the journal this decision is about
/// to be written into addresses machines by fingerprint.
pub fn classify_fleet_keeper_updates(
    targets: &[FleetRolloutTarget],
    inventory: &[WorkerStatus],
    target_contracts: &BTreeMap<String, KeeperContractV1>,
) -> FleetKeeperAdmission {
    let mut admitted = FleetKeeperAdmission::default();
    for target in targets {
        let matching: Vec<&WorkerStatus> = inventory
            .iter()
            .filter(|worker| worker.fingerprint == target.fingerprint)
            .collect();
        let [worker] = matching.as_slice() else {
            admitted.deferred.push(DeferredFleetWorker {
                fingerprint: target.fingerprint.clone(),
                label: short(&target.fingerprint),
                reason: "update admission cannot resolve one worker".to_string(),
            });
            continue;
        };
        let Some(target_contract) = target_contracts.get(&target.fingerprint) else {
            admitted.deferred.push(DeferredFleetWorker {
                fingerprint: target.fingerprint.clone(),
                label: worker.label.clone(),
                reason: "target keeper runtime proof is unavailable".to_string(),
            });
            continue;
        };
        let open_sessions: BTreeSet<String> =
            worker.coordinator_open_session_ids.iter().cloned().collect();
        let Some(admission) = keeper_update_admission(
            target_contract,
            worker.keeper_runtime.as_ref(),
            &open_sessions,
        ) else {
            admitted.deferred.push(DeferredFleetWorker {
                fingerprint: target.fingerprint.clone(),
                label: worker.label.clone(),
                reason: deferral_reason(classify_keeper_update(
                    target_contract,
                    worker.keeper_runtime.as_ref(),
                    &open_sessions,
                )),
            });
            continue;
        };
        let Some(running) = worker.keeper_runtime.as_ref() else {
            admitted.deferred.push(DeferredFleetWorker {
                fingerprint: target.fingerprint.clone(),
                label: worker.label.clone(),
                reason: deferral_reason(UNPROVEN),
            });
            continue;
        };
        admitted.workers.push(FleetRolloutWorker {
            fingerprint: target.fingerprint.clone(),
            host: target.host.clone(),
            keeper_update: JournaledKeeperUpdateV1 {
                admission,
                source_contract: running.running_contract.clone(),
                target_contract: target_contract.clone(),
            },
        });
    }
    admitted
}

/// The contract a participant's keeper must satisfy once it converges on the
/// release a rollback restores: the contract it is RUNNING now.
///
/// This is the mirror of [`classify_fleet_keeper_updates`]. A rollback does not
/// ship a new keeper, it puts back the one the machine already had, so the
/// admission a rollback is proved against has to be derived from the
/// coordinator's own observation of that keeper rather than from any release.
pub fn rollback_keeper_update(worker: &WorkerStatus) -> Option<JournaledKeeperUpdateV1> {
    let running = worker.keeper_runtime.as_ref()?;
    let open_sessions: BTreeSet<String> =
        worker.coordinator_open_session_ids.iter().cloned().collect();
    let admission =
        keeper_update_admission(&running.running_contract, Some(running), &open_sessions)?;
    Some(JournaledKeeperUpdateV1 {
        admission,
        source_contract: running.running_contract.clone(),
        target_contract: running.running_contract.clone(),
    })
}

/// Each reason names the operator's own way out, because a keeper deferral is
/// the one kind that does not clear itself on the machine's next attach.
fn deferral_reason(classification: &str) -> String {
    match classification {
        INCOMPATIBLE_WITH_LIVE_SESSIONS => "keeper cannot be adopted while its sessions are live \
             — `roost keeper-refresh <host> --yes` when those PTYs are expendable"
            .to_string(),
        UNPROVEN => "keeper update admission is unproven".to_string(),
        WORKER_ONLY_SAFE | KEEPER_RESTART_REQUIRED => {
            "keeper update admission metadata is incomplete".to_string()
        }
        other => format!("keeper update admission is {other}"),
    }
}

fn short(fingerprint: &str) -> String {
    fingerprint.chars().take(12).collect()
}
