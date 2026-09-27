//! `roost dev` — a coordinator, the worker that dials it, and the web dev
//! server, started together and stopped together. Called by the crate's
//! dispatcher; depends on `plan` for what to start, `supervisor` for the
//! fan-out, and the coordinator's own boot resolver so the bind the worker is
//! told to dial is one that resolver already accepted.
//!
//! It prints nothing. A server mode's stdio is the service's log channel, and
//! this parent is the one process in the tree whose line would land in the
//! middle of a child's.

pub mod plan;
pub mod signal;
pub mod supervisor;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;
use roost_host::{EnvSource, HostPlatform, ProcessEnv, supported_host_platform};

use crate::command_error::CommandFailure;
use crate::daemon::{CoordArgs, coord_boot};
use crate::dev::plan::dev_plan;
use crate::dev::supervisor::{DevStack, StopPolicy, TerminationSignal, TerminationWatch};

/// The dev stack is one shape: a coordinator, the worker that dials it, and the
/// web dev server. There is nothing to configure, so a flag here would be an
/// argument the contract does not have.
#[derive(Debug, Args)]
#[command(about = "Run the coordinator, the worker and the web dev server together")]
pub struct DevArgs {}

/// What the boot resolver decided before anything was started. The servers
/// themselves are child processes, so what they need from this process is the
/// one fact a child cannot resolve for itself: which coordinator to dial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevBoot {
    /// The bind the dev coordinator listens on, as `roost-host` resolved it out
    /// of this process's own environment.
    pub coordinator_bind: String,
    /// The same bind as a URL, which is what the worker child is told.
    pub coordinator_url: String,
}

pub async fn run(_args: &DevArgs) -> Result<ExitCode, CommandFailure> {
    let boot = resolve_dev_boot(&ProcessEnv::new(), supported_host_platform()?)?;
    let stack = dev_plan(&roost_executable()?, &boot.coordinator_url);
    // Installed before the first child exists, so a signal that arrives during
    // startup is caught instead of ending this process with children attached.
    let mut watch = TerminationWatch::install()?;
    let mut running = DevStack::start(&stack, StopPolicy::default()).await?;
    let forwarded = tokio::select! {
        signal = watch.next() => {
            tracing::info!(?signal, "dev received a termination signal");
            // A watch with no signal left to receive is a reason to stop the
            // stack, not a reason to leave it running unwatched.
            signal.unwrap_or(TerminationSignal::Interrupt)
        }
        exited = running.wait_for_exit() => {
            match exited {
                Some(exit) => tracing::info!(server = exit.name, code = ?exit.code, "a dev server exited; stopping the rest"),
                None => tracing::warn!("no dev server was left to wait for"),
            }
            TerminationSignal::Interrupt
        }
    };
    running.stop(forwarded).await?;
    Ok(ExitCode::SUCCESS)
}

/// Resolve the dev coordinator's boot through the SAME resolver the standalone
/// subcommand uses, against an environment overlaid in memory.
///
/// Nothing is exported: a variable that reached the loader by being set in the
/// ambient environment would outlive the command and describe this machine's
/// dev identity to whatever ran next in the same shell — the leak `roost push`
/// pays for when it deploys several targets from one process.
///
/// The worker is not resolved here. Its boot needs a registry fingerprint,
/// which a checkout does not have until the worker has resolved one, so
/// refusing here would refuse every first `roost dev`; the worker child runs
/// that same resolver and refuses for itself, before it dials anything.
pub fn resolve_dev_boot(
    env: &dyn EnvSource,
    platform: HostPlatform,
) -> Result<DevBoot, CommandFailure> {
    let coordinator = coord_boot::resolve_from(
        &CoordArgs {
            bind: None,
            db: None,
        },
        env,
        platform,
    )?;
    Ok(DevBoot {
        coordinator_url: plan::coordinator_url(&coordinator.config.bind),
        coordinator_bind: coordinator.config.bind,
    })
}

/// The binary running now, so the dev coordinator and worker are the executable
/// the operator invoked rather than whichever `roost` happens to be first on
/// `PATH`.
fn roost_executable() -> Result<PathBuf, CommandFailure> {
    std::env::current_exe().map_err(|error| {
        CommandFailure::generic(format!("cannot find the running roost binary: {error}"))
    })
}
