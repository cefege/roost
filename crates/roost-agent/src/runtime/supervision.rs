//! Failure persistence and capped backoff for supervised runs.
//! The runtime exposes each failed attempt before backing off; the persistent
//! journal records failures so an operator can diagnose resumed work.

use std::time::Duration;

use roost_protocol::wire::agent_chat::{AgentRunState, ChatEvent};
use tokio_util::sync::CancellationToken;

use super::AgentRuntime;
use crate::error::AgentError;
use crate::records::Entry;

impl AgentRuntime {
    pub(super) async fn record_attempt_failure(&self, id: &str, failure: &str) {
        tracing::warn!(
            conversation_id = id,
            error = failure,
            "agent run attempt failed; retry scheduled"
        );
        if let Err(error) = self
            .update_record(id, |record| {
                record.run_state = AgentRunState::Running;
                record.error = Some(failure.to_owned());
            })
            .await
        {
            tracing::warn!(conversation_id = id, %error, "agent failure state was not saved");
        }
        if let Err(error) = self
            .append(
                id,
                Entry::Notice {
                    level: "warn".into(),
                    title: "Run attempt failed".into(),
                    body: failure.to_owned(),
                },
            )
            .await
        {
            tracing::warn!(conversation_id = id, %error, "agent failure notice was not recorded");
        }
        self.emit(
            id,
            vec![ChatEvent::RunState {
                run_state: AgentRunState::Running,
                error: Some(failure.to_owned()),
            }],
        )
        .await;
    }

    pub(super) async fn mark_retry_running(&self, id: &str) -> Result<(), AgentError> {
        let record = self
            .update_record(id, |record| {
                record.run_state = AgentRunState::Running;
                record.error = None;
            })
            .await?;
        self.emit(
            id,
            vec![ChatEvent::RunState {
                run_state: AgentRunState::Running,
                error: None,
            }],
        )
        .await;
        tracing::info!(conversation_id = %record.id, "agent retry started after backoff");
        Ok(())
    }

    pub(super) fn retry_delay(&self, failures: usize) -> Duration {
        self.inner
            .config
            .retry_backoff
            .get(failures)
            .copied()
            .or_else(|| self.inner.config.retry_backoff.last().copied())
            .unwrap_or(Duration::from_secs(1))
    }

    pub(super) async fn reconcile_interrupted_tools_before_retry(
        &self,
        id: &str,
        cancel: &CancellationToken,
        failures: &mut usize,
    ) -> Result<Option<crate::run_loop::RecoveredRun>, AgentError> {
        loop {
            let checkpoint: Result<_, AgentError> = async {
                self.reconcile_interrupted_tools(id).await?;
                let entries = self.inner.store.entries(id).await?;
                Ok(super::recovery::recovered_run(self, id, &entries))
            }
            .await;
            match checkpoint {
                Ok(recovered) => return Ok(recovered),
                Err(error) => {
                    tracing::warn!(
                        conversation_id = id,
                        %error,
                        "interrupted run checkpoint could not be reconciled before retry"
                    );
                    let delay = self.retry_delay(*failures);
                    *failures = (*failures).saturating_add(1);
                    tokio::select! {
                        () = cancel.cancelled() => return Err(AgentError::Cancelled),
                        () = tokio::time::sleep(delay) => {}
                    }
                }
            }
        }
    }
}
