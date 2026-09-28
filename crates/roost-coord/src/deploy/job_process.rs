//! The subprocess behind one deploy job: spawn it, pump its stdout and stderr
//! into the job line by line, bound it by the deploy timeout, and finish the job
//! with its exit. Called by `deploy::start` for every job it opens. Ports the
//! `Bun.spawn` half of `startDeploy` in apps/coord/src/deploy/deploy-jobs.ts.
//!
//! WHY TWENTY MINUTES. A real POSIX deploy fetches the commit, runs a frozen
//! install and builds the SPA on the target, which routinely passes three
//! minutes. At 180 s the timer killed deploys mid-activation -- the journal
//! recovered them, but the job was reported failed and the catch-up host went
//! into cooldown while the release had in fact landed. The bound is for a
//! genuinely hung deploy, past the remote lease's own 15-minute renewal window.

use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::io::AsyncRead;
use tokio::process::{Child, Command};
use tokio_util::io::ReaderStream;

use crate::deploy::jobs::{DeployJob, DeployJournal};

/// How long one deploy may run before it is killed.
pub const DEPLOY_TIMEOUT: Duration = Duration::from_millis(1_200_000);

/// Spawn `command` as `job`'s subprocess and supervise it to the end.
///
/// A spawn that fails finishes the job at once with the reason, and the job
/// still exists: the caller already answered with its id, and a reader that
/// opens it reads why it never ran.
pub fn spawn_deploy_process(journal: &Arc<DeployJournal>, job: Arc<DeployJob>, mut command: Command) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match command.spawn() {
        Ok(child) => {
            tracing::info!(job_id = job.job_id(), host = job.host(), pid = child.id(),
                "deploy job: subprocess spawned");
            tokio::spawn(supervise(Arc::clone(journal), job, child));
        }
        Err(error) => {
            journal.finish_job(&job, None, Some(error.to_string()));
        }
    }
}

/// Pump both pipes and await the exit, then finish the job with the outcome.
///
/// A pipe that fails to read finishes the job at once with that error, without
/// waiting for the exit -- v2's `Promise.all` rejection -- and the process is
/// left to end on its own rather than killed on a read failure.
async fn supervise(journal: Arc<DeployJournal>, job: Arc<DeployJob>, mut child: Child) {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let outcome = tokio::try_join!(
        pump_lines(stdout, &job),
        pump_lines(stderr, &job),
        await_exit(&mut child),
    );
    match outcome {
        Ok(((), (), (status, timed_out))) => {
            let exit = status.code();
            let error = if timed_out {
                Some(format!("deploy timed out after {}s", DEPLOY_TIMEOUT.as_secs()))
            } else if exit == Some(0) {
                None
            } else {
                Some(format!("deploy exit {}", render_exit(exit)))
            };
            journal.finish_job(&job, exit, error);
        }
        Err(error) => journal.finish_job(&job, None, Some(error.to_string())),
    }
}

/// The exit status, and whether the deadline killed the process to get it.
async fn await_exit(child: &mut Child) -> std::io::Result<(ExitStatus, bool)> {
    match tokio::time::timeout(DEPLOY_TIMEOUT, child.wait()).await {
        Ok(status) => Ok((status?, false)),
        Err(_) => {
            tracing::warn!(pid = child.id(), "deploy job: timed out; killing the subprocess");
            // A process that exited between the deadline and the kill has
            // nothing left to kill; its exit is awaited below either way.
            if let Err(error) = child.start_kill() {
                tracing::debug!(%error, "deploy job: the kill found no process");
            }
            Ok((child.wait().await?, true))
        }
    }
}

/// Feed one pipe into the job a line at a time, flushing a trailing partial
/// line at end of stream.
///
/// Lines are cut at the newline BYTE and decoded afterwards, so a multi-byte
/// character can never be split across two reads: UTF-8 never uses `0x0A`
/// inside a sequence.
async fn pump_lines(
    pipe: Option<impl AsyncRead + Unpin>,
    job: &DeployJob,
) -> std::io::Result<()> {
    let Some(pipe) = pipe else {
        return Ok(());
    };
    let mut chunks = ReaderStream::new(pipe);
    let mut pending: Vec<u8> = Vec::new();
    while let Some(chunk) = chunks.next().await {
        pending.extend_from_slice(&chunk?);
        let mut consumed = 0;
        while let Some(offset) = pending[consumed..].iter().position(|byte| *byte == b'\n') {
            let end = consumed + offset;
            job.emit_line(&String::from_utf8_lossy(&pending[consumed..end]));
            consumed = end + 1;
        }
        pending.drain(..consumed);
    }
    if !pending.is_empty() {
        job.emit_line(&String::from_utf8_lossy(&pending));
    }
    Ok(())
}

/// An exit code as v2's template rendered it: a signal death has no code and
/// read as `null` there.
fn render_exit(exit: Option<i32>) -> String {
    exit.map_or_else(|| "null".to_owned(), |code| code.to_string())
}
