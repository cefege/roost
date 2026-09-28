//! The authenticated direct-attachment RPCs: mint one immutable grant, read one
//! durable receipt, and negotiate one attachment peer. Each authorizes the live
//! session route or the exact tab, then delegates to the separate grant, status
//! and peer owners; no attachment byte crosses the coordinator here.
//! Called by `rpc::service_impl`. Ports `apps/coord/src/attachments/handlers-attachments-direct.ts`
//! and `apps/coord/src/attachments/handlers-attachments-peer.ts`.

use connectrpc::{ConnectError, ErrorCode, Response, ServiceResult};
use roost_proto::buffa::MessageField;
use roost_proto::{
    AttachmentsDirectStatusRequest, AttachmentsDirectStatusResponse, AttachmentsGrantDirectRequest,
    AttachmentsGrantDirectResponse, SessionsNegotiateAttachmentPeerRequest,
    SessionsNegotiateAttachmentPeerResponse,
};
use roost_protocol::versioning::CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1;

use crate::attachments::grant_state::{
    AttachmentGrantDescriptor, AttachmentGrantRequest, attachment_grant_denied,
    attachment_grant_invalid, attachment_grant_unavailable, current_routable_by_name,
};
use crate::attachments::peer_state::{AttachmentPeerCaller, AttachmentPeerSignalConfig};
use crate::attachments::transfer_limits::{
    ATTACHMENT_TRANSFER_GRANT_TTL_MS, MAX_SAFE_INTEGER, is_bounded_identifier, is_opaque_upload_id,
};
use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::db::CoordDb;

const DIRECT_STATUS_IDENTIFIER_MAX_UTF8_BYTES: usize = 128;

/// Mint one direct grant for the caller's tab. v2 `attachmentsGrantDirect`.
pub async fn handle_attachments_grant_direct(
    core: &CoordCore,
    caller: &Caller,
    request: AttachmentsGrantDirectRequest,
) -> ServiceResult<AttachmentsGrantDirectResponse> {
    let device_fingerprint = require_account_device(caller)?.to_owned();
    let owner_key = browser_owner_key(caller)?;
    let tab_id = require_attachment_grant_tab(&request.tab_id, caller.tab_id.as_deref())?;
    let config = core.services.boot.require_config()?;
    if request.total_bytes > MAX_SAFE_INTEGER {
        return Err(attachment_grant_invalid(
            "attachment grant descriptor is invalid",
        ));
    }
    let descriptor = AttachmentGrantDescriptor {
        session_id: request.session_id.clone(),
        upload_id: request.upload_id,
        filename: request.filename,
        short_path: request.short_path,
        total_bytes: request.total_bytes,
    };
    let db = &core.services.db;
    let (session_id, worker_fp) = (request.session_id, request.worker_fp.clone());
    let granted = core
        .services
        .attachments
        .grants()
        .grant(
            AttachmentGrantRequest {
                owner_key,
                device_fingerprint,
                tab_id: tab_id.to_owned(),
                worker_fp: request.worker_fp,
                descriptor,
            },
            || authorize_attachment_session(db, &session_id, &worker_fp),
        )
        .await?;
    let peer_supported = config.terminal_peer_enabled
        && granted
            .lease
            .worker_handle
            .capabilities
            .contains(CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1);
    Response::ok(AttachmentsGrantDirectResponse {
        grant_id: granted.lease.grant_id,
        secret: granted.secret,
        ttl_ms: ATTACHMENT_TRANSFER_GRANT_TTL_MS,
        worker_epoch: granted.lease.worker_epoch,
        peer_supported,
        stun_urls: if peer_supported {
            config.terminal_peer_stun_urls.clone()
        } else {
            Vec::new()
        },
        ..Default::default()
    })
}

