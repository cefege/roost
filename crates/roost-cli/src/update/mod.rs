//! `roost update` — replace this binary with the latest published release, on
//! this machine, atomically. Called by the crate's dispatcher; depends on the
//! update group's own modules and on `roost-host` for the platform and build
//! identity, and on nothing outside this crate.
//!
//! The order is the safety property. An interrupted update is resolved FIRST,
//! because a machine that lost power mid-swap already has a binary in a state
//! this run has to account for before it makes a second change to it. Then the
//! release is resolved, the candidate is downloaded and proved, the keeper is
//! admitted, and only then does the rename happen.
//!
//! THE WORDING HERE IS NOT THE FLEET'S. `roost status` prints `Up to date` to
//! mean "this worker's commit equals the coordinator's". This command's
//! "already the latest release" means "this binary is the newest one published".
//! They are the same three words about two different facts, and a shared
//! constant would make a change to one silently edit the other. The fleet's five
//! wordings live in `status::update_state`; this module does not import them and
//! `tests/update_release_decision.rs` holds the two apart.

pub mod candidate;
pub mod journal;
pub mod keeper;
pub mod local_keeper;
pub mod recovery;
pub mod release;
pub mod rollout;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;
use roost_host::{HostPlatform, ProcessEnv};
use tracing::info;

use crate::command_error::CommandFailure;
use crate::update::journal::KeeperRecord;
use crate::update::local_keeper::{decide_keeper_action, local_keeper, self_update_service_dir};
use crate::update::recovery::RecoveryOutcome;
use crate::update::release::{host_arch, release_asset_name};
use crate::update::rollout::{InstalledBinary, ReplaceOutcome, read_installed};

/// What this command prints when there is nothing to do. Its own table,
/// deliberately separate from the fleet's five, and public so the executable
/// spec can hold the two apart rather than trusting that nobody imports both.
pub const ALREADY_LATEST: &str = "already the latest release";

/// What this command prints when no release has been published.
pub const NO_PUBLISHED_RELEASE: &str = "no published release to update to";

/// Why a source build refuses to replace itself.
const SOURCE_CHECKOUT_REFUSAL: &str =
    "this is a source build, which cannot replace itself; install the release binary";

/// `roost update` — no arguments. Everything it needs, it resolves.
#[derive(Debug, Args)]
#[command(about = "Replace this roost binary with the latest published release")]
pub struct UpdateArgs {}

/// Run the command.
pub async fn run(_args: &UpdateArgs) -> Result<ExitCode, CommandFailure> {
    let env = ProcessEnv::new();
    let platform = roost_host::supported_host_platform()?;
    if platform == HostPlatform::Windows {
        return Err(CommandFailure::generic(
            "self-update is not shipped for Windows; v3 hosts run on macOS and Linux",
        ));
    }
    let service_dir = self_update_service_dir(&env, platform)?;
    resolve_recovery_first(&service_dir)?;
    let identity = roost_host::build_identity(&env);
    let listing = release::fetch_latest_release_tag(&env, host_arch(platform)).await?;
    if listing.tag.is_empty() {
        println!(">> {NO_PUBLISHED_RELEASE}");
        return Ok(ExitCode::SUCCESS);
    }
    if !needs_update(&identity.artifact_version, &listing.tag)? {
        println!(">> {ALREADY_LATEST} ({})", listing.tag);
        return Ok(ExitCode::SUCCESS);
    }
    if !identity.is_compiled {
        return Err(CommandFailure::generic(SOURCE_CHECKOUT_REFUSAL));
    }
    let asset = release_asset_name(platform, &listing.arch)?;
    let executable = current_executable()?;
    info!(
        version = %identity.artifact_version,
        latest = %listing.tag,
        asset,
        "roost update resolving the latest release",
    );
    let verified = release::download_and_verify(&env, asset, &executable)
        .await
        .map_err(refusal)?;
    let installed = read_installed(&executable).map_err(refusal)?;
    let keeper = local_keeper(&env, platform).await?;
    let candidate = keeper::probe_candidate_contract(&verified.path)
        .map_err(|error| CommandFailure::generic(error.to_string()))?;
    let installed = InstalledBinary {
        keeper: Some(decide_keeper_action(&candidate, keeper.as_ref())?),
        ..installed
    };
    let outcome = rollout::replace_executable(
        &executable,
        &verified,
        &listing.tag,
        &installed,
        &service_dir,
        crate::wall_clock::now_ms(),
    )
    .map_err(refusal)?;
    report(&outcome);
    Ok(ExitCode::SUCCESS)
}

/// Resolve an in-flight update before anything else, and refuse to carry on when
/// the last one had to be rolled back.
///
/// A rollback means the swap already failed once for a reason recovery did not
/// discover. Retrying it immediately is how a release that just failed gets
/// installed anyway, so the operator re-runs the command — which is a decision
/// rather than a retry loop.
fn resolve_recovery_first(service_dir: &std::path::Path) -> Result<(), CommandFailure> {
    match recovery::resolve_interrupted_update(service_dir).map_err(refusal)? {
        RecoveryOutcome::PreviousRestored => Err(CommandFailure::generic(
            "the previous `roost update` was rolled back to the previous binary and is not \
             being retried automatically",
        )),
        RecoveryOutcome::Nothing
        | RecoveryOutcome::PreparedCleaned
        | RecoveryOutcome::InstalledCommitted => Ok(()),
    }
}

/// The release version a tag names, with any build metadata removed.
///
/// Build metadata is dropped rather than compared, because a rebuild of the
/// same release carries a different `+sha` and comparing it would make a binary
/// update itself forever against a release that has not changed.
pub fn canonical_release_version(version: &str) -> Result<String, CommandFailure> {
    let trimmed = version.trim().trim_start_matches(['v', 'V']);
    let core = trimmed.split(['-', '+']).next().unwrap_or_default();
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(CommandFailure::generic(format!(
            "{version:?} is not a release version"
        )));
    }
    Ok(parts.join("."))
}

/// Whether the running binary is behind the published release.
///
/// A source checkout is always behind: it is not a published artifact, and there
/// is nothing to compare it against. An empty tag is never behind, because a
/// listing that named no release is a question this command could not answer —
/// not an answer that there is nothing to do.
pub fn needs_update(current: &str, latest_tag: &str) -> Result<bool, CommandFailure> {
    if latest_tag.is_empty() {
        return Ok(false);
    }
    if current == roost_host::DEV_BUILD_STAMP {
        return Ok(true);
    }
    Ok(canonical_release_version(current)? != canonical_release_version(latest_tag)?)
}

/// The binary this process is running from, resolved once.
fn current_executable() -> Result<PathBuf, CommandFailure> {
    std::env::current_exe().map_err(|error| {
        CommandFailure::generic(format!("this process cannot name its own binary: {error}"))
    })
}

fn refusal(error: impl std::fmt::Display) -> CommandFailure {
    CommandFailure::generic(error.to_string())
}

/// What the operator is told, on stdout, because it is the answer to the
/// question they asked.
fn report(outcome: &ReplaceOutcome) {
    println!(">> updated to {}", outcome.target_version);
    println!(">> the running services keep the old binary until they restart:");
    match &outcome.keeper {
        KeeperRecord::NoRunningKeeper { reason } => {
            println!(">> no keeper was running here ({reason})");
        }
        KeeperRecord::Admitted {
            worker_fingerprint,
            required_action,
            ..
        } => {
            println!(">> keeper on {worker_fingerprint} preserved ({required_action})");
        }
    }
}
