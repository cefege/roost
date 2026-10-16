//! The `bash` tool: runs one shell command in the foreground and turns it into a
//! background job when asked (`async: true`), when it outlives the auto-background
//! threshold, or when the model steers mid-call. A backgrounded call returns at
//! once with its job id; the settled result is reported later by the job registry.

use std::{fmt::Write as _, path::PathBuf, sync::Arc, time::Duration};

use roost_protocol::wire::agent_chat::{BashArgs, JobState, async_details_json};
use tokio::{
    sync::{Mutex, Notify, mpsc},
    task::{JoinError, JoinHandle},
};
use tokio_util::sync::CancellationToken;

use crate::{
    bash_capture::BashCapture,
    bash_process::{BashProcess, LiveSink, run_bash_process},
    jobs::{JobLimitReached, JobRegistry},
    outcome::ToolOutcome,
};

pub const DEFAULT_AUTO_BACKGROUND_AFTER: Duration = Duration::from_secs(60);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_TIMEOUT_SECS: u64 = 3600;

/// Everything one `bash` call needs from its host.
#[derive(Debug)]
pub struct BashCall {
    pub data_dir: PathBuf,
    pub cwd: PathBuf,
    pub conversation_id: String,
    pub args: BashArgs,
    pub timeout_ms: u32,
    pub auto_background_after: Duration,
    /// Fired by a steer; detaches this call into a background job.
    pub detach: Arc<Notify>,
    pub jobs: Arc<JobRegistry>,
}

/// The state one bash process writes into. The foreground call owns it until a
/// detach hands the same state to the job registry.
struct ProcessRun {
    capture: Arc<Mutex<BashCapture>>,
    live: LiveSink,
    cancel: CancellationToken,
}

pub async fn bash_tool(
    call: BashCall,
    out: &mpsc::Sender<String>,
    cancel: CancellationToken,
) -> ToolOutcome {
    let timeout = resolve_timeout(call.args.timeout, call.timeout_ms);
    if call.args.background == Some(true) {
        return start_explicit_job(&call, timeout);
    }
    let run = ProcessRun {
        capture: Arc::new(Mutex::new(BashCapture::new(&call.data_dir))),
        live: LiveSink::attached(out.clone()),
        cancel: CancellationToken::new(),
    };
    let mut process = spawn_process(&call, timeout, &run);
    let auto_threshold = auto_background_threshold(call.auto_background_after, timeout);
    let auto_timer = tokio::time::sleep(auto_threshold.unwrap_or_default());
    tokio::pin!(auto_timer);
    let mut auto_pending = auto_threshold.is_some();
    loop {
        tokio::select! {
            joined = &mut process => return join_outcome(joined),
            _ = cancel.cancelled() => {
                run.cancel.cancel();
                return join_outcome(process.await);
            }
            _ = &mut auto_timer, if auto_pending => {
                auto_pending = false;
                if let Ok(job_id) = register_process(&call, &run) {
                    return detach_to_job(&call, &run, process, &job_id, timeout, false).await;
                }
            }
            _ = call.detach.notified() => {
                if let Ok(job_id) = register_process(&call, &run) {
                    return detach_to_job(&call, &run, process, &job_id, timeout, true).await;
                }
            }
        }
    }
}

fn start_explicit_job(call: &BashCall, timeout: Duration) -> ToolOutcome {
    let run = ProcessRun {
        capture: Arc::new(Mutex::new(BashCapture::new(&call.data_dir))),
        live: LiveSink::detached(),
        cancel: CancellationToken::new(),
    };
    match register_process(call, &run) {
        Ok(job_id) => {
            let process = spawn_process(call, timeout, &run);
            supervise_job(&call.jobs, &call.conversation_id, &job_id, process);
            started_outcome(&job_id, timeout, false, String::new())
        }
        Err(error) => ToolOutcome::failure(error.to_string()),
    }
}

async fn detach_to_job(
    call: &BashCall,
    run: &ProcessRun,
    process: JoinHandle<ToolOutcome>,
    job_id: &str,
    timeout: Duration,
    steered: bool,
) -> ToolOutcome {
    run.live.detach().await;
    let latest_output = run.capture.lock().await.content();
    supervise_job(&call.jobs, &call.conversation_id, job_id, process);
    started_outcome(job_id, timeout, steered, latest_output)
}

fn register_process(call: &BashCall, run: &ProcessRun) -> Result<String, JobLimitReached> {
    call.jobs.register(
        &call.conversation_id,
        &call.args.command,
        run.cancel.clone(),
        run.capture.clone(),
    )
}

fn spawn_process(call: &BashCall, timeout: Duration, run: &ProcessRun) -> JoinHandle<ToolOutcome> {
    tokio::spawn(run_bash_process(BashProcess {
        data_dir: call.data_dir.clone(),
        cwd: call.cwd.clone(),
        script: call.args.command.clone(),
        timeout,
        capture: run.capture.clone(),
        live: run.live.clone(),
        cancel: run.cancel.clone(),
    }))
}

/// Hands the finished process to the registry, which records its outcome once.
fn supervise_job(
    jobs: &Arc<JobRegistry>,
    conversation_id: &str,
    job_id: &str,
    process: JoinHandle<ToolOutcome>,
) {
    let jobs = jobs.clone();
    let conversation_id = conversation_id.to_owned();
    let job_id = job_id.to_owned();
    tokio::spawn(async move {
        let outcome = join_outcome(process.await);
        jobs.settle(&conversation_id, &job_id, outcome);
    });
}

fn join_outcome(joined: Result<ToolOutcome, JoinError>) -> ToolOutcome {
    joined.unwrap_or_else(|error| ToolOutcome::failure(format!("command task failed: {error}")))
}

fn resolve_timeout(seconds: Option<u64>, timeout_ms: u32) -> Duration {
    match seconds {
        Some(seconds) => Duration::from_secs(seconds.clamp(1, MAX_TIMEOUT_SECS)),
        None if timeout_ms == 0 => DEFAULT_TIMEOUT,
        None => Duration::from_millis(u64::from(timeout_ms)),
    }
}

/// The configured threshold, capped one second short of the command's own deadline
/// so a command never auto-backgrounds in the last second before it is killed.
fn auto_background_threshold(configured: Duration, timeout: Duration) -> Option<Duration> {
    let threshold = configured.min(timeout.saturating_sub(Duration::from_secs(1)));
    (!threshold.is_zero()).then_some(threshold)
}

fn started_outcome(
    job_id: &str,
    timeout: Duration,
    steered: bool,
    latest_output: String,
) -> ToolOutcome {
    let mut content = String::new();
    if steered {
        content.push_str(
            "Backgrounded early to handle an incoming message; the command keeps running.\n",
        );
    }
    let _ = write!(
        content,
        "Backgrounded as job {job_id} (killed once it has run {}s in total); its output is injected into the conversation as a follow-up the moment it finishes. Do NOT poll for it (no `sleep`, `ps`, `pgrep`, `top`, `pidwait`, log tailing): every poll is a wasted turn. Do other work, or end your reply and wait to be woken.",
        timeout.as_secs()
    );
    if !latest_output.is_empty() {
        let _ = write!(content, "\n\nOutput so far:\n{latest_output}");
    }
    ToolOutcome {
        is_error: false,
        content,
        details_json: async_details_json(JobState::Running, job_id),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
