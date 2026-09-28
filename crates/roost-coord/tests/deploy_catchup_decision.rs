//! The catch-up admission rule, pure: every refusal of
//! `deploy::catchup_decision::catch_up_deploy_decision` by name, with no
//! coordinator, database or subprocess. The runtime bookkeeping around it is
//! `tests/deploy_catchup.rs`.
//!
//! Ported from `apps/coord/tests/deploy/worker-catchup-deploy.test.ts`; each
//! test names the v2 case it ports.
//!
//! `unwrap` and `expect` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to be
//! stated here. Every panic names a value the test just built.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod deploy_support;

use std::collections::{BTreeSet, HashMap};

use deploy_support::{FLEET_SHA, HOST, behind_worker, skip, start};
use roost_coord::deploy::catchup_decision::{
    CATCH_UP_COOLDOWN_MS, CatchUpDeployDecision, CatchUpDeployDecisionInputs, CatchUpSkipReason,
    CatchUpWorkerRow, catch_up_deploy_decision,
};

/// v2's `decisionInputs()`, owned so a test can override one field.
struct Scenario {
    worker: CatchUpWorkerRow,
    coord_git_sha: Option<&'static str>,
    in_flight: BTreeSet<String>,
    cooldowns: HashMap<String, i64>,
    operator_rollout_active: bool,
    now_ms: i64,
}

impl Scenario {
    fn new() -> Self {
        Self {
            worker: behind_worker(),
            coord_git_sha: Some(FLEET_SHA),
            in_flight: BTreeSet::new(),
            cooldowns: HashMap::new(),
            operator_rollout_active: false,
            now_ms: 1_000,
        }
    }

    fn decide(&self) -> CatchUpDeployDecision {
        catch_up_deploy_decision(&CatchUpDeployDecisionInputs {
            worker: &self.worker,
            coord_git_sha: self.coord_git_sha,
            hosts_with_deploy_in_flight: &self.in_flight,
            cooldown_until_ms_by_host: &self.cooldowns,
            operator_rollout_active: self.operator_rollout_active,
            keeper_update_blocked: false,
            now_ms: self.now_ms,
        })
    }
}

// "admits a POSIX worker behind the coordinator's own SHA" and "deploys to the
// registered label when no reachable address is known".
#[test]
fn a_behind_posix_worker_is_admitted_at_its_reachable_address_then_its_label() {
    assert_eq!(Scenario::new().decide(), start(HOST));
    let mut unaddressed = Scenario::new();
    unaddressed.worker.reachable_addr = None;
    assert_eq!(unaddressed.decide(), start("m1-us"));
}

// "refuses a Windows worker: the signed broker owns that path", "refuses a
// worker already on the fleet SHA", "refuses while a deploy for that host is
// already in flight", "refuses while an operator fleet rollout owns the fleet".
#[test]
fn a_windows_current_busy_or_rolling_fleet_is_refused_by_name() {
    let mut windows = Scenario::new();
    windows.worker.os = Some("win32".to_owned());
    assert_eq!(
        windows.decide(),
        skip(CatchUpSkipReason::WindowsBrokerOwned)
    );

    let mut current = Scenario::new();
    current.worker.git_sha = Some(FLEET_SHA.to_owned());
    assert_eq!(current.decide(), skip(CatchUpSkipReason::UpToDate));

    let mut busy = Scenario::new();
    busy.in_flight.insert(HOST.to_owned());
    assert_eq!(busy.decide(), skip(CatchUpSkipReason::DeployInFlight));

    let mut rolling = Scenario::new();
    rolling.operator_rollout_active = true;
    assert_eq!(
        rolling.decide(),
        skip(CatchUpSkipReason::OperatorRolloutInProgress)
    );
}

// "refuses when the coordinator's own SHA is not a release identity" and
// "refuses when the worker reports no SHA".
#[test]
fn a_sha_that_is_not_a_release_identity_on_either_side_is_refused() {
    for coord_git_sha in [None, Some("dev"), Some("b1c2d3e")] {
        let mut scenario = Scenario::new();
        scenario.coord_git_sha = coord_git_sha;
        assert_eq!(
            scenario.decide(),
            skip(CatchUpSkipReason::CoordinatorShaUnknown),
            "{coord_git_sha:?}"
        );
    }
    for git_sha in [None, Some(""), Some("dev")] {
        let mut scenario = Scenario::new();
        scenario.worker.git_sha = git_sha.map(str::to_owned);
        assert_eq!(
            scenario.decide(),
            skip(CatchUpSkipReason::WorkerShaUnknown),
            "{git_sha:?}"
        );
    }
}

// "refuses inside the failure cooldown and admits once it lapses".
#[test]
fn the_cooldown_refuses_until_its_last_millisecond_and_admits_at_its_end() {
    let failed_at_ms = 5_000;
    let mut scenario = Scenario::new();
    scenario
        .cooldowns
        .insert(HOST.to_owned(), failed_at_ms + CATCH_UP_COOLDOWN_MS);
    scenario.now_ms = failed_at_ms + CATCH_UP_COOLDOWN_MS - 1;
    assert_eq!(scenario.decide(), skip(CatchUpSkipReason::FailureCooldown));
    scenario.now_ms = failed_at_ms + CATCH_UP_COOLDOWN_MS;
    assert_eq!(scenario.decide(), start(HOST));
}

// "refuses when nothing addressable resolves for the worker".
#[test]
fn a_worker_with_nothing_addressable_is_refused() {
    for label in ["", "   ", "old laptop"] {
        let mut scenario = Scenario::new();
        scenario.worker.label = label.to_owned();
        scenario.worker.reachable_addr = None;
        assert_eq!(
            scenario.decide(),
            skip(CatchUpSkipReason::NoReachableHost),
            "{label:?}"
        );
    }
}
