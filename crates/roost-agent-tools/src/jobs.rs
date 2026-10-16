//! Per-conversation registry of background bash jobs. The tool host owns one:
//! a detached or `async` bash call registers here, the `job` tool reads and
//! cancels through it, and each job that reaches a terminal state is sent once
//! on the settled channel for delivery to the coordinator.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use roost_protocol::wire::agent_chat::{JobArgs, JobOp, JobState};
use thiserror::Error;
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::{bash_capture::BashCapture, outcome::ToolOutcome};

pub const MAX_RUNNING_JOBS: usize = 15;
const SETTLED_RETENTION: usize = 100;
const IDLE_TTL: Duration = Duration::from_secs(60 * 60);

/// A job that reached a terminal state, ready to report to the coordinator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettledJob {
    pub conversation_id: String,
    pub job_id: String,
    pub command: String,
    pub state: JobState,
    pub is_error: bool,
    pub output: String,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error(
    "Background job limit reached ({max}). Wait for running jobs to finish or cancel one.",
    max = MAX_RUNNING_JOBS
)]
pub struct JobLimitReached;

/// Background jobs of every conversation on this worker. The state sits behind a
/// std mutex so the synchronous link-drop path can cancel jobs without awaiting.
#[derive(Debug)]
pub struct JobRegistry {
    conversations: Mutex<HashMap<String, ConversationJobs>>,
    settled_jobs: mpsc::UnboundedSender<SettledJob>,
}

#[derive(Debug)]
struct ConversationJobs {
    next_number: u64,
    jobs: BTreeMap<u64, JobRecord>,
    last_touched: Instant,
}

#[derive(Debug)]
struct JobRecord {
    command: String,
    state: JobState,
    cancel: CancellationToken,
    capture: Arc<AsyncMutex<BashCapture>>,
    is_error: bool,
    output: String,
    exit_code: Option<i32>,
}

struct JobView {
    state: JobState,
    is_error: bool,
    output: String,
    capture: Arc<AsyncMutex<BashCapture>>,
}

impl ConversationJobs {
    fn new() -> Self {
        Self {
            next_number: 1,
            jobs: BTreeMap::new(),
            last_touched: Instant::now(),
        }
    }

    fn running_count(&self) -> usize {
        self.jobs
            .values()
            .filter(|job| job.state == JobState::Running)
            .count()
    }
}

impl JobRegistry {
    pub fn new(settled_jobs: mpsc::UnboundedSender<SettledJob>) -> Self {
        Self {
            conversations: Mutex::new(HashMap::new()),
            settled_jobs,
        }
    }

    /// Records a new running job and returns its `bg_N` id.
    pub fn register(
        &self,
        conversation_id: &str,
        command: &str,
        cancel: CancellationToken,
        capture: Arc<AsyncMutex<BashCapture>>,
    ) -> Result<String, JobLimitReached> {
        let mut conversations = self.lock();
        let conversation = conversations
            .entry(conversation_id.to_owned())
            .or_insert_with(ConversationJobs::new);
        conversation.last_touched = Instant::now();
        if conversation.running_count() >= MAX_RUNNING_JOBS {
            return Err(JobLimitReached);
        }
        let number = conversation.next_number;
        conversation.next_number += 1;
        conversation.jobs.insert(
            number,
            JobRecord {
                command: command.to_owned(),
                state: JobState::Running,
                cancel,
                capture,
                is_error: false,
                output: String::new(),
                exit_code: None,
            },
        );
        let job_id = job_id_for(number);
        tracing::info!(conversation_id, job_id = %job_id, "background job started");
        Ok(job_id)
    }

    /// Records the outcome of a running job and reports it once. A job whose
    /// conversation was closed is dropped silently: nothing is waiting for it.
    pub fn settle(&self, conversation_id: &str, job_id: &str, outcome: ToolOutcome) {
        let Some(number) = parse_job_number(job_id) else {
            return;
        };
        let mut conversations = self.lock();
        let Some(conversation) = conversations.get_mut(conversation_id) else {
            return;
        };
        let Some(job) = conversation.jobs.get_mut(&number) else {
            return;
        };
        if job.state != JobState::Running {
            return;
        }
        let state = if job.cancel.is_cancelled() {
            JobState::Cancelled
        } else if outcome.is_error {
            JobState::Failed
        } else {
            JobState::Completed
        };
        let exit_code = exit_code_of(&outcome.details_json);
        job.state = state;
        job.is_error = outcome.is_error;
        job.output = outcome.content.clone();
        job.exit_code = exit_code;
        let settled = SettledJob {
            conversation_id: conversation_id.to_owned(),
            job_id: job_id.to_owned(),
            command: job.command.clone(),
            state,
            is_error: outcome.is_error,
            output: outcome.content,
            exit_code,
        };
        prune_settled(conversation);
        drop(conversations);
        tracing::info!(
            conversation_id,
            job_id,
            state = state.as_str(),
            "background job settled"
        );
        let _ = self.settled_jobs.send(settled);
    }

