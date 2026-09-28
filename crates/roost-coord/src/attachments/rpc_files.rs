//! The eight file and attachment Connect handlers `service_impl.rs` calls:
//! `FilesRead`, `FilesReadChunk`, `FilesListDir`, `FilesMkdir`,
//! `AttachFileChunk`, `AttachmentProbe`, `ListAttachments`, `DeleteAttachment`.
//!
//! Ported from v2 `apps/coord/src/attachments/handlers-attachments.ts`: each
//! checks the caller, resolves the worker, and hands off to `files` or
//! `session_files`, which own the frame and the reply decode.

use connectrpc::{ConnectError, ErrorCode, Response, ServiceResult};
use roost_proto::{
    AttachFileChunkRequest, AttachFileChunkResponse, AttachmentProbeRequest,
    AttachmentProbeResponse, DeleteAttachmentRequest, DeleteAttachmentResponse,
    FilesListDirRequest, FilesListDirResponse, FilesMkdirRequest, FilesMkdirResponse,
    FilesReadChunkRequest, FilesReadChunkResponse, FilesReadRequest, FilesReadResponse,
    ListAttachmentsRequest, ListAttachmentsResponse,
};
use roost_protocol::wire::SessionId;

use super::files::{files_list_dir, files_mkdir, files_read, files_read_chunk};
use super::relay::require_worker_hub_socket;
use super::session_files::{
    attach_file_chunk, attachment_probe, delete_attachment, list_attachments,
};
use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::terminal_screen::scrollback_relay::SessionWorkerBinding;

/// Whole-file read. v2 `filesRead`.
pub async fn handle_files_read(
    core: &CoordCore,
    caller: &Caller,
    req: FilesReadRequest,
) -> ServiceResult<FilesReadResponse> {
    let viewer_id = require_account_device(caller)?;
    let relay = &core.services.scrollback;
    let worker_fp = require_worker_hub_socket(&core.services.db, relay, &req.worker_fp).await?;
    let request_id = relay.pending().next_request_id();
    Response::ok(files_read(relay, &worker_fp, viewer_id, &req.path, &request_id).await?)
}

/// One page of a download. v2 `filesReadChunk`: the worker is resolved before
/// the length is checked, so an offline worker answers first.
pub async fn handle_files_read_chunk(
    core: &CoordCore,
    caller: &Caller,
    req: FilesReadChunkRequest,
) -> ServiceResult<FilesReadChunkResponse> {
    let viewer_id = require_account_device(caller)?;
    let relay = &core.services.scrollback;
    let worker_fp = require_worker_hub_socket(&core.services.db, relay, &req.worker_fp).await?;
    let offset = i64::try_from(req.offset).unwrap_or(i64::MAX);
    Response::ok(
        files_read_chunk(
            relay,
            &worker_fp,
            viewer_id,
            &req.path,
            offset,
            i64::from(req.len),
            &relay.pending().next_request_id(),
        )
        .await?,
    )
}

/// List a directory. v2 `filesListDir`.
pub async fn handle_files_list_dir(
    core: &CoordCore,
    caller: &Caller,
    req: FilesListDirRequest,
) -> ServiceResult<FilesListDirResponse> {
    let viewer_id = require_account_device(caller)?;
    let relay = &core.services.scrollback;
    let worker_fp = require_worker_hub_socket(&core.services.db, relay, &req.worker_fp).await?;
    let request_id = relay.pending().next_request_id();
    Response::ok(files_list_dir(relay, &worker_fp, viewer_id, &req.path, &request_id).await?)
}

/// Create a directory. v2 `filesMkdir`.
pub async fn handle_files_mkdir(
    core: &CoordCore,
    caller: &Caller,
    req: FilesMkdirRequest,
) -> ServiceResult<FilesMkdirResponse> {
    let viewer_id = require_account_device(caller)?;
    let relay = &core.services.scrollback;
    let worker_fp = require_worker_hub_socket(&core.services.db, relay, &req.worker_fp).await?;
    let request_id = relay.pending().next_request_id();
    Response::ok(files_mkdir(relay, &worker_fp, viewer_id, &req.path, &request_id).await?)
}

