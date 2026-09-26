//! `roost reset` — stop the two local services and delete the coordinator
//! database. Called by the crate's dispatcher. The database triad and nothing
//! else: keys, journals and the worker's own state survive, because a reset
//! that removed them would also remove every paired device, and "start over
//! with a fresh fleet" is a different command an operator asks for explicitly.
//!
//! The data directory comes from `roost-host`, so `ROOST_COORD_DATA_DIR` —
//! which the installer sets on both platforms — decides what gets deleted. An
//! isolated test install therefore resets its own database and never this
//! machine's, which is the only reason this command is safe to script.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;
use roost_host::{
    EnvSource, HostPlatform, ProcessEnv, coord_data_dir, coord_service_label, worker_service_label,
};

use crate::command_error::CommandFailure;

#[derive(Debug, Args)]
#[command(about = "Stop the local services and delete the coordinator database")]
pub struct ResetArgs {
    /// Print every step and every path, and remove nothing.
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(args: &ResetArgs) -> Result<ExitCode, CommandFailure> {
    let env = ProcessEnv::new();
    let platform = roost_host::supported_host_platform()?;
    println!(">> stop coord + worker services");
    if !args.dry_run {
        stop_services(&env, platform)?;
    }
    let base = coordinator_database(&env, platform)?;
    // All three, not just the database: a reset that leaves `-wal` or `-shm`
    // behind leaves a database whose next open replays a write the operator
    // just asked to discard.
    for path in database_triad(&base) {
        if !path.exists() {
            continue;
        }
        println!(">> rm {}", path.display());
        if !args.dry_run {
            std::fs::remove_file(&path)?;
        }
    }
    if args.dry_run {
        println!(">> done (dry run: nothing was removed)");
    } else {
        println!(">> done");
    }
    Ok(ExitCode::SUCCESS)
}

pub fn coordinator_database(
    env: &dyn EnvSource,
    platform: HostPlatform,
) -> Result<PathBuf, CommandFailure> {
    Ok(coord_data_dir(env, platform)?.join(roost_host::coord_config::COORD_DB_FILE_NAME))
}

pub fn database_triad(base: &std::path::Path) -> [PathBuf; 3] {
    let text = base.to_string_lossy().to_string();
    [
        base.to_path_buf(),
        PathBuf::from(format!("{text}-wal")),
        PathBuf::from(format!("{text}-shm")),
    ]
}

/// Ask each platform's service manager to stop the two services. A service
/// that was never installed is the normal state on a machine being reset for
/// the first time, so a non-zero exit from the manager is not a reason to
/// refuse: what the operator asked for is a stopped service, and a manager
/// reporting "no such unit" has stopped it.
fn stop_services(env: &dyn EnvSource, platform: HostPlatform) -> Result<(), CommandFailure> {
    let coord = coord_service_label(env, platform)?;
    let worker = worker_service_label(env, platform)?;
    for argv in stop_commands(platform, &coord, &worker) {
        let _ = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .status();
    }
    Ok(())
}

/// The stop command per platform, as argv rather than a shell string: a label
/// can come from the environment, and a shell string would be a quoting bug
/// waiting for a label with a space in it.
pub fn stop_commands(platform: HostPlatform, coord: &str, worker: &str) -> Vec<Vec<String>> {
    match platform {
        HostPlatform::Linux => ["roost3-coord", "roost3-worker"]
            .iter()
            .map(|unit| {
                vec![
                    "systemctl".to_string(),
                    "--user".to_string(),
                    "stop".to_string(),
                    (*unit).to_string(),
                ]
            })
            .collect(),
        HostPlatform::MacOs => [coord, worker]
            .iter()
            .map(|label| {
                vec![
                    "launchctl".to_string(),
                    "bootout".to_string(),
                    format!("gui/{}/{}", current_uid(), label),
                ]
            })
            .collect(),
        // v3 ships Linux and macOS only; a Windows host is refused at startup.
        HostPlatform::Windows => Vec::new(),
    }
}

/// The uid in the launchd per-user domain. `libc::getuid` is an `unsafe` call
/// and this crate forbids `unsafe`, so it is read from the home directory the
/// account owns — which is the same uid `launchctl` will be asked about.
fn current_uid() -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::env::var("HOME")
        .ok()
        .and_then(|home| std::fs::metadata(home).ok())
        .map(|metadata| metadata.uid())
        .unwrap_or(0)
}