    /// Cancels the running jobs of one conversation; each settles as cancelled.
    pub fn cancel_running(&self, conversation_id: &str) {
        let conversations = self.lock();
        if let Some(conversation) = conversations.get(conversation_id) {
            cancel_running_jobs(conversation);
        }
    }

    pub fn cancel_all(&self) {
        let conversations = self.lock();
        for conversation in conversations.values() {
            cancel_running_jobs(conversation);
        }
    }

    pub fn close_conversation(&self, conversation_id: &str) {
        let removed = self.lock().remove(conversation_id);
        if let Some(conversation) = removed {
            cancel_running_jobs(&conversation);
        }
    }

    /// Drops conversations with nothing running and no activity for the idle TTL.
    pub fn evict_idle(&self) {
        self.lock().retain(|_, conversation| {
            conversation.running_count() > 0 || conversation.last_touched.elapsed() < IDLE_TTL
        });
    }

    pub async fn run_job_tool(&self, conversation_id: &str, args: JobArgs) -> ToolOutcome {
        match args.op {
            JobOp::List => self.list_jobs(conversation_id),
            JobOp::Output => match args.id {
                Some(job_id) => self.job_output(conversation_id, &job_id).await,
                None => ToolOutcome::failure("job output requires an id"),
            },
            JobOp::Cancel => match args.id {
                Some(job_id) => self.cancel_job(conversation_id, &job_id),
                None => ToolOutcome::failure("job cancel requires an id"),
            },
        }
    }

    fn list_jobs(&self, conversation_id: &str) -> ToolOutcome {
        let conversations = self.lock();
        let lines: Vec<String> = conversations
            .get(conversation_id)
            .map(|conversation| {
                conversation
                    .jobs
                    .iter()
                    .map(|(number, job)| {
                        format!(
                            "{} {} {}",
                            job_id_for(*number),
                            job.state.as_str(),
                            job.command
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        if lines.is_empty() {
            ToolOutcome::success("No background jobs.")
        } else {
            ToolOutcome::success(lines.join("\n"))
        }
    }

    async fn job_output(&self, conversation_id: &str, job_id: &str) -> ToolOutcome {
        let Some(job) = self.job_view(conversation_id, job_id) else {
            return not_found(job_id);
        };
        if job.state != JobState::Running {
            return if job.is_error {
                ToolOutcome::failure(job.output)
            } else {
                ToolOutcome::success(job.output)
            };
        }
        let tail = job.capture.lock().await.content();
        let shown = if tail.is_empty() {
            "(no output yet)".to_owned()
        } else {
            tail
        };
        ToolOutcome::success(format!(
            "Background job {job_id} is still running.\n{shown}"
        ))
    }

    fn cancel_job(&self, conversation_id: &str, job_id: &str) -> ToolOutcome {
        let conversations = self.lock();
        let Some(job) = parse_job_number(job_id).and_then(|number| {
            conversations
                .get(conversation_id)
                .and_then(|conversation| conversation.jobs.get(&number))
        }) else {
            return not_found(job_id);
        };
        if job.state != JobState::Running {
            return ToolOutcome::success(format!(
                "Background job {job_id} is already {}.",
                job.state.as_str()
            ));
        }
        job.cancel.cancel();
        ToolOutcome::success(format!("Cancelled background job {job_id}."))
    }

    fn job_view(&self, conversation_id: &str, job_id: &str) -> Option<JobView> {
        let number = parse_job_number(job_id)?;
        let conversations = self.lock();
        let job = conversations.get(conversation_id)?.jobs.get(&number)?;
        Some(JobView {
            state: job.state,
            is_error: job.is_error,
            output: job.output.clone(),
            capture: job.capture.clone(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, ConversationJobs>> {
        self.conversations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

fn cancel_running_jobs(conversation: &ConversationJobs) {
    for job in conversation
        .jobs
        .values()
        .filter(|job| job.state == JobState::Running)
    {
        job.cancel.cancel();
    }
}

/// Keeps the newest `SETTLED_RETENTION` settled jobs; running jobs are never pruned.
fn prune_settled(conversation: &mut ConversationJobs) {
    let settled: Vec<u64> = conversation
        .jobs
        .iter()
        .filter(|(_, job)| job.state != JobState::Running)
        .map(|(number, _)| *number)
        .collect();
    let excess = settled.len().saturating_sub(SETTLED_RETENTION);
    for number in settled.into_iter().take(excess) {
        conversation.jobs.remove(&number);
    }
}

fn job_id_for(number: u64) -> String {
    format!("bg_{number}")
}

fn parse_job_number(job_id: &str) -> Option<u64> {
    job_id.strip_prefix("bg_")?.parse().ok()
}

fn not_found(job_id: &str) -> ToolOutcome {
    ToolOutcome::failure(format!("Background job or service not found: {job_id}"))
}

fn exit_code_of(details_json: &str) -> Option<i32> {
    let details: serde_json::Value = serde_json::from_str(details_json).ok()?;
    i32::try_from(details.get("exit_code")?.as_i64()?).ok()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
