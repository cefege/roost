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
//! wordings live in `roost_protocol::fleet_update`; this module does not import them and
//! `tests/update_release_decision.rs` holds the two apart.

pub mod assets;
pub mod candidate;
pub mod journal;
pub mod keeper;
pub mod local_keeper;
pub mod recovery;
pub mod release;
pub mod rollout;
pub mod version;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Args;
use roost_host::{EnvSource, HostPlatform, ProcessEnv};
use tracing::info;

use crate::command_error::CommandFailure;
use crate::quickstart::web_source;
use crate::services::web_bundle;
use crate::update::journal::KeeperRecord;
use crate::update::local_keeper::{decide_keeper_action, local_keeper, self_update_service_dir};
use crate::update::recovery::RecoveryOutcome;
use crate::update::release::{host_arch, release_asset_name};
use crate::update::rollout::{InstalledBinary, ReplaceOutcome, read_installed};
use crate::update::version::{needs_update, release_channel};

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
    let channel = release_channel(&env, &identity.artifact_version)?;
    let listing = release::fetch_latest_release_tag(host_arch(platform), channel).await?;
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
        channel = ?channel,
        asset,
        "roost update resolving the latest release",
    );
    let verified = release::download_and_verify(&env, &listing.tag, asset, &executable)
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
    install_web_bundle(&env, &executable, &listing.tag).await;
    report(&outcome);
    Ok(ExitCode::SUCCESS)
}

/// Put the new release's web bundle beside the binary that was just replaced.
///
/// The bundle travels with the binaries, so a swap that moved only the binary
/// would leave a machine running the new coordinator over the old page — an
/// index that references asset hashes the new build does not ship, which loads
/// its shell and then fails every request for its code. Both definitions
/// already point at this directory, which is why none is rewritten here.
///
/// A binary outside a release tree — a tarball dropped in `~/bin` — has no
/// bundle directory to put one in. That is reported rather than worked around,
/// because the alternative is writing `~/web` and leaving the operator to find
/// it.
async fn install_web_bundle(env: &dyn EnvSource, executable: &Path, tag: &str) {
    let release_dir = executable.parent().filter(|bin_dir| {
        bin_dir.file_name().and_then(std::ffi::OsStr::to_str)
            == Some(crate::deploy::apply_release::RELEASE_BIN_DIR)
    });
    let Some(bin_dir) = release_dir else {
        eprintln!(
            ">> {} is not inside a release's bin directory, so no web bundle was installed \
             beside it",
            executable.display()
        );
        return;
    };
    let destination = web_bundle::release_web_dir(bin_dir);
    let archive = match web_source::download_web_bundle(env, tag).await {
        Ok(archive) => archive,
        Err(failure) => {
            // The swap already settled, and the page is a second problem while
            // the binary is the first. Refusing here would report a completed
            // update as failed and send the operator to re-run a swap that has
            // already happened.
            eprintln!(">> the {tag} web bundle could not be installed: {failure}");
            eprintln!(
                ">> the coordinator and worker keep serving whatever bundle is already there"
            );
            return;
        }
    };
    match web_bundle::install_from_tarball(&archive, &destination) {
        Ok(installed) => {
            let _ = std::fs::remove_file(&archive);
            eprintln!(
                ">> installed the {tag} web bundle ({} files) into {}",
                installed.files,
                installed.root.display()
            );
        }
        Err(error) => {
            let _ = std::fs::remove_file(&archive);
            eprintln!(">> the {tag} web bundle could not be unpacked: {error}");
        }
    }
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
