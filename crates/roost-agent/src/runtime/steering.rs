//! Admission and delivery of accepted user or advisor messages.
//! The journal commits input and run intent before the live queue sees them.
//! Runtime API callers use this path whether a run is active or idle.

use super::{AgentRuntime, Steer};
use crate::error::AgentError;
use crate::records::Entry;

impl AgentRuntime {
    /// Delivers a message: into the running loop after its current tool
    /// round, or as a new run when the conversation is idle.
    pub(crate) async fn steer(&self, id: &str, message: Steer) -> Result<(), AgentError> {
        let lifecycle = self.lifecycle_lock(id);
        let _guard = lifecycle.lock().await;
        let accepted_submission = matches!(&message, Steer::User(_) | Steer::Advisory { .. });
        let user_submission = matches!(&message, Steer::User(_));
        if let Steer::User(text) = &message {
            self.prepare_loop_prompt_capture(id, text).await?;
        }
        let entry = message.into_entry();
        let sequence = match self.append(id, entry.clone()).await {
            Ok(sequence) => sequence,
            Err(error) => {
                if user_submission {
                    self.cancel_loop_prompt_capture(id).await;
                }
                return Err(error);
            }
        };
        if user_submission && let Err(error) = self.commit_loop_prompt_capture(id, sequence).await {
            self.schedule_readmission(id);
            return Err(error);
        }
        if let Err(error) = self
            .append(
                id,
                Entry::RunIntent {
                    active: true,
                    through_seq: sequence,
                    aborted: false,
                },
            )
            .await
        {
            if accepted_submission {
                self.schedule_readmission(id);
            }
            return Err(error);
        }
        let committed = Steer::Persisted { entry, sequence };
        let queued = self.with_run(id, |run| {
            !run.cancel.is_cancelled() && run.steer.send(committed).is_ok()
        });
        if queued == Some(true) {
            return Ok(());
        }
        if queued == Some(false) {
            return Ok(());
        }
        match self.start_run_now(id, None, None).await {
            Ok(_) => Ok(()),
            Err(error) => {
                if accepted_submission {
                    self.schedule_readmission(id);
                }
                Err(error)
            }
        }
    }
}
