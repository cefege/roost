//! The deploy command's step order, and every refusal it can raise on the way.
//! Called by the crate's dispatcher for `roost deploy`; depends on the rest of
//! the deploy group and on nothing else outside it.
//!
//! The order is the safety property, so it is worth stating once. A deploy proves
//! what it is shipping before it builds, probes the target before it builds, asks
//! the coordinator whether the target's keeper may be carried across BEFORE the
//! target's definition is replaced, and only then touches the machine. The keeper
//! question is asked early and acted on early because the answer is what decides
//! whether the machine may be touched at all; the definition is replaced last
//! because that is the only step that is hard to put back on its own.
//!
//! Progress goes to stderr and the one line an operator asked for goes to stdout.
//! A deploy that fails halfway has still changed a machine, and a script that
//! reads stdout must never be able to mistake a partial run for a settled one.

use std::process::ExitCode;

use roost_platform::posix_shell_quote;
use tracing::info;

use crate::command_error::CommandFailure;
use crate::deploy::codes;
use crate::deploy::facts::{self, RemoteFacts};
use crate::deploy::identity_env::{
    self, EnvTarget, resolve_deploy_env_value, resolve_remote_deploy_identity,
};
use crate::deploy::invocation::{
    definition_environment, prove_identity, source_root, target_contract, validate,
};
use crate::deploy::keeper_step::{self, KeeperPlan};
use crate::deploy::machine_txn::TransactionKind;
use crate::deploy::manifest::{ApplyManifest, ApplyOutcome, ApplyReport};
use crate::deploy::release;
use crate::deploy::release_stage;
use crate::deploy::ssh;
use crate::deploy::txn_session::{self, RemoteTransaction};
use crate::deploy::{DeployArgs, apply_release::staging_dir};
use crate::services::service_environment::{ENV_REACHABLE_ADDR, ENV_WORKER_LABEL};
use roost_worker::runtime::boot::ENV_COORDINATOR_URL;

/// How many times a keeper action is retried before the deploy gives up on it.
///
/// A worker behind a relayed hop can be a few seconds from answering, and a
/// deploy that gives up on the first silence fails on a healthy fleet. The bound
/// is a minute: long enough for a slow round trip, short enough that an operator
/// is not watching a spinner.
pub const KEEPER_ATTEMPTS: u32 = 60;

/// The gap between those attempts.
pub const KEEPER_RETRY: std::time::Duration = std::time::Duration::from_secs(1);

/// Progress, on stderr: about what this command is doing, not the answer.
fn progress(line: impl AsRef<str>) {
    eprintln!("{}", line.as_ref());
}

/// Run `roost deploy <host>`.
pub async fn run(args: &DeployArgs) -> Result<ExitCode, CommandFailure> {
    validate(args)?;
    let ambient = identity_env::ambient_environment();
    let source_root = source_root(args)?;
    let git_sha =
        prove_identity(args, &source_root, roost_host::supported_host_platform()?).await?;

    progress(format!(">> reachability check ssh {}", args.host));
    ssh::require_reachable(&args.host).await?;
    let platform = ssh::remote_platform(&args.host).await?;
    let arch = ssh::remote_arch(&args.host).await?;
    let home = ssh::remote_home(&args.host).await?;

    if args.force_live {
        warn_force_live(&args.host);
    }

    let triple = release::target_triple(platform, &arch)?;
    let mut staged = release::build_release(&source_root, triple).await?;
    staged.git_sha = git_sha.clone();
    if let Some(expected) = &args.expected_manifest_sha256
        && !expected.eq_ignore_ascii_case(&staged.digest)
    {
        return Err(codes::refuse(
            codes::BUILD_FAILED,
            format!(
                "the release this deploy built hashes to {}, not the {expected} it was required \
                 to install",
                staged.digest
            ),
        ));
    }
    let contract = target_contract(&staged.keeper_contract, &git_sha)?;

    let staged_dir = staging_dir(&home, &git_sha);
    let staged_text = staged_dir.display().to_string();
    release_stage::stage_over_ssh(&args.host, &staged, &staged_text).await?;
    let staged_program = staged_dir
        .join(crate::deploy::apply_release::RELEASE_BIN_DIR)
        .join(crate::deploy::apply_release::ROOST_PROGRAM)
        .display()
        .to_string();

    let facts = read_facts(&args.host, &staged_program).await?;
    if facts.platform != platform.as_str() {
        return Err(codes::refuse(
            codes::NO_REMOTE_RUNTIME,
            format!(
                "{} reported platform {} after the release was built for {platform}; the target \
                 changed under the deploy",
                args.host, facts.platform
            ),
        ));
    }
    let reused = reused_keys(&facts);
    if !reused.is_empty() {
        progress(format!(
            ">> reused from the installed definition on {}: {}",
            args.host,
            reused.join(", ")
        ));
    }

    let installed = &facts.installed_environment;
    let coordinator_url = resolve_deploy_env_value(
        ENV_COORDINATOR_URL,
        installed,
        None,
        EnvTarget::Remote,
        &ambient,
    )
    .filter(|value| !value.is_empty())
    .ok_or_else(|| {
        codes::refuse(
            codes::NO_COORDINATOR_URL,
            format!(
                "{} has no coordinator URL: set {ENV_COORDINATOR_URL} in this environment, or \
                 deploy to a machine that already has a worker installed",
                args.host
            ),
        )
    })?;
    let identity_overrides = resolve_remote_deploy_identity(
        &args.host,
        installed,
        args.label.as_deref(),
        args.reachable_addr.as_deref(),
        &ambient,
    )?;

    let plan = keeper_step::plan_and_apply(
        args,
        &ambient,
        installed,
        &contract,
        &coordinator_url,
        &staged_program,
    )
    .await?;

    let manifest = ApplyManifest::new(
        &git_sha,
        &staged_text,
        &staged.digest,
        definition_environment(
            installed,
            &identity_overrides,
            &coordinator_url,
            &git_sha,
            args,
            &ambient,
        ),
    );
    let transaction = RemoteTransaction::acquire(
        &args.host,
        &txn_session::command_for(&staged_program, TransactionKind::Deploy),
    )
    .await?;
    let applied = apply_over_ssh(&args.host, &staged_program, &manifest).await;
    // The machine is released whether the deploy settled or not. A transaction
    // left held after a refusal would make the NEXT deploy fail for a reason
    // that has nothing to do with it.
    let released = transaction.release().await;
    let report = applied?;
    let settled = report_outcome(&report, &args.host)?;
    released?;

    if let KeeperPlan {
        update: Some(update),
        fingerprint: Some(fingerprint),
        heartbeat_baseline_ms,
        reconciliation_baseline_ms,
    } = &plan
    {
        progress(format!(
            ">> proving the keeper on {} converged on the {} action",
            args.host, update.admission.required_action
        ));
        keeper_step::prove_convergence(
            &coordinator_url,
            &args.host,
            fingerprint,
            update,
            *heartbeat_baseline_ms,
            *reconciliation_baseline_ms,
            &git_sha,
        )
        .await?;
    }
    info!(host = %args.host, sha = %git_sha, "deploy settled");
    println!("{}", settled);
    Ok(ExitCode::SUCCESS)
}

