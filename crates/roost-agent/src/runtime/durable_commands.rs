//! Leased native commands that drive Durable without starting a generation.
//! A lifecycle guard excludes a second local engine; shared SQL owner fences
//! exclude other replicas while the heartbeat keeps long summaries admitted.

use super::AgentRuntime;
use crate::durable::DurableDriver;
use crate::error::AgentError;

impl AgentRuntime {
    pub(crate) async fn compact_durable(
        &self,
        id: &str,
        instructions: Option<String>,
    ) -> Result<bool, AgentError> {
        let lifecycle = self.lifecycle_lock(id);
        let _guard = lifecycle.lock().await;
        if self.is_running(id) {
            return Err(AgentError::FailedPrecondition(
                "wait for the current run to finish before compacting".into(),
            ));
        }
        if self.claim_lease(id).await?.is_none() {
            return Err(AgentError::FailedPrecondition(
                "Conversation is owned by another coordinator".into(),
            ));
        }
        let result = async {
            let driver = DurableDriver::open(self.clone(), id.to_owned()).await?;
            let compact = driver.compact(instructions);
            tokio::pin!(compact);
            let mut renewal = tokio::time::interval(self.inner.config.lease_renew);
            renewal.reset();
            let compacted = loop {
                tokio::select! {
                    result = &mut compact => break result,
                    _ = renewal.tick() => self.renew_lease(id).await,
                }
            };
            let closed = driver.close(false).await;
            match (compacted, closed) {
                (Err(error), _) | (_, Err(error)) => Err(error),
                (Ok(compacted), Ok(())) => Ok(compacted),
            }
        }
        .await;
        self.release_lease(id).await;
        self.inner.signal.conversation_changed(id).await;
        result
    }
}