/// One chunk of an upload. v2 `attachFileChunk` checks `upload_id`, then
/// `session_id`, then resolves the session's worker, in that order.
pub async fn handle_attach_file_chunk(
    core: &CoordCore,
    caller: &Caller,
    req: AttachFileChunkRequest,
) -> ServiceResult<AttachFileChunkResponse> {
    require_account_device(caller)?;
    if req.upload_id.is_empty() {
        return Err(ConnectError::new(
            ErrorCode::InvalidArgument,
            "upload_id required",
        ));
    }
    let (session, binding) = session_binding(core, &req.session_id, true).await?;
    let abs_path =
        attach_file_chunk(&core.services.scrollback, &binding.worker_fp, &session, req).await?;
    Response::ok(AttachFileChunkResponse {
        abs_path,
        ..Default::default()
    })
}

/// The content-dedup probe. v2 `attachmentProbe`.
pub async fn handle_attachment_probe(
    core: &CoordCore,
    caller: &Caller,
    req: AttachmentProbeRequest,
) -> ServiceResult<AttachmentProbeResponse> {
    let viewer_id = require_account_device(caller)?;
    let (session, binding) = session_binding(core, &req.session_id, true).await?;
    let (hit, abs_path) = attachment_probe(
        &core.services.scrollback,
        &binding.worker_fp,
        viewer_id,
        &session,
        &req.sha256,
        req.short_path,
        &core.services.scrollback.pending().next_request_id(),
    )
    .await?;
    Response::ok(AttachmentProbeResponse {
        hit,
        abs_path,
        ..Default::default()
    })
}

/// A session's attachment directory. v2 `listAttachments`.
pub async fn handle_list_attachments(
    core: &CoordCore,
    caller: &Caller,
    req: ListAttachmentsRequest,
) -> ServiceResult<ListAttachmentsResponse> {
    let viewer_id = require_account_device(caller)?;
    let (session, binding) = session_binding(core, &req.session_id, false).await?;
    let entries = list_attachments(
        &core.services.scrollback,
        &binding.worker_fp,
        viewer_id,
        &session,
        &core.services.scrollback.pending().next_request_id(),
    )
    .await?;
    Response::ok(ListAttachmentsResponse {
        entries,
        ..Default::default()
    })
}

/// Delete one attachment. v2 `deleteAttachment`.
pub async fn handle_delete_attachment(
    core: &CoordCore,
    caller: &Caller,
    req: DeleteAttachmentRequest,
) -> ServiceResult<DeleteAttachmentResponse> {
    let viewer_id = require_account_device(caller)?;
    let (session, binding) = session_binding(core, &req.session_id, false).await?;
    let ok = delete_attachment(
        &core.services.scrollback,
        &binding.worker_fp,
        viewer_id,
        &session,
        &req.filename,
        &core.services.scrollback.pending().next_request_id(),
    )
    .await?;
    Response::ok(DeleteAttachmentResponse {
        ok,
        ..Default::default()
    })
}

/// The session a request names and its live worker, as v2's
/// `requireSessionWorkerSocket` answers: `NotFound` "session not found" for a
/// session that is not there — including an id that is not one, because v2
/// looks it up rather than parsing it — and `Unavailable` "worker offline".
/// `presence_required` is v2's explicit `session_id required` check, which only
/// the upload and the probe make.
async fn session_binding(
    core: &CoordCore,
    raw: &str,
    presence_required: bool,
) -> Result<(SessionId, SessionWorkerBinding), ConnectError> {
    if presence_required && raw.is_empty() {
        return Err(ConnectError::new(
            ErrorCode::InvalidArgument,
            "session_id required",
        ));
    }
    let session = SessionId::try_from(raw)
        .map_err(|_| ConnectError::new(ErrorCode::NotFound, "session not found"))?;
    let binding = core
        .services
        .scrollback
        .session_worker_socket(&core.services.db, &session)
        .await?;
    Ok((session, binding))
}
