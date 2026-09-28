//! The catch-up admission rule, pure: given one attaching worker's registry row
//! and the coordinator's deploy state, start a deploy of which host, or skip
//! with which reason. Called by `deploy::catchup`; depends on
//! `deploy::{start,rpc_deploy}` for the SHA shape and the host resolution, and
//! on `roost_protocol::fleet_update` for the fleet classification v2's CLI
//! status row shares (`packages/protocol/src/fleet-update.ts`). Ports
//! `_catchUpDeployDecision` and `_keeperSignature` of
//! apps/coord/src/deploy/worker-catchup-deploy.ts.
//!
//! PURE SO EVERY REFUSAL IS TESTABLE without a coordinator, a database or a
//! subprocess. Evaluated only from the attach hook, which is why the worker
//! counts as online.

use std::collections::{BTreeSet, HashMap};

use roost_protocol::fleet_update::{WorkerUpdateInputs, WorkerUpdateState, worker_update_state};
use serde_json::Value;

use crate::deploy::rpc_deploy::{WorkerDeployRecord, worker_deploy_host};
use crate::deploy::start::{is_deploy_host, is_full_git_sha};

/// A catch-up that settled must not re-arm on the next reconnect of a flapping
/// worker: the same failure would repeat every few seconds.
pub const CATCH_UP_COOLDOWN_MS: i64 = 10 * 60 * 1000;

/// The registry columns a catch-up decision reads, named at this boundary so
/// the decision never depends on the SQLite row shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatchUpWorkerRow {
    pub fp: String,
    pub os: Option<String>,
    pub label: String,
    pub reachable_addr: Option<String>,
    pub git_sha: Option<String>,
    /// The keeper proof the worker last reported, `None` until its first
    /// post-reconcile heartbeat. Its epoch and channel count are what a keeper
    /// block is pinned to, so the block clears when the situation changes.
    pub keeper_runtime_json: Option<String>,
}

/// Everything the rule reads besides the row.
#[derive(Debug, Clone, Copy)]
pub struct CatchUpDeployDecisionInputs<'a> {
    pub worker: &'a CatchUpWorkerRow,
    /// The running coordinator's SHA -- the fleet's desired release.
    pub coord_git_sha: Option<&'a str>,
    /// Hosts with a deploy job running right now, operator-driven ones included.
    pub hosts_with_deploy_in_flight: &'a BTreeSet<String>,
    /// Host to cooldown expiry, keyed by the host this rule resolves.
    pub cooldown_until_ms_by_host: &'a HashMap<String, i64>,
    /// `roost push` owns the fleet right now (its journal is on disk).
    pub operator_rollout_active: bool,
    /// This host's keeper was already refused at this exact keeper signature.
    /// A keeper holding sessions the release cannot adopt fails the SAME
    /// admission every time, so retrying each attach would fill `roost doctor`
    /// with a failure only `roost keeper-refresh` can clear.
    pub keeper_update_blocked: bool,
    pub now_ms: i64,
}

/// Why a catch-up did not start, as the `catchup_skipped` line names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatchUpSkipReason {
    /// The signed Windows updater owns its own admission, journal and rollback.
    WindowsBrokerOwned,
    /// A rollout holds per-host journals and the machine lease throughout.
    OperatorRolloutInProgress,
    CoordinatorShaUnknown,
    WorkerShaUnknown,
    KeeperUpdateBlocked,
    NoReachableHost,
    DeployInFlight,
    UpToDate,
    FailureCooldown,
    /// Admitted, and the job itself could not be started.
    StartFailed,
    /// The fleet classification answered a state the attach hook cannot
    /// reach (both SHAs are full ids and the transport is live); refused
    /// rather than guessed at, as v2's `update_state_<state>` default.
    UnreachableUpdateState(WorkerUpdateState),
}

impl CatchUpSkipReason {
    /// The reason as the log line spells it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WindowsBrokerOwned => "windows_broker_owned",
            Self::OperatorRolloutInProgress => "operator_rollout_in_progress",
            Self::CoordinatorShaUnknown => "coordinator_sha_unknown",
            Self::WorkerShaUnknown => "worker_sha_unknown",
            Self::KeeperUpdateBlocked => "keeper_update_blocked",
            Self::NoReachableHost => "no_reachable_host",
            Self::DeployInFlight => "deploy_in_flight",
            Self::UpToDate => "up_to_date",
            Self::FailureCooldown => "failure_cooldown",
            Self::StartFailed => "start_failed",
            Self::UnreachableUpdateState(state) => match state {
                WorkerUpdateState::Unknown => "update_state_unknown",
                WorkerUpdateState::UpToDate => "update_state_up-to-date",
                WorkerUpdateState::Updating => "update_state_updating",
                WorkerUpdateState::UpdateAvailable => "update_state_update-available",
                WorkerUpdateState::UpdateDeferred => "update_state_update-deferred",
            },
        }
    }
}

/// Start a deploy of `host`, or skip for a reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatchUpDeployDecision {
    Start { host: String },
    Skip(CatchUpSkipReason),
}

