//! Attachment-aware message admission validates content before persisting an input.
//! Coordinator submits use this path; text-only callers share the same admission policy.
//! Accepted attachments follow the run's existing steering and follow-up boundaries.

use roost_protocol::wire::agent_chat::{parse_slash_command, validate_agent_chat_image_payload};

use super::SubmitMode;
use crate::commands;
use crate::error::AgentError;
use crate::runtime::AgentRuntime;
use crate::traits::PlanAdmission;

impl AgentRuntime {
    /// Submits text and durable attachment metadata under an explicit policy.
    pub async fn submit_request_with_attachments(
        &self,
        id: &str,
        request_id: &str,
        mut text: String,
        attachments: Vec<roost_protocol::wire::agent_chat::AgentChatAttachment>,
        mode: SubmitMode,
    ) -> Result<(), AgentError> {
        validate_agent_chat_image_payload(&text, &attachments)
            .map_err(|error| AgentError::InvalidArgument(error.to_string()))?;
        if request_id.trim().is_empty() {
            return Err(AgentError::InvalidArgument("request_id is required".into()));
        }
        for attachment in &attachments {
            if attachment.mime_type.starts_with("image/") && attachment.data.is_empty() {
                return Err(AgentError::InvalidArgument(format!(
                    "image attachment {} has no data",
                    attachment.filename
                )));
            }
            if !attachment.mime_type.starts_with("image/") && attachment.path.trim().is_empty() {
                return Err(AgentError::InvalidArgument(format!(
                    "attachment {} has no worker path",
                    attachment.filename
                )));
            }
        }
        if text.trim().is_empty() && !attachments.is_empty() {
            text = "Please inspect the attached file.".to_owned();
        }
        let record = self.record(id).await?;
        if attachments.is_empty()
            && let Some((command, args)) = parse_slash_command(&text)
        {
            self.deduplicated_with_plan_admission(
                id,
                request_id,
                "command",
                PlanAdmission::RejectIfPending,
                || async {
                    commands::execute(self, &record, command, args).await?;
                    Ok(String::new())
                },
            )
            .await?;
            return Ok(());
        }
        if text.trim().is_empty() {
            return Err(AgentError::InvalidArgument("message is empty".into()));
        }
        self.deliver_user_content_with_mode(&record, text, attachments, Some(request_id), mode)
            .await
    }
}
