//! Run lifecycle: durable submission, supervision and settling for one conversation.
//! The runtime owns the task registry; the journal owns which accepted work
//! remains unfinished across failures, cancellation and process restart.

mod settle;
mod start_intent;
mod steer;

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use futures::FutureExt;
use futures::future::BoxFuture;

use roost_protocol::wire::agent_chat::{AgentRunState, ChatEvent};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::{AgentRuntime, RunHandle, new_abort_flag};
use crate::error::AgentError;
use crate::records::{AdvisorySeverity, Entry};
use crate::run_loop;

/// A message injected into a conversation between tool rounds.
#[derive(Debug, Clone)]
pub(crate) enum Steer {
    User(String),
    Advisory {
        item_id: String,
        severity: AdvisorySeverity,
        note: String,
    },
    /// A settled background job, journaled as the entry that wakes or joins the run.
    BackgroundJob(Entry),
    /// A message from a peer conversation, journaled as the entry it becomes.
    /// `wakes_idle` starts a run when the recipient is idle. A subagent runs only
    /// under its task, so a direct message to one waits in its journal until then.
    PeerMessage {
        entry: Entry,
        wakes_idle: bool,
    },
    Persisted {
        entry: Entry,
        sequence: u64,
    },
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunEnd {
    Completed,
    /// A subagent's `yield` result.
    Yielded(String),
    Aborted,
    Failed(String),
}

impl AgentRuntime {
    /// Starts a run unless one is active. A child run's token derives from
    /// its parent's, so aborting the parent aborts the child.
    ///
    /// The future is type-erased: a run can start runs (subagents, a late
    /// steer), and the erasure is what lets that recursive future be `Send`.
    pub(crate) fn start_run(
        &self,
        id: &str,
        parent: Option<&CancellationToken>,
        expected_intent: Option<u64>,
    ) -> BoxFuture<'static, Result<Option<oneshot::Receiver<RunEnd>>, AgentError>> {
        let runtime = self.clone();
        let id = id.to_owned();
        let parent = parent.cloned();
        let lifecycle = runtime.lifecycle_lock(&id);
        Box::pin(async move {
            let _guard = lifecycle.lock().await;
            runtime
                .start_run_now(&id, parent.as_ref(), None, expected_intent)
                .await
        })
    }

