//! Run lifecycle: starting a conversation's run task, steering messages into
//! it, and settling its row when it ends. A message that arrives as a run is
//! ending is never lost: the settle step drains it and starts the next run.

use std::sync::atomic::Ordering;

use futures::future::BoxFuture;

use roost_protocol::wire::agent_chat::{AgentRunState, ChatEvent};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::{AgentRuntime, RunHandle, new_abort_flag};
use crate::error::AgentError;
use crate::records::{AdvisorySeverity, Entry};
use crate::run_loop;

/// A message injected into a conversation between tool rounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Steer {
    User(String),
    Advisory {
        item_id: String,
        severity: AdvisorySeverity,
        note: String,
    },
}

impl Steer {
    pub(crate) fn into_entry(self) -> Entry {
        match self {
            Steer::User(text) => Entry::User { text },
            Steer::Advisory {
                item_id,
                severity,
                note,
            } => Entry::Advisory {
                item_id,
                severity,
                note,
                delivered: true,
            },
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
        Box::pin(async move { runtime.start_run_now(&id, parent.as_ref()).await })
    }

    async fn start_run_now(
        &self,
        id: &str,
        parent: Option<&CancellationToken>,
    ) -> Result<Option<oneshot::Receiver<RunEnd>>, AgentError> {
        let cancel = parent.map_or_else(CancellationToken::new, CancellationToken::child_token);
        let (steer, steer_rx) = mpsc::unbounded_channel();
        let user_aborted = new_abort_flag();
        let handle = RunHandle {
            cancel: cancel.clone(),
            steer,
            user_aborted: user_aborted.clone(),
        };
        if !self.insert_run(id, handle) {
            return Ok(None);
        }
        self.mark_aborted(id, false);
        if let Err(error) = self
            .update_record(id, |record| {
                record.run_state = AgentRunState::Running;
                record.error = None;
            })
            .await
        {
            self.take_run(id);
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
        let (done, done_rx) = oneshot::channel();
        let runtime = self.clone();
        let conversation_id = id.to_owned();
        tokio::spawn(async move {
            let (end, steer_rx) = run_loop::run_conversation(
                runtime.clone(),
                conversation_id.clone(),
                cancel,
                steer_rx,
            )
            .await;
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
        self.take_run(id);
        self.mark_aborted(id, user_aborted || matches!(end, RunEnd::Aborted));
        let (run_state, error) = match end {
            RunEnd::Failed(error) => (AgentRunState::Failed, Some(error.clone())),
            RunEnd::Completed | RunEnd::Yielded(_) | RunEnd::Aborted => (AgentRunState::Idle, None),
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
        self.emit(id, vec![ChatEvent::RunState { run_state, error }])
            .await;
        let mut late = Vec::new();
        while let Ok(message) = steer_rx.try_recv() {
            late.push(message);
        }
        if late.is_empty() {
            return;
        }
        for message in late {
            if let Err(error) = self.append(id, message.into_entry()).await {
                tracing::warn!(conversation_id = id, %error, "late steer message was not recorded");
                return;
            }
        }
        if let Err(error) = self.start_run(id, None).await {
            tracing::warn!(conversation_id = id, %error, "run for a late steer message did not start");
        }
    }

    /// Delivers a message: into the running loop after its current tool
    /// round, or as a new run when the conversation is idle.
    pub(crate) async fn steer(&self, id: &str, message: Steer) -> Result<(), AgentError> {
        let pending = self.with_run(id, |run| run.steer.send(message.clone()).is_ok());
        if pending == Some(true) {
            return Ok(());
        }
        self.append(id, message.into_entry()).await?;
        self.start_run(id, None).await.map(|_| ())
    }

    /// Cancels the running loop, if any, as a user abort.
    pub fn abort(&self, id: &str) -> bool {
        let aborted = self.user_abort(id);
        if aborted {
            tracing::info!(conversation_id = id, "agent run aborted by user");
        }
        aborted
    }
}