/// What the target says about itself, asked of the release this deploy staged.
async fn read_facts(host: &str, staged_program: &str) -> Result<RemoteFacts, CommandFailure> {
    let command = format!(
        "{quoted} __remote-facts",
        quoted = posix_shell_quote(staged_program)
    );
    let outcome = ssh::exec(host, &command).await?;
    if !outcome.ok() {
        return Err(codes::refuse(
            codes::REMOTE_LOST,
            format!("cannot ask {host} what it is\n{}", outcome.detail()),
        ));
    }
    facts::decode(&outcome.stdout).map_err(|cause| codes::refuse(codes::REMOTE_LOST, cause))
}

/// The keys the target's own installed definition supplied, for the readout.
fn reused_keys(facts: &RemoteFacts) -> Vec<String> {
    [ENV_COORDINATOR_URL, ENV_WORKER_LABEL, ENV_REACHABLE_ADDR]
        .into_iter()
        .filter(|key| facts.installed_environment.contains_key(*key))
        .map(str::to_string)
        .collect()
}

/// Hand the manifest to the target and read its report.
async fn apply_over_ssh(
    host: &str,
    staged_program: &str,
    manifest: &ApplyManifest,
) -> Result<ApplyReport, CommandFailure> {
    let command = format!(
        "{quoted} __remote-apply",
        quoted = posix_shell_quote(staged_program)
    );
    let encoded = manifest
        .encode()
        .map_err(|cause| codes::refuse(codes::REMOTE_LOST, cause))?;
    let outcome = ssh::exec_with_stdin(host, &command, encoded).await?;
    ApplyReport::decode(&outcome.stdout).map_err(|cause| {
        if outcome.exit == 255 {
            codes::refuse(
                codes::REMOTE_LOST,
                format!("the target's connection died mid-deploy: {cause}"),
            )
        } else {
            codes::refuse(codes::REMOTE_LOST, format!("{cause}\n{}", outcome.detail()))
        }
    })
}

/// Turn the target's report into a refusal, or into the one line stdout carries.
fn report_outcome(report: &ApplyReport, host: &str) -> Result<String, CommandFailure> {
    match report.outcome {
        ApplyOutcome::Settled => Ok(format!(
            "deployed to {host}: {} ({})",
            short(&report.release_dir.clone().unwrap_or_default()),
            changed(report)
        )),
        ApplyOutcome::Recovered => Ok(format!(
            "recovered an unfinished deploy on {host} and left it on the release it was running; \
             the staged release was not installed"
        )),
        ApplyOutcome::RolledBack => Err(codes::refuse(
            codes::KEEPER_NOT_ADOPTABLE,
            format!("{host}: {}", report.detail),
        )),
        ApplyOutcome::Unsettled => Err(codes::refuse(
            codes::SETTLEMENT_FAILED,
            format!(
                "{host} was left past the point where this deploy could be undone: {}. Its deploy \
                 journal is retained and the next deploy will resolve it.",
                report.detail
            ),
        )),
        ApplyOutcome::Refused => Err(codes::refuse(
            codes::NO_REMOTE_RUNTIME,
            format!("{host}: {}", report.detail),
        )),
    }
}

fn changed(report: &ApplyReport) -> &'static str {
    if report.definition_changed {
        "definition replaced"
    } else {
        "definition already current"
    }
}

fn short(value: &str) -> String {
    value
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

fn warn_force_live(host: &str) {
    eprintln!("--force-live authorizes {host} to DESTROY every PTY held by a keeper");
    eprintln!("  the deployed worker can neither adopt nor prove empty.");
    eprintln!("  Every shell, dev server, and test in those PTYs exits.");
    eprintln!("  It applies to this deploy only; the next deploy clears it.");
}