    /// Boxed so the restart in `steer_command` does not embed `start_run_now`'s
    /// future in the steer future: the spawned run loop reaches `steer` again,
    /// and that cycle broke the `Send` proof for the spawn. The caller must
    /// already hold the conversation's lifecycle lock; this does not take it.
    pub(crate) fn start_run_now_boxed(
        &self,
        id: &str,
    ) -> BoxFuture<'static, Result<Option<oneshot::Receiver<RunEnd>>, AgentError>> {
        let runtime = self.clone();
        let id = id.to_owned();
        Box::pin(async move { runtime.start_run_now(&id, None, None, None).await })
    }

    pub(crate) async fn start_run_now(
        &self,
        id: &str,
        parent: Option<&CancellationToken>,
        mut recovered: Option<run_loop::RecoveredRun>,
        expected_intent: Option<u64>,
    ) -> Result<Option<oneshot::Receiver<RunEnd>>, AgentError> {
        if self.is_running(id) {
            return Ok(None);
        }
        if self.claim_lease(id).await?.is_none() {
            return Ok(None);
        }
        let entries = match self.inner.store.entries(id).await {
            Ok(entries) => entries,
            Err(error) => {
                self.release_lease(id).await;
                return Err(error);
            }
        };
        let through_seq = entries.last().map_or(0, |(sequence, _)| *sequence);
        let cancel = parent.map_or_else(CancellationToken::new, CancellationToken::child_token);
        let (steer, steer_rx) = mpsc::unbounded_channel();
        let user_aborted = new_abort_flag();
        let fenced = Arc::new(AtomicBool::new(false));
        let processed_seq = Arc::new(AtomicU64::new(through_seq));
        // Ignore historical Stop notices, but not one admitted while startup awaits storage.
        let start_intent_seq = Arc::new(AtomicU64::new(through_seq));
        let handle = RunHandle {
            cancel: cancel.clone(),
            steer,
            user_aborted: user_aborted.clone(),
            processed_seq,
            abort_seq: Arc::new(AtomicU64::new(0)),
            queued_seq: Arc::new(AtomicU64::new(through_seq)),
            start_intent_seq: Arc::clone(&start_intent_seq),
            fenced: Arc::clone(&fenced),
        };
        if !self.insert_run(id, handle) {
            return Ok(None);
        }
        match self
            .commit_start_intent(id, through_seq, expected_intent)
            .await
        {
            Ok(Some(sequence)) => start_intent_seq.store(sequence, Ordering::SeqCst),
            Ok(None) => {
                let _ = self.take_run(id);
                self.release_lease(id).await;
                return Ok(None);
            }
            Err(error) => {
                let _ = self.take_run(id);
                self.release_lease(id).await;
                return Err(error);
            }
        }
        self.mark_aborted(id, false);
        if let Err(error) = self
            .update_record(id, |record| {
                record.run_state = AgentRunState::Running;
                record.error = None;
            })
            .await
        {
            let _ = self.take_run(id);
            self.release_lease(id).await;
            return Err(error);
        }
        self.emit(
            id,
            vec![ChatEvent::RunState {
                run_state: AgentRunState::Running,
                error: None,
            }],
        )
        .await;
        tracing::info!(conversation_id = id, instance_id = %self.inner.config.instance_id, "agent run started");
        if let Err(error) = self.start_goal_clock(id).await {
            tracing::warn!(conversation_id = id, %error, "goal execution clock could not start");
        }
        let clock_runtime = self.clone();
        let clock_id = id.to_owned();
        let clock_cancel = cancel.child_token();
        let checkpoint_cancel = clock_cancel.clone();
        let renew_every = self.inner.config.lease_renew;
        tokio::spawn(async move {
            let mut checkpoint = tokio::time::interval(std::time::Duration::from_secs(5));
            let mut renewal = tokio::time::interval(renew_every);
            renewal.reset();
            loop {
                tokio::select! {
                    () = checkpoint_cancel.cancelled() => {
                        let _ = clock_runtime.checkpoint_goal_clock(&clock_id).await;
                        break;
                    }
                    _ = renewal.tick() => {
                        if !clock_runtime.is_running(&clock_id) {
                            break;
                        }
                        clock_runtime.renew_lease(&clock_id).await;
                    }
                    _ = checkpoint.tick() => {
                        if !clock_runtime.is_running(&clock_id) {
                            let _ = clock_runtime.checkpoint_goal_clock(&clock_id).await;
                            break;
                        }
                        if let Err(error) = clock_runtime.checkpoint_goal_clock(&clock_id).await {
                            tracing::warn!(conversation_id = %clock_id, %error, "goal execution clock checkpoint failed");
                        }
                    }
                }
            }
        });
        let (done, done_rx) = oneshot::channel();
        let runtime = self.clone();
        let conversation_id = id.to_owned();
        tokio::spawn(async move {
            let mut steer_rx = steer_rx;
            let mut failures = 0_usize;
            let end = loop {
                let attempt = AssertUnwindSafe(async {
                    let end = run_loop::run_conversation(
                        runtime.clone(),
                        conversation_id.clone(),
                        cancel.clone(),
                        recovered.take(),
                        &mut steer_rx,
                    )
                    .await;
                    if matches!(end, RunEnd::Failed(_)) {
                        return (end, false);
                    }
                    let aborted = user_aborted.load(Ordering::SeqCst);
                    match runtime
                        .next_loop_iteration(&conversation_id, &end, aborted, &cancel)
                        .await
                    {
                        Ok(repeat) => (end, repeat),
                        Err(error) => (RunEnd::Failed(error.to_string()), false),
                    }
                })
                .catch_unwind()
                .await;
                let (end, repeat) = match attempt {
                    Ok(result) => result,
                    Err(payload) => {
                        let error = payload
                            .downcast_ref::<String>()
                            .cloned()
                            .or_else(|| payload.downcast_ref::<&str>().map(|value| (*value).into()))
                            .unwrap_or_else(|| "agent run panicked".into());
                        (RunEnd::Failed(error), false)
                    }
                };
                if let RunEnd::Failed(error) = &end {
                    if fenced.load(Ordering::SeqCst) {
                        break RunEnd::Aborted;
                    }
                    runtime
                        .record_attempt_failure(&conversation_id, error)
                        .await;
                    match runtime
                        .reconcile_interrupted_tools_before_retry(
                            &conversation_id,
                            &cancel,
                            &mut failures,
                        )
                        .await
                    {
                        Ok(checkpoint) => recovered = checkpoint,
                        Err(_) => break RunEnd::Aborted,
                    }
                    if cancel.is_cancelled() {
                        break RunEnd::Aborted;
                    }
                    let delay = runtime.retry_delay(failures);
                    failures = failures.saturating_add(1);
                    tokio::select! {
                        () = cancel.cancelled() => break RunEnd::Aborted,
                        () = tokio::time::sleep(delay) => {}
                    }
                    if let Err(state_error) = runtime.mark_retry_running(&conversation_id).await {
                        tracing::warn!(
                            conversation_id,
                            %state_error,
                            "agent retry state was not saved"
                        );
                    }
                    continue;
                }
                if repeat {
                    continue;
                }
                break end;
            };
            clock_cancel.cancel();
            let aborted = user_aborted.load(Ordering::SeqCst);
            if fenced.load(Ordering::SeqCst) {
                runtime.abandon_fenced_run(&conversation_id).await;
                let _ = done.send(RunEnd::Aborted);
                return;
            }
            runtime
                .settle_run(&conversation_id, &end, aborted, steer_rx)
                .await;
            let _ = done.send(end);
        });
        Ok(Some(done_rx))
    }
}