/// The deploy host of an attaching worker, the one key the decision and the
/// block registry share: a block keyed by a differently-resolved host would
/// silently never match. An attach carries no operator-typed host, so an empty
/// resolution means nothing addressable is known.
#[must_use]
pub fn worker_catch_up_host(worker: &CatchUpWorkerRow) -> String {
    let record = WorkerDeployRecord {
        fp: worker.fp.clone(),
        os: worker.os.clone(),
        label: worker.label.clone(),
        reachable_addr: worker.reachable_addr.clone(),
    };
    worker_deploy_host(Some(&record), "")
}

/// The whole admission rule, in v2's order.
#[must_use]
pub fn catch_up_deploy_decision(inputs: &CatchUpDeployDecisionInputs<'_>) -> CatchUpDeployDecision {
    use CatchUpSkipReason as Skip;
    let worker = inputs.worker;
    if worker.os.as_deref() == Some("win32") {
        return CatchUpDeployDecision::Skip(Skip::WindowsBrokerOwned);
    }
    if inputs.operator_rollout_active {
        return CatchUpDeployDecision::Skip(Skip::OperatorRolloutInProgress);
    }
    let Some(coord_git_sha) = inputs.coord_git_sha.filter(|sha| is_full_git_sha(sha)) else {
        return CatchUpDeployDecision::Skip(Skip::CoordinatorShaUnknown);
    };
    let Some(worker_git_sha) = worker.git_sha.as_deref().filter(|sha| is_full_git_sha(sha)) else {
        return CatchUpDeployDecision::Skip(Skip::WorkerShaUnknown);
    };
    if inputs.keeper_update_blocked {
        return CatchUpDeployDecision::Skip(Skip::KeeperUpdateBlocked);
    }
    let host = worker_catch_up_host(worker);
    if !is_deploy_host(&host) {
        return CatchUpDeployDecision::Skip(Skip::NoReachableHost);
    }
    let state = worker_update_state(WorkerUpdateInputs {
        worker_git_sha: Some(worker_git_sha),
        coord_git_sha: Some(coord_git_sha),
        // The attach hook is the only caller, so the transport is live by
        // construction; `update-deferred` belongs to the surfaces that render it.
        online: true,
        deploy_in_flight: inputs.hosts_with_deploy_in_flight.contains(&host),
    });
    match state {
        WorkerUpdateState::UpdateAvailable => {}
        WorkerUpdateState::Updating => return CatchUpDeployDecision::Skip(Skip::DeployInFlight),
        WorkerUpdateState::UpToDate => return CatchUpDeployDecision::Skip(Skip::UpToDate),
        unreachable_here @ (WorkerUpdateState::Unknown | WorkerUpdateState::UpdateDeferred) => {
            return CatchUpDeployDecision::Skip(Skip::UnreachableUpdateState(unreachable_here));
        }
    }
    if inputs
        .cooldown_until_ms_by_host
        .get(&host)
        .is_some_and(|until_ms| inputs.now_ms < *until_ms)
    {
        return CatchUpDeployDecision::Skip(Skip::FailureCooldown);
    }
    CatchUpDeployDecision::Start { host }
}

/// The keeper identity a block is pinned to: a new epoch or a changed channel
/// count is a different keeper situation and deserves a fresh attempt.
#[must_use]
pub fn keeper_signature(keeper_runtime_json: Option<&str>) -> String {
    let Some(text) = keeper_runtime_json.filter(|text| !text.is_empty()) else {
        return "none".to_owned();
    };
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(runtime)) => format!(
            "{}:{}",
            script_string(runtime.get("keeper_epoch")),
            script_string(runtime.get("channel_count"))
        ),
        // An array is an object to the original's `typeof`, and it has neither
        // field.
        Ok(Value::Array(_)) => "undefined:undefined".to_owned(),
        _ => "unparsed".to_owned(),
    }
}

/// A JSON value as the original's `String(value)` renders it, so a block
/// recorded before and after a port compares equal.
fn script_string(value: Option<&Value>) -> String {
    match value {
        None => "undefined".to_owned(),
        Some(Value::Null) => "null".to_owned(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => match number.as_f64() {
            Some(float) if float.fract() == 0.0 && float.abs() < 1e21 => format!("{float:.0}"),
            _ => number.to_string(),
        },
        Some(Value::Bool(flag)) => flag.to_string(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::Null => String::new(),
                other => script_string(Some(other)),
            })
            .collect::<Vec<_>>()
            .join(","),
        Some(Value::Object(_)) => "[object Object]".to_owned(),
    }
}

/// Whether a deploy line is `roost deploy`'s own keeper admission refusing:
/// the job's terminal frame carries only an exit code, so this line is the only
/// place the keeper reason exists.
#[must_use]
pub fn is_keeper_blocked_output(line: &str) -> bool {
    let lowered = line.to_ascii_lowercase();
    lowered.contains("keeper replacement blocked")
        || ["blocked", "unproven", "incompatible"]
            .iter()
            .any(|verdict| {
                lowered.contains(&format!("keeper update {verdict}"))
                    || lowered.contains(&format!("keeper update is {verdict}"))
            })
}
