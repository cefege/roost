//! The four session-keyed attachment RPCs: the content-dedup probe, the
//! listing, the delete, and the chunked upload.
//!
//! Ported from v2 `apps/coord/src/attachments/handlers-attachments.ts`
//! (`attachFileChunk`, `attachmentProbe`, `listAttachments`,
//! `deleteAttachment`). Called by `rpc_files`; the relay is `attachments::relay`.

use serde_json::Value;

use super::relay::{optional_str, relay_once, required, settle_within};
use crate::terminal_screen::scrollback_relay::ScrollbackRelay;
use crate::workers::send::SendOutcome;
use roost_proto::{AttachFileChunkRequest, AttachmentEntry, DAttachmentChunk};
use roost_protocol::wire::SessionId;
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;

use connectrpc::{ConnectError, ErrorCode};

/// The v2 deadline for a probe, a listing and a delete: `10_000` ms at v2
/// `:131`, `:141`.
const DEADLINE_MS: u64 = 10_000;
/// The final upload chunk's deadline, "5 min covers a multi-GB tailnet upload"
/// (v2 `:99`).
const UPLOAD_DEADLINE_MS: u64 = 300_000;

/// v2 `:151` truncates the float the worker sends, because the proto field is
/// an integer and a fractional mtime would not encode.
pub fn attachment_entries(reply: &Value) -> Result<Vec<AttachmentEntry>, ConnectError> {
    let mut entries = Vec::new();
    for entry in required(reply, "entries")?
        .as_array()
        .ok_or_else(|| ConnectError::new(ErrorCode::Internal, "entries was not a list"))?
    {
        entries.push(AttachmentEntry {
            __buffa_unknown_fields: Default::default(),
            filename: entry
                .get("filename")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            size_bytes: entry
                .get("size_bytes")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            mtime_ms: entry
                .get("mtime_ms")
                .and_then(Value::as_f64)
                .map_or(0.0, f64::trunc)
                .max(0.0) as u64,
            abs_path: entry
                .get("abs_path")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        });
    }
    Ok(entries)
}

/// The probe's two fields, and the delete's one.
pub fn probe_hit(reply: &Value) -> Result<(bool, String), ConnectError> {
    Ok((
        reply
            .get("hit")
            .and_then(Value::as_bool)
            .ok_or_else(|| ConnectError::new(ErrorCode::Internal, "the probe had no hit"))?,
        optional_str(reply, "abs_path", "").to_owned(),
    ))
}

/// The content-dedup probe, so the SPA skips a byte upload it already has.
/// v2 `:126-140`.
pub async fn attachment_probe(
    relay: &ScrollbackRelay,
    worker_fp: &WorkerFp,
    viewer_id: &str,
    session_id: &SessionId,
    sha256: &str,
    short_path: bool,
    request_id: &str,
) -> Result<(bool, String), ConnectError> {
    let reply = relay_once(
        relay,
        worker_fp,
        viewer_id,
        request_id,
        DEADLINE_MS,
        ClientControlFrame::AttachmentProbe {
            request_id: request_id.to_owned(),
            session_id: session_id.clone(),
            sha256: sha256.to_owned(),
            short_path,
            trace_id: None,
        },
    )
    .await?;
    probe_hit(&reply)
}

/// What a session's attachment directory already holds. v2 `:142-156`.
pub async fn list_attachments(
    relay: &ScrollbackRelay,
    worker_fp: &WorkerFp,
    viewer_id: &str,
    session_id: &SessionId,
    request_id: &str,
) -> Result<Vec<AttachmentEntry>, ConnectError> {
    let reply = relay_once(
        relay,
        worker_fp,
        viewer_id,
        request_id,
        DEADLINE_MS,
        ClientControlFrame::ListAttachments {
            request_id: request_id.to_owned(),
            session_id: session_id.clone(),
            trace_id: None,
        },
    )
    .await?;
    attachment_entries(&reply)
}

