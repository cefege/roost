//! Running the external tools the harness orchestrates: build steps, `git`
//! queries, and POSIX signals. Called by `prepare`, `stack::boot` and `run`.
//! Signals go through the `kill` binary because this crate forbids `unsafe`.

use std::path::Path;

use tokio::process::Command;

use crate::error::BenchError;

/// Run `program args…` in `cwd` to completion, inheriting stdio, failing on a
/// non-zero exit.
pub async fn run_step(program: &str, args: &[&str], cwd: &Path) -> Result<(), BenchError> {
    let rendered = render(program, args);
    tracing::info!(command = %rendered, cwd = %cwd.display(), "bench step start");
    let status = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .status()
        .await
        .map_err(|error| BenchError::io(format!("spawning `{rendered}`"), error))?;
    if !status.success() {
        return Err(BenchError::CommandFailed {
            command: rendered,
            status: status.to_string(),
        });
    }
    tracing::info!(command = %rendered, "bench step done");
    Ok(())
}

/// Run a cargo or dx build under the machine-wide build lock.
pub async fn run_locked_build(args: &[&str], cwd: &Path) -> Result<(), BenchError> {
    let mut locked = vec!["/tmp/roost-cargo.lock"];
    locked.extend_from_slice(args);
    run_step("flock", &locked, cwd).await
}

/// The trimmed stdout of a command that must succeed.
pub async fn capture_stdout(
    program: &str,
    args: &[&str],
    cwd: &Path,
) -> Result<String, BenchError> {
    let rendered = render(program, args);
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .await
        .map_err(|error| BenchError::io(format!("spawning `{rendered}`"), error))?;
    if !output.status.success() {
        return Err(BenchError::CommandFailed {
            command: rendered,
            status: output.status.to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Whether `program --version` runs: the presence check for a required tool.
pub async fn tool_present(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
}

/// Deliver `signal` (`TERM`, `KILL`) to each pid. A pid that already exited is
/// not an error: every caller is cleaning up.
pub async fn signal_pids(signal: &str, pids: &[u32]) {
    if pids.is_empty() {
        return;
    }
    let mut args = vec![format!("-{signal}")];
    args.extend(pids.iter().map(u32::to_string));
    let delivered = Command::new("kill")
        .args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;
    if let Err(error) = delivered {
        tracing::warn!(%error, signal, ?pids, "kill could not be spawned");
    }
}

fn render(program: &str, args: &[&str]) -> String {
    std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
}
