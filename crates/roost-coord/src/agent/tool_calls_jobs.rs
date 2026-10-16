//! Background bash jobs as the coordinator tracks them: the ledger of running
//! jobs, delivery of settled and lost jobs into their conversation, and the
//! detach and abort frames that steer a worker's jobs. `tool_calls` dispatches
//! to these methods; the worker owns each job's process.

use roost_agent::Entry;
use roost_protocol::versioning::CAPABILITY_AGENT_TOOLS_V1;
use roost_protocol::wire::agent_chat::{BashArgs, JobState, TOOL_BASH, parse_async_details};
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;

use super::job_ledger::{JobKey, RunningJob};
use super::tool_calls::ToolCallRegistry;

/// The command a bash call runs, recorded so a job notice can name it. Other
/// tools have no command.
pub(super) fn bash_command(tool: &str, args_json: &str) -> String {
    if tool != TOOL_BASH {
        return String::new();
    }
    serde_json::from_str::<BashArgs>(args_json)
        .map(|args| args.command)
        .unwrap_or_default()
}

impl ToolCallRegistry {
    /// Gives the registry the runtime that receives settled and lost jobs. Bound
    /// once, after the runtime is built; deliveries before that are logged.
    pub fn bind_runtime(&self, runtime: roost_agent::AgentRuntime) {
        let _ = self.runtime.set(runtime);
    }

    /// Tracks a job when its bash result reports that the command is running in
    /// the background. A settlement that arrived first leaves no entry.
    pub(super) fn note_job_started(
        &self,
        worker_fp: &str,
        worker_label: &str,
        conversation_id: &str,
        command: String,
        details_json: &str,
    ) {
        let Some(details) = parse_async_details(details_json) else {
            return;
        };
        if details.state != JobState::Running {
            return;
        }
        let job = RunningJob {
            key: JobKey {
                worker_fp: worker_fp.to_owned(),
                conversation_id: conversation_id.to_owned(),
                job_id: details.job_id.clone(),
            },
            worker_label: worker_label.to_owned(),
            command,
        };
        let tracked = self
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_started(job);
        if tracked {
            tracing::info!(
                worker_fp,
                worker_label,
                conversation_id,
                job_id = %details.job_id,
                "background job started on worker"
            );
        }
    }

    /// Delivers a job the worker settled into its conversation. A frame with a
    /// state that is not terminal is refused.
    pub(super) fn receive_job_result(
        &self,
        worker_fp: &str,
        result: roost_proto::WAgentJobResult,
    ) -> bool {
        let Some(state) = JobState::parse(&result.state).filter(|state| state.is_settled()) else {
            return false;
        };
        let key = JobKey {
            worker_fp: worker_fp.to_owned(),
            conversation_id: result.conversation_id.clone(),
            job_id: result.job_id.clone(),
        };
        self.jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_settled(&key);
        tracing::info!(
            worker_fp,
            conversation_id = %result.conversation_id,
            job_id = %result.job_id,
            state = state.as_str(),
            "background job settled"
        );
        self.deliver_to_conversation(
            result.conversation_id,
            Entry::BackgroundJob {
                job_id: result.job_id,
                command: result.command,
                state,
                is_error: result.is_error,
                output: result.output,
                exit_code: result.exit_code,
            },
        );
        true
    }

    /// Marks every job a departed worker held as failed. Its processes died
    /// with the link, and the worker does not replay settlements.
    pub(super) fn lose_worker_jobs(&self, worker_fp: &str) {
        let lost = self
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take_worker(worker_fp);
        for job in lost {
            tracing::warn!(
                worker_fp,
                worker_label = %job.worker_label,
                conversation_id = %job.key.conversation_id,
                job_id = %job.key.job_id,
                "background job lost with its worker link"
            );
            let output = format!(
                "Machine {} disconnected before this job settled, so its outcome is unknown. Inspect current state before repeating the command.",
                job.worker_label
            );
            self.deliver_to_conversation(
                job.key.conversation_id,
                Entry::BackgroundJob {
                    job_id: job.key.job_id,
                    command: job.command,
                    state: JobState::Failed,
                    is_error: true,
                    output,
                    exit_code: None,
                },
            );
        }
    }

    /// Asks each worker running a foreground bash call of the conversation to
    /// move that call into a background job.
    pub fn detach_calls(&self, conversation_id: &str) {
        let targets: Vec<(String, String)> = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(_, call)| call.conversation_id == conversation_id)
            .map(|(call_id, call)| (call_id.clone(), call.worker_fp.clone()))
            .collect();
        for (call_id, worker_fp) in targets {
            self.send_to_routable_worker(
                &worker_fp,
                CoordWorkerDownstream::AgentToolBackground(roost_proto::DAgentToolBackground {
                    call_id,
                    ..Default::default()
                }),
            );
        }
    }

    /// Cancels the conversation's running jobs on every worker holding one.
    pub fn abort_jobs(&self, conversation_id: &str) {
        let workers = self
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .workers_running(conversation_id);
        for worker_fp in workers {
            self.send_to_routable_worker(
                &worker_fp,
                CoordWorkerDownstream::AgentJobsAbort(roost_proto::DAgentJobsAbort {
                    conversation_id: conversation_id.to_owned(),
                    ..Default::default()
                }),
            );
        }
    }

    /// The conversation's background jobs that have started and not settled.
    pub fn running_jobs(&self, conversation_id: &str) -> usize {
        self.jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .running_count(conversation_id)
    }

    fn send_to_routable_worker(&self, worker_fp: &str, frame: CoordWorkerDownstream) {
        let Ok(fingerprint) = roost_protocol::wire::WorkerFp::try_from(worker_fp.to_owned()) else {
            return;
        };
        if let Some(worker) = self.workers.current_routable(&fingerprint)
            && worker.capabilities.contains(CAPABILITY_AGENT_TOOLS_V1)
        {
            worker.send(frame);
        }
    }

    fn deliver_to_conversation(&self, conversation_id: String, entry: Entry) {
        let Some(runtime) = self.runtime.get().cloned() else {
            tracing::warn!(
                conversation_id,
                "background job result has no agent runtime to deliver to"
            );
            return;
        };
        tokio::spawn(async move {
            if let Err(error) = runtime
                .deliver_background_job(&conversation_id, entry)
                .await
            {
                tracing::warn!(%error, conversation_id, "background job result could not be delivered");
            }
        });
    }
}
