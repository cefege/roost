//! Run lifecycle: durable submission, supervision and settling for one conversation.
//! The runtime owns the task registry; the journal owns which accepted work
//! remains unfinished across failures, cancellation and process restart.

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::FutureExt;
use futures::future::BoxFuture;

use super::recovery::latest_submission_seq;
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
    Persisted {
        entry: Entry,
        sequence: u64,
    },
}

impl Steer {
    pub(crate) fn into_entry(self) -> Entry {
        match self {
            Self::User(text) => Entry::User {
                text,
                loop_repeat: false,
            },
            Self::Advisory {
                item_id,
                severity,
                note,
            } => Entry::Advisory {
                item_id,
                severity,
                note,
                delivered: true,
            },
            Self::Persisted { entry, .. } => entry,
        }
    }
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
    ) -> BoxFuture<'static, Result<Option<oneshot::Receiver<RunEnd>>, AgentError>> {
        let runtime = self.clone();
        let id = id.to_owned();
        let parent = parent.cloned();
        let lifecycle = runtime.lifecycle_lock(&id);
        Box::pin(async move {
            let _guard = lifecycle.lock().await;
            runtime.start_run_now(&id, parent.as_ref(), None).await
        })
    }

    pub(crate) async fn start_run_now(
        &self,
        id: &str,
        parent: Option<&CancellationToken>,
        mut recovered: Option<run_loop::RecoveredRun>,
    ) -> Result<Option<oneshot::Receiver<RunEnd>>, AgentError> {
        let entries = self.inner.store.entries(id).await?;
        let through_seq = entries.last().map_or(0, |(sequence, _)| *sequence);
        let cancel = parent.map_or_else(CancellationToken::new, CancellationToken::child_token);
        let (steer, steer_rx) = mpsc::unbounded_channel();
        let user_aborted = new_abort_flag();
        let processed_seq = std::sync::Arc::new(AtomicU64::new(through_seq));
        let handle = RunHandle {
            cancel: cancel.clone(),
            steer,
            user_aborted: user_aborted.clone(),
            processed_seq,
            abort_seq: std::sync::Arc::new(AtomicU64::new(0)),
        };
        if !self.insert_run(id, handle) {
            return Ok(None);
        }
        if let Err(error) = self
            .append(
                id,
                Entry::RunIntent {
                    active: true,
                    through_seq,
                    aborted: false,
                },
            )
            .await
        {
            let _ = self.take_run(id);
            return Err(error);
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
        tracing::info!(conversation_id = id, "agent run started");
        if let Err(error) = self.start_goal_clock(id).await {
            tracing::warn!(conversation_id = id, %error, "goal execution clock could not start");
        }
        let clock_runtime = self.clone();
        let clock_id = id.to_owned();
        let clock_cancel = cancel.child_token();
        let checkpoint_cancel = clock_cancel.clone();
        tokio::spawn(async move {
            let mut checkpoint = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                tokio::select! {
                    () = checkpoint_cancel.cancelled() => {
                        let _ = clock_runtime.checkpoint_goal_clock(&clock_id).await;
                        break;
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
            runtime
                .settle_run(&conversation_id, &end, aborted, steer_rx)
                .await;
            let _ = done.send(end);
        });
        Ok(Some(done_rx))
    }

    async fn settle_run(
        &self,
        id: &str,
        end: &RunEnd,
        user_aborted: bool,
        mut steer_rx: mpsc::UnboundedReceiver<Steer>,
    ) {
        let lifecycle = self.lifecycle_lock(id);
        let _guard = lifecycle.lock().await;
        if let Err(error) = self.checkpoint_goal_clock(id).await {
            tracing::warn!(conversation_id = id, %error, "goal execution clock did not settle");
        }
        self.stop_goal_clock(id);
        let handle = self.take_run(id);
        let processed_seq = handle
            .as_ref()
            .map_or(0, |run| run.processed_seq.load(Ordering::SeqCst));
        let mut queued = 0_usize;
        while steer_rx.try_recv().is_ok() {
            queued = queued.saturating_add(1);
        }
        let (entries, entries_read_failed) = match self.inner.store.entries(id).await {
            Ok(entries) => (entries, false),
            Err(error) => {
                tracing::warn!(conversation_id = id, %error, "agent run journal could not be read while settling");
                (Vec::new(), true)
            }
        };
        let latest_active = super::recovery::latest_active_intent_seq(&entries).unwrap_or(0);
        let superseded_abort = user_aborted
            && handle
                .as_ref()
                .is_some_and(|run| latest_active > run.abort_seq.load(Ordering::SeqCst));
        let aborted = (user_aborted || matches!(end, RunEnd::Aborted)) && !superseded_abort;
        self.mark_aborted(id, aborted);
        let last_seq = entries.last().map_or(0, |(sequence, _)| *sequence);
        let through_seq = if aborted { last_seq } else { processed_seq };
        let pending =
            latest_submission_seq(&entries).is_some_and(|sequence| sequence > processed_seq);
        let intent_error = self
            .append(
                id,
                Entry::RunIntent {
                    active: false,
                    through_seq,
                    aborted,
                },
            )
            .await
            .err();
        let (run_state, error) = if let Some(error) = &intent_error {
            (AgentRunState::Failed, Some(error.to_string()))
        } else {
            match end {
                RunEnd::Failed(error) => (AgentRunState::Failed, Some(error.clone())),
                RunEnd::Completed | RunEnd::Yielded(_) | RunEnd::Aborted => {
                    (AgentRunState::Idle, None)
                }
            }
        };
        tracing::info!(conversation_id = id, ?end, "agent run ended");
        if let Err(store_error) = self
            .update_record(id, |record| {
                record.run_state = run_state;
                record.error = error.clone();
            })
            .await
        {
            tracing::warn!(conversation_id = id, %store_error, "agent run state was not saved");
        }
        if let Some(error) = &intent_error {
            tracing::warn!(conversation_id = id, %error, "agent terminal intent was not saved");
        }
        self.emit(id, vec![ChatEvent::RunState { run_state, error }])
            .await;
        let restart = !aborted && (pending || queued > 0);
        drop(_guard);
        if intent_error.is_some() || (entries_read_failed && !aborted) {
            self.schedule_readmission(id);
        } else if restart && let Err(error) = self.start_run(id, None).await {
            tracing::warn!(conversation_id = id, %error, "accepted late steer did not start another run");
            self.schedule_readmission(id);
        }
    }
}
