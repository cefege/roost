//! Resolves agent conversation identity before its worker receives an attachment.
//! Only worker-hosted OMP conversations exist; the native runtime is gone.

use connectrpc::{ConnectError, ErrorCode};
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
    let Some(omp) = crate::agent::omp::store::find(&core.services.db, conversation_id)
        .await
        .map_err(|error| ConnectError::new(ErrorCode::Internal, error.to_string()))?
    else {
        return Ok(None);
    };
    let (resolved_id, cwd, target_worker_fp) = (omp.id, omp.cwd, omp.worker_fp);
    if resolved_id != conversation_id || cwd.trim().is_empty() {
        tracing::error!(
            conversation_id,
            "attachments: conversation metadata is incomplete"
        );
        return Err(ConnectError::new(
            ErrorCode::Internal,
            "attachment conversation metadata is invalid",
        ));
    }
    if expected_worker_fp.is_some_and(|expected| expected != target_worker_fp.as_str()) {
        return Err(ConnectError::new(
            ErrorCode::PermissionDenied,
            "attachment worker does not own the conversation",
        ));
    }
    let worker_fp = crate::attachments::relay::require_worker_hub_socket(
        &core.services.db,
        &core.services.scrollback,
        &target_worker_fp,
    )
    .await?;
    Ok(Some(ConversationAttachmentTarget { worker_fp }))
}
