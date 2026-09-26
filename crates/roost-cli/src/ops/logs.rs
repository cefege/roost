//! `roost logs <coord|worker> [--tail N]` — follow a service's own log files.
//! Called by the crate's dispatcher. Both directories come from `roost-host`, so
//! the `ROOST_COORD_LOG_DIR` / `ROOST_WORKER_LOG_DIR` the installers set are
//! honoured here for the same reason they are honoured there: one naming
//! oracle, so a log the operator is told to read is the log that exists.
//!
//! The files are handed to `tail -F` rather than read here. Following a file
//! this process did not open is what `tail` is for, and re-implementing
//! follow-the-rename across a rotation is a bug surface with no upside.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, ValueEnum};
use roost_host::{EnvSource, HostPlatform, ProcessEnv, coord_log_dir, worker_log_dir};

use crate::command_error::CommandFailure;

/// A log file this large is a service that has stopped rotating, and following
/// it costs the operator more than the answer is worth. The v2 threshold, kept
/// so the same file warns on the same day.
pub const LOG_ROTATE_WARN_BYTES: u64 = 100 * 1024 * 1024;

/// How many existing lines print before the follow begins. Roughly one screen
/// of a quiet service and one of a busy one.
pub const DEFAULT_TAIL_LINES: u64 = 100;

#[derive(Debug, Args)]
#[command(about = "Tail a service's logs")]
pub struct LogsArgs {
    #[arg(value_name = "APP", value_enum)]
    pub app: LogApp,
    #[arg(long, short = 'n', value_name = "LINES", default_value_t = DEFAULT_TAIL_LINES)]
    pub tail: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LogApp {
    Coord,
    Worker,
}

impl LogApp {
    pub fn as_str(self) -> &'static str {
        match self {
            LogApp::Coord => "coord",
            LogApp::Worker => "worker",
        }
    }

    /// stdout first, then stderr: the two channels answer different questions,
    /// and an operator reading a terminal wants them in that order.
    fn files(
        self,
        env: &dyn EnvSource,
        platform: HostPlatform,
    ) -> Result<Vec<PathBuf>, CommandFailure> {
        let dir = match self {
            LogApp::Coord => coord_log_dir(env, platform)?,
            LogApp::Worker => worker_log_dir(env, platform)?,
        };
        Ok(vec![dir.join("main.out.log"), dir.join("main.err.log")])
    }
}

pub fn run(args: &LogsArgs) -> Result<ExitCode, CommandFailure> {
    let env = ProcessEnv::new();
    let platform = roost_host::supported_host_platform()?;
    let files = args.app.files(&env, platform)?;
    warn_oversized(&files, platform);
    let present: Vec<&Path> = files
        .iter()
        .map(PathBuf::as_path)
        .filter(|path| path.exists())
        .collect();
    if present.is_empty() {
        return Err(CommandFailure::generic(format!(
            "no {} log files at {}",
            args.app.as_str(),
            files
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(" or ")
        )));
    }
    // `-F` rather than `-f`: the service managers rotate by rename, and a
    // descriptor-following tail sits on the rotated file forever while the live
    // one goes unwatched — which is exactly when an operator is watching.
    let status = std::process::Command::new("tail")
        .arg("-F")
        .arg("-n")
        .arg(args.tail.to_string())
        .args(present)
        .status()?;
    Ok(if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn warn_oversized(files: &[PathBuf], platform: HostPlatform) {
    for path in files {
        let Ok(metadata) = std::fs::metadata(path) else {
            continue;
        };
        if metadata.len() <= LOG_ROTATE_WARN_BYTES {
            continue;
        }
        let megabytes = metadata.len() / (1024 * 1024);
        // newsyslog is macOS-only; on Linux the logrotate timer is the remedy
        // and truncating by hand is the emergency one.
        let rotate = match platform {
            HostPlatform::MacOs => {
                " Rotate via: sudo newsyslog -vf /etc/newsyslog.d/roost-coord.conf or"
            }
            // v3 ships Linux and macOS only; on Linux the logrotate timer is
            // the remedy and truncating by hand is the emergency one.
            HostPlatform::Linux | HostPlatform::Windows => "",
        };
        eprintln!(
            "[roost-logs] {} is {megabytes}MB (>{}MB).{rotate} truncate: > \"{}\"",
            path.display(),
            LOG_ROTATE_WARN_BYTES / (1024 * 1024),
            path.display()
        );
    }
}
