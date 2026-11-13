//! Runs the source-pinned OMP TUI/native-HTML differential gate.
//! Called by the xtask CLI; Bun owns checkout preparation and browser/runtime
//! diagnostics under this repository's target directory.

use std::ffi::OsString;
use std::process::{Command, ExitCode};

use clap::Args;

#[derive(Args)]
pub struct OmpParityArgs {
    /// Prepare only the owned pinned source and matching published native addon.
    #[arg(long)]
    prepare_source: bool,
    /// Arguments passed unchanged to the Bun runner, after `--`.
    #[arg(last = true)]
    runner_args: Vec<OsString>,
}

pub fn run(arguments: &OmpParityArgs) -> ExitCode {
    if arguments.prepare_source && !arguments.runner_args.is_empty() {
        eprintln!("xtask omp-parity: --prepare-source does not accept runner arguments");
        return ExitCode::FAILURE;
    }
    let repository = crate::source_tree::repo_root();
    let script = if arguments.prepare_source {
        "deploy/omp/parity/source.ts"
    } else {
        "deploy/omp/parity/run.ts"
    };
    let mut command = Command::new("bun");
    command.current_dir(&repository).arg("run").arg(script);
    if arguments.prepare_source {
        command.arg("--repo-root").arg(&repository);
    } else {
        command.args(&arguments.runner_args);
    }
    match command.status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => {
            eprintln!("xtask omp-parity: Bun exited with {status}");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!(
                "xtask omp-parity: cannot start Bun (install the pinned source's supported Bun version): {error}"
            );
            ExitCode::FAILURE
        }
    }
}
