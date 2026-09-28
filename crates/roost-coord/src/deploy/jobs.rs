//! The in-memory deploy job registry: one record per `roost deploy` run, its
//! buffered output and bus, and its expiry twenty minutes after it finishes.
//! Called by `deploy::job_process` (which feeds a job), `deploy::output_stream`
//! (which reads one), `deploy::catchup` (which hosts are busy) and
//! `deploy::rpc_deploy`. Ports the registry half of
//! apps/coord/src/deploy/deploy-jobs.ts.
//!
//! A LINE IS BUFFERED AND PUBLISHED UNDER ONE LOCK, and a reader snapshots the
//! buffer and subscribes under the same lock. v2 got that for free from one
//! thread; here it is what keeps a line from reaching a new reader twice, or
//! falling between its snapshot and its subscription.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use roost_observability::{LogFields, SignalKind};
use serde_json::Value;

use crate::coord_core::ids::{draw, render_v4};
use crate::deploy::output_stream::{BusSubscriberQueue, DeployOutput, deploy_output_bounds};
use crate::events::bus::BoundedBus;

/// How long a finished job's output stays readable.
pub const DEPLOY_JOB_TTL_MS: u64 = 20 * 60 * 1000;

/// The diagnostic ring each job's bus keeps.
const DEPLOY_JOB_BUS_CAPACITY: usize = 2048;

/// The prefix a detached `roost deploy` child writes to lift a line into a
/// durable signal: its stderr reaches only this job, so the sentinel is the
/// only way its anomaly reaches `roost doctor`.
const SIGNAL_SENTINEL: &str = "ROOST_SIGNAL ";

/// One message of a job's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployStreamMsg {
    /// One line the subprocess wrote, `\r` stripped.
    Line(String),
    /// The job ended: its exit code when it had one, and why it failed.
    Done {
        exit: Option<i32>,
        error: Option<String>,
    },
}

/// Whether a job is still running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployJobStatus {
    Running,
    Done,
}

/// Whether `job_id` has the shape this coordinator mints: a lowercase
/// 8-4-4-4-12 hex id. Anything else is refused before any lookup.
#[must_use]
pub fn is_deploy_job_id(job_id: &str) -> bool {
    let groups: Vec<&str> = job_id.split('-').collect();
    groups.len() == 5
        && groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, width)| {
            group.len() == width
                && group
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

/// One `roost deploy` run.
pub struct DeployJob {
    job_id: String,
    host: String,
    state: Mutex<DeployJobState>,
    bus: BoundedBus<DeployStreamMsg>,
}

struct DeployJobState {
    lines: Vec<String>,
    status: DeployJobStatus,
    exit_code: Option<i32>,
    error: Option<String>,
}

impl std::fmt::Debug for DeployJob {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeployJob")
            .field("job_id", &self.job_id)
            .field("host", &self.host)
            .field("status", &self.status())
            .finish()
    }
}

impl DeployJob {
    /// The id a reader opens this job's output by.
    #[must_use]
    pub fn job_id(&self) -> &str {
        &self.job_id
    }

    /// The host this job deploys.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Whether the job is still running.
    #[must_use]
    pub fn status(&self) -> DeployJobStatus {
        self.lock().status
    }

    fn lock(&self) -> MutexGuard<'_, DeployJobState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Record one line the subprocess wrote, or lift a `ROOST_SIGNAL` sentinel
    /// of a known kind into its signal instead. An unknown sentinel is an
    /// ordinary line: a typo in the child must not mint a phantom signal.
    pub fn emit_line(&self, text: &str) {
        let trimmed = text.strip_suffix('\r').unwrap_or(text);
        if let Some(rest) = trimmed.strip_prefix(SIGNAL_SENTINEL)
            && self.lift_signal(rest.trim_start())
        {
            return;
        }
        let mut state = self.lock();
        state.lines.push(trimmed.to_owned());
        self.bus.publish(DeployStreamMsg::Line(trimmed.to_owned()));
    }

    /// Emit the sentinel's signal when its kind is one the bridge forwards.
    fn lift_signal(&self, sentinel: &str) -> bool {
        let (kind, detail) = match sentinel.split_once(' ') {
            Some((kind, detail)) => (kind, Some(detail)),
            None => (sentinel, None),
        };
        // The one kind the child forwards; `deploy.failed` fires from the exit
        // code instead, never through the bridge.
        if kind != SignalKind::DeployCertSkipped.as_str() {
            return false;
        }
        let mut fields = LogFields::new().set("host", &self.host);
        // A malformed detail still forwards the kind, with no detail.
        if let Some(Ok(Value::Object(detail))) = detail.map(serde_json::from_str::<Value>) {
            for (key, value) in detail {
                fields.put(&key, value);
            }
        }
        fields.put("cooldownKey", &self.host);
        roost_observability::signal::emit(SignalKind::DeployCertSkipped, fields);
        true
    }

