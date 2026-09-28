//! Converges a machine that was unreachable while the fleet moved: when a worker
//! behind the coordinator's own SHA attaches, the coordinator starts the same
//! POSIX `roost deploy <host>` job the Settings button drives, so a laptop that
//! slept through `roost push` catches up by itself instead of staying behind.
//! Called by the worker lifecycle's `on_ready` (v2 `main.ts` onWorkerConnected);
//! depends on `deploy::{jobs,start,output_stream,catchup_decision}`. Ports
//! apps/coord/src/deploy/worker-catchup-deploy.ts.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, MutexGuard, PoisonError};

use roost_host::{EnvSource, roost_service_dir};
use roost_observability::{LogFields, SignalKind};
use roost_platform::HostPlatform;

use crate::db::CoordDb;
use crate::deploy::DeployRuntime;
use crate::deploy::catchup_decision::{
    CATCH_UP_COOLDOWN_MS, CatchUpDeployDecision, CatchUpDeployDecisionInputs, CatchUpSkipReason,
    CatchUpWorkerRow, catch_up_deploy_decision, is_keeper_blocked_output, keeper_signature,
    worker_catch_up_host,
};
use crate::deploy::jobs::DeployStreamMsg;
use crate::deploy::output_stream::open_deploy_output;
use crate::deploy::start::DeployStartResult;

/// The journal `roost push` writes into the coordinator's service directory
/// before it stages anything and removes once the rollout finalizes, so its
/// presence is the fleet-wide "hands off". Named here rather than imported: the
/// coordinator never depends on the CLI's modules.
pub const FLEET_PUSH_JOURNAL_FILE_NAME: &str = "fleet-push-journal.json";

/// Every mutable catch-up fact, so a host that stopped catching up is one grep
/// away.
#[derive(Debug, Default)]
pub struct CatchUpDeploys {
    state: Mutex<CatchUpState>,
}

#[derive(Debug, Default)]
struct CatchUpState {
    /// Host to the job this module started and has not yet seen settle.
    job_id_by_host: HashMap<String, String>,
    /// Host to the instant its cooldown ends.
    cooldown_until_ms_by_host: HashMap<String, i64>,
    /// Host to the keeper signature that was refused. Keyed by signature, not
    /// host alone, so the block clears by itself once that keeper's epoch or
    /// channel count moves.
    keeper_blocked_signature_by_host: HashMap<String, String>,
}

impl CatchUpDeploys {
    fn lock(&self) -> MutexGuard<'_, CatchUpState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// What one catch-up attempt runs against.
pub struct CatchUpDeployOptions<'a> {
    /// Starts the job; production passes the real POSIX deploy.
    pub deploy_starter: &'a (dyn Fn(&str, &str) -> DeployStartResult + Sync),
    /// The decision instant; `None` reads the clock when the decision is made.
    pub now_ms: Option<i64>,
    /// The fleet's desired release: always the running coordinator's own SHA
    /// in production, never a second record of it.
    pub coord_git_sha: &'a str,
    /// Where the service directory, and so the rollout journal, is resolved.
    pub env: &'a dyn EnvSource,
}

impl std::fmt::Debug for CatchUpDeployOptions<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CatchUpDeployOptions")
            .field("now_ms", &self.now_ms)
            .field("coord_git_sha", &self.coord_git_sha)
            .finish_non_exhaustive()
    }
}

impl DeployRuntime {
    /// Evaluate admission against live coordinator state and start the job
    /// when admitted. Decision, start and bookkeeping hold one lock, so two
    /// attaches of one host cannot both be admitted.
    pub fn start_catch_up_deploy_for_worker(
        &self,
        worker: &CatchUpWorkerRow,
        options: &CatchUpDeployOptions<'_>,
    ) -> CatchUpDeployDecision {
        let now_ms = options.now_ms.unwrap_or_else(crate::rpc::service::now_ms);
        let signature = keeper_signature(worker.keeper_runtime_json.as_deref());
        let operator_rollout_active = operator_fleet_rollout_active(options.env);
        let mut state = self.catch_up().lock();
        let mut hosts_with_deploy_in_flight: BTreeSet<String> =
            state.job_id_by_host.keys().cloned().collect();
        hosts_with_deploy_in_flight.extend(self.journal().running_hosts());
        let decision = catch_up_deploy_decision(&CatchUpDeployDecisionInputs {
            worker,
            coord_git_sha: Some(options.coord_git_sha),
            hosts_with_deploy_in_flight: &hosts_with_deploy_in_flight,
            cooldown_until_ms_by_host: &state.cooldown_until_ms_by_host,
            operator_rollout_active,
            keeper_update_blocked: state
                .keeper_blocked_signature_by_host
                .get(&worker_catch_up_host(worker))
                == Some(&signature),
            now_ms,
        });
        let host = match &decision {
            CatchUpDeployDecision::Start { host } => host.clone(),
            CatchUpDeployDecision::Skip(reason) => {
                // A silent no-op here is unexplainable in production: the skip
                // reason is the only record of why a behind machine stayed behind.
                tracing::info!(worker_fp = %worker.fp, reason = reason.as_str(),
                    worker_git_sha = ?worker.git_sha, coord_git_sha = options.coord_git_sha,
                    "deploy: catchup_skipped");
                return decision;
            }
        };
        // Pinned, not implicit: without the SHA the job deploys the checkout's
        // HEAD, which is the fleet's release only by coincidence.
        let job_id = match (options.deploy_starter)(&host, options.coord_git_sha) {
            DeployStartResult::Started { job_id } => job_id,
            DeployStartResult::Refused { error } => {
                state
                    .cooldown_until_ms_by_host
                    .insert(host.clone(), now_ms + CATCH_UP_COOLDOWN_MS);
                drop(state);
                // No job exists, so the job's own failure signal never fires and
                // doctor would otherwise never see an unstartable catch-up.
                roost_observability::signal::emit(
                    SignalKind::DeployFailed,
                    LogFields::new()
                        .set("host", &host)
                        .set("reason", &error)
                        .set("cooldownKey", &host),
                );
                tracing::warn!(worker_fp = %worker.fp, host, error, "deploy: catchup_start_failed");
                return CatchUpDeployDecision::Skip(CatchUpSkipReason::StartFailed);
            }
        };
        state.job_id_by_host.insert(host.clone(), job_id.clone());
        drop(state);
        tracing::info!(worker_fp = %worker.fp, host, job_id, worker_git_sha = ?worker.git_sha,
            coord_git_sha = options.coord_git_sha, "deploy: catchup_started");
        let runtime = self.clone();
        tokio::spawn(async move {
            runtime
                .watch_catch_up_deploy_outcome(&host, &job_id, &signature)
                .await;
        });
        decision
    }

