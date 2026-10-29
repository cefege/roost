//! Durable readmission retries accepted work after a store outage.
//! `runs` schedules retries; `recovery` rechecks the journal before admitting.
//! Re-reading durable intent prevents a delayed retry from undoing Stop.

use super::AgentRuntime;

impl AgentRuntime {
    pub(crate) fn schedule_readmission(&self, id: &str) {
        let runtime = self.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            runtime.readmit_until_admitted(&id).await;
        });
    }

    async fn readmit_until_admitted(&self, id: &str) {
        let mut failures = 0_usize;
        loop {
            match self.recover_run(id, None).await {
                Ok(_) => return,
                Err(crate::error::AgentError::NotFound(_)) => {
                    tracing::info!(
                        conversation_id = id,
                        "run readmission ended because the conversation no longer exists"
                    );
                    return;
                }
                Err(error) => {
                    tracing::warn!(
                        conversation_id = id,
                        %error,
                        "accepted unfinished work could not be readmitted; retrying"
                    );
                    let delay = self.retry_delay(failures);
                    failures = failures.saturating_add(1);
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
}