/// Read one upload's durable receipt from the session's current worker.
/// v2 `attachmentsDirectStatus`.
pub async fn handle_attachments_direct_status(
    core: &CoordCore,
    caller: &Caller,
    request: AttachmentsDirectStatusRequest,
) -> ServiceResult<AttachmentsDirectStatusResponse> {
    require_account_device(caller)?;
    if !is_bounded_identifier(&request.session_id, DIRECT_STATUS_IDENTIFIER_MAX_UTF8_BYTES)
        || !is_opaque_upload_id(&request.upload_id, DIRECT_STATUS_IDENTIFIER_MAX_UTF8_BYTES)
    {
        return Err(attachment_grant_invalid(
            "attachment status request is invalid",
        ));
    }
    let worker_fp = attachment_session_worker(&core.services.db, &request.session_id).await?;
    let worker = current_routable_by_name(&core.services.workers, &worker_fp)
        .ok_or_else(|| attachment_grant_unavailable("attachment status worker is unavailable"))?;
    let status = core
        .services
        .attachments
        .statuses()
        .request(&worker, &request.session_id, &request.upload_id)
        .await?;
    Response::ok(AttachmentsDirectStatusResponse {
        status: MessageField::some(status),
        ..Default::default()
    })
}

/// Bridge one attachment-peer negotiation into its owner with the
/// authenticated device and exact tab. v2 `sessionsNegotiateAttachmentPeer`.
pub async fn handle_sessions_negotiate_attachment_peer(
    core: &CoordCore,
    caller: &Caller,
    request: SessionsNegotiateAttachmentPeerRequest,
) -> ServiceResult<SessionsNegotiateAttachmentPeerResponse> {
    let device_fingerprint = require_account_device(caller)?.to_owned();
    let config = core.services.boot.require_config()?;
    let peer_caller = AttachmentPeerCaller {
        owner_key: browser_owner_key(caller)?,
        device_fingerprint,
        tab_id: caller.tab_id.clone(),
    };
    let signal = AttachmentPeerSignalConfig {
        enabled: config.terminal_peer_enabled,
        stun_urls: config.terminal_peer_stun_urls.clone(),
    };
    let peers = core.services.attachments.peers();
    Response::ok(peers.negotiate(&peer_caller, &request, &signal).await?)
}

/// The owner key of a browser principal; `require_account_device` has already
/// refused a machine, which is the only principal without one.
fn browser_owner_key(caller: &Caller) -> Result<String, ConnectError> {
    caller
        .principal
        .capture_owner_key()
        .ok_or_else(|| ConnectError::new(ErrorCode::Unauthenticated, "authentication required"))
}

fn require_attachment_grant_tab<'a>(
    requested: &'a str,
    authenticated: Option<&str>,
) -> Result<&'a str, ConnectError> {
    let Some(authenticated) = authenticated else {
        return Err(attachment_grant_invalid("attachment grant tab is invalid"));
    };
    if requested != authenticated {
        return Err(attachment_grant_denied(
            "attachment grant tab does not match the authenticated document",
        ));
    }
    Ok(requested)
}

/// The grant's authorization, re-run after the worker acknowledged it: the
/// session must still be open and still on the named worker.
async fn authorize_attachment_session(
    db: &CoordDb,
    session_id: &str,
    expected_worker_fp: &str,
) -> Result<(), ConnectError> {
    if attachment_session_worker(db, session_id).await? != expected_worker_fp {
        return Err(attachment_grant_denied(
            "attachment grant worker does not own the session",
        ));
    }
    Ok(())
}

/// The worker an open session lives on, refusing a closed session or one whose
/// worker was deleted.
async fn attachment_session_worker(db: &CoordDb, session_id: &str) -> Result<String, ConnectError> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT session.worker_fp, session.status FROM sessions AS session \
         INNER JOIN workers AS worker ON worker.fp = session.worker_fp \
         WHERE session.id = ?1 AND worker.deleted_at_ms IS NULL",
    )
    .bind(session_id)
    .fetch_optional(db.pool())
    .await
    .map_err(|error| {
        tracing::error!(session_id, %error, "attachments: the session worker lookup failed");
        ConnectError::new(ErrorCode::Internal, "attachment session lookup failed")
    })?;
    match row {
        Some((worker_fp, status)) if status == "open" => Ok(worker_fp),
        _ => Err(ConnectError::new(
            ErrorCode::NotFound,
            "attachment session is unavailable",
        )),
    }
}