    /// Snapshot the buffered output and, while the job runs, subscribe to the
    /// rest under the same lock a line is published under.
    #[must_use]
    pub(crate) fn open_output(&self) -> DeployOutput {
        let state = self.lock();
        let mut buffered: VecDeque<DeployStreamMsg> = state
            .lines
            .iter()
            .cloned()
            .map(DeployStreamMsg::Line)
            .collect();
        match state.status {
            DeployJobStatus::Done => {
                buffered.push_back(DeployStreamMsg::Done {
                    exit: state.exit_code,
                    error: state.error.clone(),
                });
                DeployOutput::settled(buffered)
            }
            DeployJobStatus::Running => DeployOutput::following(
                buffered,
                BusSubscriberQueue::subscribe(&self.bus, deploy_output_bounds()),
            ),
        }
    }
}

/// Every deploy job this coordinator started and has not yet expired.
#[derive(Debug, Default)]
pub struct DeployJournal {
    jobs: Mutex<HashMap<String, Arc<DeployJob>>>,
}

impl DeployJournal {
    /// A journal with no jobs.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Arc<DeployJob>>> {
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Open a running job for `host` under a fresh random id.
    pub fn open_job(&self, host: &str) -> std::io::Result<Arc<DeployJob>> {
        let job = Arc::new(DeployJob {
            job_id: render_v4(draw::<16>()?),
            host: host.to_owned(),
            state: Mutex::new(DeployJobState {
                lines: Vec::new(),
                status: DeployJobStatus::Running,
                exit_code: None,
                error: None,
            }),
            bus: BoundedBus::new(DEPLOY_JOB_BUS_CAPACITY),
        });
        self.lock().insert(job.job_id.clone(), Arc::clone(&job));
        tracing::info!(job_id = %job.job_id, host, "deploy job opened");
        Ok(job)
    }

    /// The job with this id, while it is held.
    #[must_use]
    pub fn job(&self, job_id: &str) -> Option<Arc<DeployJob>> {
        self.lock().get(job_id).cloned()
    }

    /// Every host a job is running for right now.
    #[must_use]
    pub fn running_hosts(&self) -> BTreeSet<String> {
        self.lock()
            .values()
            .filter(|job| job.status() == DeployJobStatus::Running)
            .map(|job| job.host.clone())
            .collect()
    }

    /// End a job: record and publish its outcome, signal a failure, and expire
    /// its record once the output has been readable for the TTL.
    pub fn finish_job(
        self: &Arc<Self>,
        job: &Arc<DeployJob>,
        exit: Option<i32>,
        error: Option<String>,
    ) {
        {
            let mut state = job.lock();
            state.status = DeployJobStatus::Done;
            state.exit_code = exit;
            state.error.clone_from(&error);
            job.bus.publish(DeployStreamMsg::Done {
                exit,
                error: error.clone(),
            });
        }
        match &error {
            Some(reason) => {
                tracing::warn!(job_id = %job.job_id, host = %job.host, ?exit, reason,
                    "deploy job failed");
                roost_observability::signal::emit(
                    SignalKind::DeployFailed,
                    LogFields::new()
                        .set("host", &job.host)
                        .set("exit", exit)
                        .set("reason", reason)
                        .set("cooldownKey", &job.host),
                );
            }
            None => tracing::info!(job_id = %job.job_id, host = %job.host, ?exit,
                "deploy job finished"),
        }
        self.schedule_expiry(job);
    }

    /// Drop the record `DEPLOY_JOB_TTL_MS` after the job finished, unless the
    /// id has since been reused by a different record.
    fn schedule_expiry(self: &Arc<Self>, job: &Arc<DeployJob>) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(job_id = %job.job_id,
                "deploy job: no runtime to expire the record on; it stays readable");
            return;
        };
        let journal: Weak<Self> = Arc::downgrade(self);
        let job = Arc::clone(job);
        runtime.spawn(async move {
            tokio::time::sleep(Duration::from_millis(DEPLOY_JOB_TTL_MS)).await;
            let Some(journal) = journal.upgrade() else {
                return;
            };
            let mut jobs = journal.lock();
            if jobs
                .get(&job.job_id)
                .is_some_and(|held| Arc::ptr_eq(held, &job))
            {
                jobs.remove(&job.job_id);
                tracing::info!(job_id = %job.job_id, "deploy job expired");
            }
        });
    }
}
