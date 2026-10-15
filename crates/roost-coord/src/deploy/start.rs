//! Starting one POSIX deploy job: the host and release checks, the coordinator
//! URL the deployed worker will dial, and the `roost deploy` invocation.
//! Called by `deploy::rpc_deploy` (the Settings button) and `deploy::catchup`
//! (a behind worker attaching). Ports `startDeploy` and
//! `resolveDeployCoordinatorUrl` of apps/coord/src/deploy/deploy-jobs.ts, and
//! `resolveCoordinatorDialUrl` of packages/protocol/src/coordinator-dial-url.ts.
//!
//! A COORDINATOR-STARTED DEPLOY ALWAYS SHIPS THE COORDINATOR'S OWN RELEASE,
//! never whatever its checkout's HEAD happens to be: `--coordinator-release`
//! makes the CLI prove the SHA from the installed service definition instead of
//! the upstream tip, and a `dev` stamp fails the full-SHA check before that.

use std::sync::Arc;

use roost_host::{BuildIdentity, EnvSource, ProcessEnv, build_identity};
use tokio::process::Command;

use crate::deploy::job_process::spawn_deploy_process;
use crate::deploy::jobs::DeployJournal;

/// The variable a deployed worker reads its coordinator from, and the first of
/// the declared front doors in precedence order: an explicit worker target
/// beats the coordinator identity origin, which beats the browser front door.
pub const COORDINATOR_DIAL_URL_ENV_NAMES: [&str; 3] = [
    "ROOST_COORDINATOR_URL",
    roost_host::ENV_COORDINATOR_PUBLIC_URL,
    roost_host::ENV_WEB_PUBLIC_URL,
];

/// The refusal when no front door is declared, naming every variable read.
#[must_use]
pub fn coordinator_dial_url_required_message() -> String {
    format!(
        "no coordinator URL is configured: set {}",
        COORDINATOR_DIAL_URL_ENV_NAMES.join(", ")
    )
}

/// The first declared front door. A declared-but-blank entry is undeclared:
/// installed service definitions carry `Environment="ROOST_…_URL="` lines.
#[must_use]
pub fn resolve_coordinator_dial_url(env: &dyn EnvSource) -> Option<String> {
    COORDINATOR_DIAL_URL_ENV_NAMES.iter().find_map(|name| {
        env.get(name)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    })
}

/// The operator-declared origin a deployed worker dials. A worker reaches it
/// from another machine, so a loopback or link-local host is refused even when
/// declared: roost never invents a substitute.
#[must_use]
pub fn resolve_deploy_coordinator_url(env: &dyn EnvSource) -> Option<String> {
    let declared = resolve_coordinator_dial_url(env)?;
    let Ok(url) = reqwest::Url::parse(&declared) else {
        tracing::warn!(url = %declared, "deploy: coordinator_url_malformed");
        return None;
    };
    let hostname = url.host_str().unwrap_or_default().to_ascii_lowercase();
    if matches!(hostname.as_str(), "localhost" | "127.0.0.1" | "[::1]")
        || hostname.ends_with(".local")
    {
        tracing::warn!(url = %declared, "deploy: coordinator_url_unreachable");
        return None;
    }
    Some(declared)
}

/// What a start answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployStartResult {
    /// A job exists under this id, running or already failed to spawn.
    Started { job_id: String },
    /// Nothing was opened, and why.
    Refused { error: String },
}

/// The `roost deploy` a start runs, decided before anything is spawned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployInvocation {
    /// The arguments after the executable.
    pub args: Vec<String>,
    /// The origin the deployed worker is handed as `ROOST_COORDINATOR_URL`.
    pub coordinator_url: String,
}

/// Mirrors the CLI's own host allowlist, so an unusable address is refused with
/// a reason instead of surfacing as a job that never spawns.
#[must_use]
pub fn is_deploy_host(host: &str) -> bool {
    !host.is_empty()
        && host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-')
}

/// Both object-id widths git reports, lowercase. A short stamp or `dev` is not
/// a release identity.
#[must_use]
pub fn is_full_git_sha(sha: &str) -> bool {
    matches!(sha.len(), 40 | 64)
        && sha
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Decide the invocation for `host`, or the refusal the caller answers with.
pub fn deploy_invocation(
    host: &str,
    expected_git_sha: Option<&str>,
    build: &BuildIdentity,
    env: &dyn EnvSource,
) -> Result<DeployInvocation, String> {
    if !is_deploy_host(host) {
        return Err("invalid host".to_owned());
    }
    let release_sha = expected_git_sha.unwrap_or(&build.build_sha);
    if !is_full_git_sha(release_sha) {
        return Err("invalid expected git sha".to_owned());
    }
    if build.is_compiled {
        return Err(
            "POSIX source deployment requires a coordinator source checkout; run `roost push` \
             from that checkout"
                .to_owned(),
        );
    }
    let coordinator_url =
        resolve_deploy_coordinator_url(env).ok_or_else(coordinator_dial_url_required_message)?;
    Ok(DeployInvocation {
        args: vec![
            "deploy".to_owned(),
            host.to_owned(),
            "--coordinator-release".to_owned(),
            format!("--expected-sha={release_sha}"),
        ],
        coordinator_url,
    })
}

/// Start a POSIX deploy of `host` as a job this coordinator supervises.
///
/// The subprocess is this binary's own `roost deploy`, run from the
/// coordinator's working directory with the process environment plus the
/// resolved coordinator URL.
pub fn start_deploy(
    journal: &Arc<DeployJournal>,
    host: &str,
    expected_git_sha: Option<&str>,
) -> DeployStartResult {
    let env = ProcessEnv::new();
    let invocation = match deploy_invocation(host, expected_git_sha, &build_identity(&env), &env) {
        Ok(invocation) => invocation,
        Err(error) => {
            tracing::info!(host, error, "deploy: start refused");
            return DeployStartResult::Refused { error };
        }
    };
    let job = match journal.open_job(host) {
        Ok(job) => job,
        Err(error) => {
            return DeployStartResult::Refused {
                error: format!("no deploy job id could be minted: {error}"),
            };
        }
    };
    let started = DeployStartResult::Started {
        job_id: job.job_id().to_owned(),
    };
    match std::env::current_exe().and_then(|exe| Ok((exe, std::env::current_dir()?))) {
        Ok((exe, working_dir)) => {
            let mut command = Command::new(exe);
            command.args(&invocation.args).current_dir(working_dir).env(
                COORDINATOR_DIAL_URL_ENV_NAMES[0],
                &invocation.coordinator_url,
            );
            spawn_deploy_process(journal, job, command);
        }
        Err(error) => journal.finish_job(&job, None, Some(error.to_string())),
    }
    started
}