    /// Await a started job's terminal frame and record the outcome, watching
    /// its output for a keeper refusal on the way.
    pub async fn watch_catch_up_deploy_outcome(
        &self,
        host: &str,
        job_id: &str,
        keeper_signature: &str,
    ) {
        let mut output = open_deploy_output(self.journal(), job_id);
        let mut keeper_blocked = false;
        let error = loop {
            match output.next_message().await {
                Some(Ok(DeployStreamMsg::Line(text))) => {
                    keeper_blocked |= is_keeper_blocked_output(&text);
                }
                Some(Ok(DeployStreamMsg::Done { error, .. })) => break error,
                Some(Err(overflow)) => {
                    keeper_blocked = false;
                    break Some(overflow.to_string());
                }
                None => {
                    keeper_blocked = false;
                    break Some("deploy output ended without a result".to_owned());
                }
            }
        };
        self.note_catch_up_deploy_settled(
            host,
            error.filter(|error| !error.is_empty()),
            keeper_blocked.then_some(keeper_signature),
            crate::rpc::service::now_ms(),
        );
    }

    /// Record the end of this module's catch-up for `host`. A settled job always
    /// arms the cooldown, not only a failed one: a deploy that exits 0 without
    /// moving the worker's reported SHA would otherwise re-arm on every attach.
    fn note_catch_up_deploy_settled(
        &self,
        host: &str,
        error: Option<String>,
        keeper_blocked_signature: Option<&str>,
        now_ms: i64,
    ) {
        {
            let mut state = self.catch_up().lock();
            state.job_id_by_host.remove(host);
            state
                .cooldown_until_ms_by_host
                .insert(host.to_owned(), now_ms + CATCH_UP_COOLDOWN_MS);
            if let Some(signature) = keeper_blocked_signature {
                state
                    .keeper_blocked_signature_by_host
                    .insert(host.to_owned(), signature.to_owned());
            }
        }
        // The job already signalled its own failure; a second signal for one
        // exit would only shorten doctor's cooldown window.
        match error {
            Some(error) => tracing::warn!(host, error,
                keeper_blocked = keeper_blocked_signature.is_some(),
                cooldown_ms = CATCH_UP_COOLDOWN_MS, "deploy: catchup_failed"),
            None => tracing::info!(host, cooldown_ms = CATCH_UP_COOLDOWN_MS,
                "deploy: catchup_settled"),
        }
    }

    /// The attach hook. Never fails: a worker that reconnected is worth more
    /// than the deploy it might have needed, so every failure is a logged skip.
    pub async fn start_catch_up_deploy_on_attach(
        &self,
        database: &CoordDb,
        worker_fp: &str,
        options: &CatchUpDeployOptions<'_>,
    ) {
        let row = match crate::workers::rows::read_live_worker(database, worker_fp).await {
            Ok(Some(row)) => row,
            Ok(None) => {
                tracing::info!(worker_fp, reason = "worker_not_registered",
                    "deploy: catchup_skipped");
                return;
            }
            Err(error) => {
                tracing::warn!(worker_fp, %error, "deploy: catchup_attach_failed");
                return;
            }
        };
        self.start_catch_up_deploy_for_worker(
            &CatchUpWorkerRow {
                fp: row.fp,
                os: Some(row.os),
                label: row.label,
                reachable_addr: row.reachable_addr,
                git_sha: row.git_sha,
                keeper_runtime_json: row.keeper_runtime_json,
            },
            options,
        );
    }
}

/// Whether `roost push`'s fleet journal is on disk. An unresolvable service
/// directory is not proof the fleet is free, so it answers "active".
#[must_use]
pub fn operator_fleet_rollout_active(env: &dyn EnvSource) -> bool {
    let service_dir = HostPlatform::current()
        .ok_or_else(|| "this platform has no service directory".to_owned())
        .and_then(|platform| roost_service_dir(env, platform).map_err(|error| error.to_string()));
    match service_dir {
        Ok(service_dir) => service_dir.join(FLEET_PUSH_JOURNAL_FILE_NAME).exists(),
        Err(error) => {
            tracing::warn!(error, "deploy: catchup_rollout_probe_failed");
            true
        }
    }
}
