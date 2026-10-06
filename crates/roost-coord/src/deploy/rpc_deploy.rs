//! The two worker-deploy RPCs: `WorkersDeployStart` resolves the operator's
//! target to one registered worker and starts its job; `WorkersDeployOutput`
//! streams a job's output to its end. Called by `rpc::service_impl`; job
//! identity and buffered output belong to `deploy::{jobs,start,output_stream}`,
//! this file only authorizes and adapts them. Ports
//! apps/coord/src/deploy/handlers-workers-deploy.ts.

use connectrpc::{ConnectError, ErrorCode, Response, ServiceResult, ServiceStream};
use futures_util::StreamExt;
use roost_proto::{
    WorkersDeployOutputFrame, WorkersDeployOutputRequest, WorkersDeployStartRequest,
    WorkersDeployStartResponse,
};

use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::deploy::jobs::DeployStreamMsg;
use crate::deploy::output_stream::open_deploy_output;
use crate::deploy::start::{DeployStartResult, start_deploy};

/// The terminal frame a reader gets when its queue overflowed: the SPA explains
/// the stop and reopens for the tail.
const OVERFLOW_ERROR: &str = "deploy output stream overflowed; reopen to resume";

/// The signed Windows updater owns a Windows worker's releases, and this
/// coordinator does not carry it: Windows update deploys are paused.
const WINDOWS_DEPLOY_REFUSAL: &str = "Windows workers update through the signed Windows updater, which this coordinator does not run";

/// The registry columns a deploy target is resolved from.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct WorkerDeployRecord {
    pub fp: String,
    pub os: Option<String>,
    pub label: String,
    pub reachable_addr: Option<String>,
}

/// The one registered worker `requested_host` names, `None` when it names
/// none, or the refusal when it names more than one.
///
/// An authenticated fingerprint wins over a colliding display label or address,
/// because only the fingerprint is an identity; two workers answering to the
/// same label or address are refused rather than picked between.
pub fn resolve_worker_deploy_target<'a>(
    workers: &'a [WorkerDeployRecord],
    requested_host: &str,
) -> Result<Option<&'a WorkerDeployRecord>, String> {
    let fingerprint_matches: Vec<&WorkerDeployRecord> = workers
        .iter()
        .filter(|worker| worker.fp == requested_host)
        .collect();
    if fingerprint_matches.len() > 1 {
        return Err(format!(
            "ambiguous deploy target \"{requested_host}\" matches multiple worker fingerprints"
        ));
    }
    if let Some(worker) = fingerprint_matches.first() {
        return Ok(Some(worker));
    }
    let alias_matches: Vec<&WorkerDeployRecord> = workers
        .iter()
        .filter(|worker| {
            worker.label == requested_host
                || worker.reachable_addr.as_deref() == Some(requested_host)
        })
        .collect();
    if alias_matches.len() > 1 {
        return Err(format!(
            "ambiguous deploy target \"{requested_host}\" matches multiple registered workers; \
             use the worker fingerprint"
        ));
    }
    Ok(alias_matches.first().copied())
}

/// The address a deploy of `worker` dials: a Windows worker by its fingerprint,
/// otherwise its live reachable address, then its label, then what the operator
/// typed.
#[must_use]
pub fn worker_deploy_host(worker: Option<&WorkerDeployRecord>, requested_host: &str) -> String {
    let Some(worker) = worker else {
        return requested_host.to_owned();
    };
    if worker.os.as_deref() == Some("win32") && !worker.fp.is_empty() {
        return worker.fp.clone();
    }
    if let Some(reachable) = worker
        .reachable_addr
        .as_deref()
        .map(str::trim)
        .filter(|reachable| !reachable.is_empty())
    {
        return reachable.to_owned();
    }
    let label = worker.label.trim();
    if !label.is_empty() {
        return label.to_owned();
    }
    requested_host.to_owned()
}

/// Start a deploy of the registered worker the operator named.
pub async fn handle_workers_deploy_start(
    core: &CoordCore,
    caller: &Caller,
    request: WorkersDeployStartRequest,
) -> ServiceResult<WorkersDeployStartResponse> {
    require_account_device(caller)?;
    let _lease = core
        .services
        .write_gate()
        .acquire_shared()
        .map_err(|error| ConnectError::new(ErrorCode::Unavailable, error.to_string()))?;
    let workers: Vec<WorkerDeployRecord> = sqlx::query_as(
        "SELECT fp, os, label, reachable_addr FROM workers \
         WHERE deleted_at_ms IS NULL AND (fp = $1 OR label = $1 OR reachable_addr = $1)",
    )
    .bind(&request.host)
    .fetch_all(core.services.db.pool())
    .await
    .map_err(|error| ConnectError::new(ErrorCode::Internal, error.to_string()))?;
    let worker = match resolve_worker_deploy_target(&workers, &request.host) {
        Ok(Some(worker)) => worker,
        Ok(None) => return start_response(refused("worker not found")),
        Err(error) => return start_response(DeployStartResult::Refused { error }),
    };
    let host = worker_deploy_host(Some(worker), &request.host);
    if worker.os.as_deref() == Some("win32") {
        tracing::warn!(worker_fp = %worker.fp,
            "deploy: a Windows worker's update was refused; the signed updater is not ported");
        return start_response(refused(WINDOWS_DEPLOY_REFUSAL));
    }
    let expected_git_sha = request
        .expected_git_sha
        .as_deref()
        .filter(|sha| !sha.is_empty());
    start_response(start_deploy(
        core.services.deploy.journal(),
        &host,
        expected_git_sha,
    ))
}

/// Stream one job's output: what it already wrote, then each new line, ending
/// with its `done`.
pub fn handle_workers_deploy_output(
    core: &CoordCore,
    caller: &Caller,
    request: &WorkersDeployOutputRequest,
) -> ServiceResult<ServiceStream<WorkersDeployOutputFrame>> {
    require_account_device(caller)?;
    tracing::info!(job_id = %request.job_id, "deploy output: subscribed");
    let output = open_deploy_output(core.services.deploy.journal(), &request.job_id);
    Response::stream_ok(output.into_stream().map(|next| {
        Ok(match next {
            Ok(DeployStreamMsg::Line(text)) => WorkersDeployOutputFrame {
                kind: "line".to_owned(),
                text,
                ..Default::default()
            },
            Ok(DeployStreamMsg::Done { exit, error }) => {
                done_frame(exit, error.unwrap_or_default())
            }
            Err(overflow) => {
                tracing::warn!(%overflow, "deploy output: the reader fell behind; ended");
                done_frame(None, OVERFLOW_ERROR.to_owned())
            }
        })
    }))
}

/// The terminal frame: `-1` stands for "no exit code".
fn done_frame(exit: Option<i32>, error: String) -> WorkersDeployOutputFrame {
    WorkersDeployOutputFrame {
        kind: "done".to_owned(),
        exit: exit.unwrap_or(-1),
        error,
        ..Default::default()
    }
}

fn refused(error: &str) -> DeployStartResult {
    DeployStartResult::Refused {
        error: error.to_owned(),
    }
}

fn start_response(result: DeployStartResult) -> ServiceResult<WorkersDeployStartResponse> {
    let (ok, job_id, error) = match result {
        DeployStartResult::Started { job_id } => (true, job_id, String::new()),
        DeployStartResult::Refused { error } => (false, String::new(), error),
    };
    Response::ok(WorkersDeployStartResponse {
        ok,
        job_id,
        error,
        ..Default::default()
    })
}
