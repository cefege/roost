//! `roost coord`, `roost worker` and `roost keeper <socket>` — the three server
//! modes, and the only commands whose output is somebody else's log. Each owns
//! its own argument parsing and boot-config resolution, then hands a
//! validated config to its daemon crate and blocks.
//!
//! The split is the whole point: `roost-host` decides what a valid config IS,
//! this module refuses before starting anything, and `serve` blocks and
//! RETURNS. A daemon that called `process::exit` could not be run from a test
//! or from a subcommand, and could not have its shutdown and exit code decided
//! by the process that started it.
//!
//! None of these three prints a progress line. In v2 they printed nothing
//! either, and anything they did print would interleave with the service's own
//! JSON log channel — which `roost doctor` reads.

pub mod coord_boot;
pub mod worker_boot;

use std::process::ExitCode;

use clap::Args;

use crate::command_error::CommandFailure;

#[derive(Debug, Args)]
#[command(about = "Run the coordinator in this process (server mode)")]
pub struct CoordArgs {
    /// Override the loopback bind. The value is validated by `roost-host`
    /// against the same rule the installer enforces, so a bind that would
    /// expose the coordinator's unauthenticated surfaces is refused before
    /// anything binds a socket.
    #[arg(long, value_name = "HOST:PORT")]
    pub bind: Option<String>,
    /// Override the coordinator database path.
    #[arg(long, value_name = "PATH")]
    pub db: Option<String>,
}

#[derive(Debug, Args)]
#[command(about = "Run the worker in this process (server mode)")]
pub struct WorkerArgs {
    /// The coordinator this worker dials. Read from the installed service
    /// environment when absent.
    #[arg(long, value_name = "URL")]
    pub coordinator_url: Option<String>,
}

#[derive(Debug, Args)]
#[command(about = "Run the keeper in this process (the self-exec target)")]
pub struct KeeperArgs {
    /// The keeper socket path the worker connects to. Positional and required:
    /// a keeper with no socket has nothing to serve and no way to be found.
    #[arg(value_name = "SOCKET_PATH")]
    pub socket: String,
}

pub async fn run_coord(args: &CoordArgs) -> Result<ExitCode, CommandFailure> {
    let boot = coord_boot::resolve(args)?;
    roost_coord::serve(boot).await?;
    Ok(ExitCode::SUCCESS)
}

pub async fn run_worker(args: &WorkerArgs) -> Result<ExitCode, CommandFailure> {
    let boot = worker_boot::resolve(args)?;
    roost_worker::serve(boot)?;
    Ok(ExitCode::SUCCESS)
}

/// The keeper is a SEPARATE BINARY in v3 (`roost-keeper`), shipped apart from
/// `roost` precisely so a coordinator deploy never disturbs a live PTY. This
/// subcommand stays because the worker spawns a keeper by argv and because the
/// upgrade harness addresses it; it refuses rather than silently running
/// something else, so a stale caller finds out immediately.
pub fn run_keeper(args: &KeeperArgs) -> Result<ExitCode, CommandFailure> {
    let binary = crate::ops::keeper_contract::keeper_binary_path()?;
    let status = std::process::Command::new(binary)
        .arg(&args.socket)
        .status()?;
    Ok(if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