/// v2 `:158-172`.
pub async fn delete_attachment(
    relay: &ScrollbackRelay,
    worker_fp: &WorkerFp,
    viewer_id: &str,
    session_id: &SessionId,
    filename: &str,
    request_id: &str,
) -> Result<bool, ConnectError> {
    let reply = relay_once(
        relay,
        worker_fp,
        viewer_id,
        request_id,
        DEADLINE_MS,
        ClientControlFrame::DeleteAttachment {
            request_id: request_id.to_owned(),
            session_id: session_id.clone(),
            filename: filename.to_owned(),
            trace_id: None,
        },
    )
    .await?;
    reply.get("ok").and_then(Value::as_bool).ok_or_else(|| {
        ConnectError::new(ErrorCode::Internal, "the worker did not answer the delete")
    })
}

/// One bounded chunk of a chunked upload, with no size ceiling.
///
/// Stateless across calls: every chunk resolves the session's worker and is
/// relayed raw, and the worker assembles by `upload_id` into a temp file. Memory
/// here is O(chunk), not O(file) — v2 `:85-88`.
///
/// **THE PENDING IS REGISTERED BEFORE THE FINAL CHUNK IS SENT.** v2's own words
/// at `:96-98`: "so the worker's rpc-ok can't race ahead of the pending entry."
/// Invisible when it is right; it loses the last chunk when it is wrong.
///
/// **AND ONLY THE FINAL CHUNK GETS ONE.** v2 registers the pending for `last`
/// and fires every earlier chunk without one, which is what makes
/// "non-last chunks are fire-and-forget and answer `{absPath: ""}`" (v2 `:112`)
/// true rather than aspirational. It is also forced: the correlation is on
/// `upload_id`, the worker echoes it, and `PendingRpcs::create` REFUSES a
/// duplicate with `AlreadyExists` — so one entry per chunk would be a refusal on
/// chunk two. Rust refuses where v2 silently re-points the completion: the first
/// caller owns it, and re-pointing would let a replayed request steal another
/// call's result.
///
/// The request is taken by value so its bytes move into the downstream frame
/// instead of being copied; `upload_id` and `session_id` were checked by the
/// handler before the session's worker was resolved, as v2 `:116-118` orders it.
pub async fn attach_file_chunk(
    relay: &ScrollbackRelay,
    worker_fp: &WorkerFp,
    session_id: &SessionId,
    request: AttachFileChunkRequest,
) -> Result<String, ConnectError> {
    let AttachFileChunkRequest {
        upload_id,
        filename,
        short_path,
        data,
        last,
        seq,
        ..
    } = request;
    let pending = if last {
        Some(
            relay
                .pending()
                .create(&upload_id, Some(worker_fp.as_str()), relay.now_ms())?,
        )
    } else {
        None
    };
    let outcome = crate::workers::send::send_frame(
        relay.workers(),
        worker_fp,
        CoordWorkerDownstream::AttachmentChunk(DAttachmentChunk {
            request_id: upload_id.clone(),
            session_id: session_id.as_str().to_owned(),
            filename,
            short_path,
            data,
            last,
            seq,
            __buffa_unknown_fields: Default::default(),
        }),
    );
    if let SendOutcome::Refused(refusal) = outcome {
        // `Unavailable`, not `Internal` — v2 `:138` throws the same. An upload
        // interrupted mid-flight is retryable, so it is neither the caller's
        // fault nor a statement failure.
        tracing::info!(%worker_fp, %upload_id, %refusal, "attachments: an upload chunk was not sent");
        relay.pending().reject_unavailable(
            &upload_id,
            "worker disconnected mid-upload",
            Some(worker_fp.as_str()),
        );
        return Err(ConnectError::new(
            ErrorCode::Unavailable,
            "worker disconnected mid-upload",
        ));
    }
    let Some(mut pending) = pending else {
        return Ok(String::new());
    };
    let reply = settle_within(&mut pending, UPLOAD_DEADLINE_MS).await?;
    tracing::info!(%worker_fp, %upload_id, "attachments: the final upload chunk was stored");
    Ok(optional_str(&reply, "abs_path", "").to_owned())
}
