//! Agent settings reads and writes: the saved document, resolved against a
//! conversation's folder, and the validated save path. Split out of `api.rs`
//! to keep it under the file-size cap; same `AgentRuntime` inherent impl.

use super::AgentRuntime;
use crate::error::AgentError;
use crate::records::{AgentSettings, ConversationRecord};
use crate::{folder_roles, roles};

impl AgentRuntime {
    pub async fn settings(&self) -> Result<AgentSettings, AgentError> {
        self.inner.store.settings().await
    }

    /// The settings a conversation resolves against: the saved document with
    /// the matching folder's role overrides applied.
    pub async fn settings_for(
        &self,
        record: &ConversationRecord,
    ) -> Result<AgentSettings, AgentError> {
        Ok(self
            .settings()
            .await?
            .for_folder(&record.worker_fp, &record.cwd))
    }

    /// Saves settings after checking every role selector's syntax, folder rows
    /// included.
    pub async fn set_settings(&self, mut settings: AgentSettings) -> Result<(), AgentError> {
        for (role, selector) in &settings.model_roles {
            roles::validate_role_selector(*role, selector).map_err(|error| {
                AgentError::InvalidArgument(format!("role {}: {error}", role.as_str()))
            })?;
        }
        crate::model_presets::validate_presets(&settings.model_presets)?;
        settings.folder_model_roles =
            folder_roles::prepare_folder_rows(&settings.folder_model_roles)?;
        self.inner.store.save_settings(&settings).await?;
        tracing::info!(
            roles = settings.model_roles.len(),
            folder_overrides = settings.folder_model_roles.len(),
            advisor_enabled = settings.advisor_enabled,
            "agent settings saved"
        );
        for record in self.inner.store.conversations().await? {
            self.publish_summary(&record).await;
        }
        Ok(())
    }
}
