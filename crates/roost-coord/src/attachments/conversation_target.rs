//! Resolves agent conversation identity before its worker receives an attachment.
//! The conversation runtime owns the authoritative worker and workspace metadata.

use connectrpc::{ConnectError, ErrorCode};
use roost_agent::AgentError;
use roost_protocol::wire::{AttachmentTargetId, WorkerFp};

use crate::coord_core::CoordCore;

pub(super) struct ConversationAttachmentTarget {
    pub worker_fp: WorkerFp,
}

pub(super) async fn conversation_attachment_target(
    core: &CoordCore,
    target_id: &AttachmentTargetId,
    expected_worker_fp: Option<&str>,
) -> Result<Option<ConversationAttachmentTarget>, ConnectError> {
    if !target_id.is_conversation() {
        return Ok(None);
    }
    let conversation_id = target_id.as_str();
    let summary = match core
        .services
        .agent
        .runtime
        .conversation(conversation_id)
        .await
    {
        Ok(summary) => summary,
        Err(AgentError::NotFound(_)) => return Ok(None),
        Err(error) => {
            tracing::error!(conversation_id, %error, "attachments: agent conversation lookup failed");
            return Err(ConnectError::new(
                ErrorCode::Internal,
                "attachment conversation lookup failed",
            ));
        }
    };
    if summary.id != conversation_id || summary.cwd.trim().is_empty() {
        tracing::error!(
            conversation_id,
            "attachments: conversation metadata is incomplete"
        );
        return Err(ConnectError::new(
            ErrorCode::Internal,
            "attachment conversation metadata is invalid",
        ));
    }
    if expected_worker_fp.is_some_and(|expected| expected != summary.worker_fp.as_str()) {
        return Err(ConnectError::new(
            ErrorCode::PermissionDenied,
            "attachment worker does not own the conversation",
        ));
    }
    let worker_fp = crate::attachments::relay::require_worker_hub_socket(
        &core.services.db,
        &core.services.scrollback,
        &summary.worker_fp,
    )
    .await?;
    Ok(Some(ConversationAttachmentTarget { worker_fp }))
}
