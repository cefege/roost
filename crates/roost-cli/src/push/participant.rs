//! One machine of a fleet rollout: stage a named commit, ask the coordinator
//! about its keeper, replace its definition, and prove it came up. Called by
//! the push runtime for every participant, in both directions; depends on the
//! deploy group's own seams — reachability, the release build, the staged
//! keeper contract, the apply manifest, the machine transaction and the keeper
//! fence — and on nothing else in the push group.
//!
//! **This is not a fork of `deploy::run.rs`.** Every step below is a call into
//! the same public function `roost deploy` calls. The one thing it does not do
//! is re-prove the build identity, because a push already proved it once for the
//! whole fleet and a ROLLBACK has to ship a commit that is by definition not the
//! upstream tip — re-proving that against the tip would make the one operation
//! that most needs to work impossible. So the commit arrives as a parameter, and
//! `push::command` is the single place that decides what may be shipped.

use std::path::Path;

use roost_platform::posix_shell_quote;
use roost_worker::runtime::boot::ENV_COORDINATOR_URL;
use tracing::info;

use crate::command_error::CommandFailure;
use crate::deploy::apply_release::{RELEASE_BIN_DIR, ROOST_PROGRAM, staging_dir};
use crate::deploy::codes;
use crate::deploy::facts::{self, RemoteFacts};
use crate::deploy::identity_env::{
    Ambient, EnvTarget, resolve_deploy_env_value, resolve_remote_deploy_identity,
};
use crate::deploy::invocation::{definition_environment, target_contract};
use crate::deploy::keeper_step::{self, KeeperPlan};
use crate::deploy::machine_txn::TransactionKind;
use crate::deploy::manifest::{ApplyManifest, ApplyOutcome, ApplyReport};
use crate::deploy::release;
use crate::deploy::release_stage;
use crate::deploy::ssh;
use crate::deploy::txn_session::{self, RemoteTransaction};
use crate::deploy::DeployArgs;

/// What one machine did, for the progress line an operator watches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParticipantOutcome {
    /// Did the machine's definition actually change, or was it already current?
    pub changed: bool,
}

/// Replace one machine's release with `git_sha`'s, and prove that the keeper and
/// the worker both arrived.
///
/// `args.force_live` is honoured because the operator named this command and no
/// other, and it is never set by push itself: a fleet-wide authorization to
/// destroy every PTY on every machine is not something one operator typed and
/// meant for all of them.
pub async fn deploy_participant(
    args: &DeployArgs,
    ambient: &Ambient,
    source_root: &Path,
    git_sha: &str,
) -> Result<ParticipantOutcome, CommandFailure> {
    ssh::require_reachable(&args.host).await?;
    let platform = ssh::remote_platform(&args.host).await?;
    let arch = ssh::remote_arch(&args.host).await?;
    let home = ssh::remote_home(&args.host).await?;

    let triple = release::target_triple(platform, &arch)?;
    let mut staged = release::build_release(source_root, triple).await?;
    staged.git_sha = git_sha.to_string();
    let contract = target_contract(&staged.keeper_contract, git_sha)?;

    let staged_dir = staging_dir(&home, git_sha);
    let staged_text = staged_dir.display().to_string();
    release_stage::stage_over_ssh(&args.host, &staged, &staged_text).await?;
    let staged_program = staged_dir
        .join(RELEASE_BIN_DIR)
        .join(ROOST_PROGRAM)
        .display()
        .to_string();

    let facts = read_facts(&args.host, &staged_program).await?;
    if facts.platform != platform.as_str() {
        return Err(codes::refuse(
            codes::NO_REMOTE_RUNTIME,
            format!(
                "{} reported platform {} after the release was built for {platform}; the target \
                 changed under the rollout",
                args.host, facts.platform
            ),
        ));
    }
    let installed = &facts.installed_environment;
    let coordinator_url = resolve_deploy_env_value(
        ENV_COORDINATOR_URL,
        installed,
        None,
        EnvTarget::Remote,
        ambient,
    )
    .filter(|value| !value.is_empty())
    .ok_or_else(|| {
        codes::refuse(
            codes::NO_COORDINATOR_URL,
            format!(
                "{} has no coordinator URL: a fleet rollout cannot converge a machine that does \
                 not know its coordinator",
                args.host
            ),
        )
    })?;
    let identity_overrides = resolve_remote_deploy_identity(
        &args.host,
        installed,
        args.label.as_deref(),
        args.reachable_addr.as_deref(),
        ambient,
    )?;

    let plan = keeper_step::plan_and_apply(
        args,
        ambient,
        installed,
        &contract,
        &coordinator_url,
        &staged_program,
    )
    .await?;

    let manifest = ApplyManifest::new(
        git_sha,
        &staged_text,
        &staged.digest,
        definition_environment(
            installed,
            &identity_overrides,
            &coordinator_url,
            git_sha,
            args,
            ambient,
        ),
    );
    let transaction = RemoteTransaction::acquire(
        &args.host,
        &txn_session::command_for(&staged_program, TransactionKind::Deploy),
    )
    .await?;
    let applied = apply_over_ssh(&args.host, &staged_program, &manifest).await;
    // The machine is released whether the rollout settled or not: a transaction
    // left held after a refusal would make the NEXT deploy fail for a reason
    // that has nothing to do with it.
    let released = transaction.release().await;
    let report = applied?;
    let outcome = report_outcome(&report, &args.host)?;
    released?;

    if let KeeperPlan {
        update: Some(update),
        fingerprint: Some(fingerprint),
        heartbeat_baseline_ms,
        reconciliation_baseline_ms,
    } = &plan
    {
        info!(
            host = %args.host,
            action = %update.admission.required_action,
            "participant keeper action applied through the coordinator's fence"
        );
        keeper_step::prove_convergence(
            &coordinator_url,
            &args.host,
            fingerprint,
            update,
            *heartbeat_baseline_ms,
            *reconciliation_baseline_ms,
            git_sha,
        )
        .await?;
    }
    info!(host = %args.host, sha = %git_sha, "participant converged");
    Ok(outcome)
}

/// What the target says about itself, asked of the release this rollout staged.
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
                format!("the target's connection died mid-rollout: {cause}"),
            )
        } else {
            codes::refuse(codes::REMOTE_LOST, format!("{cause}\n{}", outcome.detail()))
        }
    })
}

/// Turn the target's report into a refusal, or into what this machine did.
fn report_outcome(report: &ApplyReport, host: &str) -> Result<ParticipantOutcome, CommandFailure> {
    match report.outcome {
        ApplyOutcome::Settled => Ok(ParticipantOutcome {
            changed: report.definition_changed,
        }),
        ApplyOutcome::Recovered => Err(codes::refuse(
            codes::SETTLEMENT_FAILED,
            format!(
                "{host} recovered an unfinished deploy of its own instead of taking this one; its \
                 release is unchanged and the rollout cannot continue"
            ),
        )),
        ApplyOutcome::RolledBack => Err(codes::refuse(
            codes::KEEPER_NOT_ADOPTABLE,
            format!("{host}: {}", report.detail),
        )),
        ApplyOutcome::Unsettled => Err(codes::refuse(
            codes::SETTLEMENT_FAILED,
            format!(
                "{host} was left past the point where this rollout could be undone: {}. Its \
                 deploy journal is retained and the next deploy will resolve it.",
                report.detail
            ),
        )),
        ApplyOutcome::Refused => Err(codes::refuse(
            codes::NO_REMOTE_RUNTIME,
            format!("{host}: {}", report.detail),
        )),
    }
}
