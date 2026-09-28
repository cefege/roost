//! The relay under the file and attachment RPCs: the worker lookup, one send,
//! one deadline-bounded correlation wait, and the decode policy every reply
//! goes through.
//!
//! Ported from v2 `apps/coord/src/attachments/handlers-attachments.ts`
//! (`requireWorkerHubSocket`, `createPendingRpc` + `sendBrowserCmd`). Called by
//! `files`, `session_files` and `rpc_files`; the table is `ScrollbackRelay`'s.

use std::time::Duration;

use serde_json::Value;

use crate::db::CoordDb;
use crate::terminal_screen::pending_rpcs::PendingRpc;
use crate::terminal_screen::scrollback_relay::ScrollbackRelay;
use crate::workers::send::{SendOutcome, SendRefusal};
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::control::ClientControlFrame;

use connectrpc::{ConnectError, ErrorCode};

/// Resolve the worker a Files* request names, applying the refusal table's
/// first two rows before anything is sent (v2 `requireWorkerHubSocket`,
/// `:29-40`).
///
/// **A client acts on the difference between the codes**: `NotFound` means the
/// MACHINE is gone and it should re-pair; `FailedPrecondition` means the row
/// exists but the LINK is down and a retry is right. Collapsing them costs a
/// user a re-pair they did not need.
///
/// |condition|v2|Rust|why|
/// |---|---|---|---|
/// |fingerprint absent, or `deleted_at_ms` set|NotFound|NotFound|the machine is not there|
/// |row present, no routable generation|FailedPrecondition|FailedPrecondition|the link is down, retry|
/// |socket took the frame and dropped it|Unavailable|Unavailable|retryable, so not a statement failure|
pub(super) async fn require_worker_hub_socket(
    db: &CoordDb,
    relay: &ScrollbackRelay,
    worker_fp: &str,
) -> Result<WorkerFp, ConnectError> {
    let row: Option<String> =
        sqlx::query_scalar("SELECT fp FROM workers WHERE fp = ?1 AND deleted_at_ms IS NULL")
            .bind(worker_fp)
            .fetch_optional(db.pool())
            .await
            .map_err(|error| {
                tracing::error!(worker_fp, %error, "files: the worker lookup failed");
                ConnectError::new(ErrorCode::Internal, "worker lookup failed")
            })?;
    let Some(fingerprint) = row.and_then(|fp| WorkerFp::try_from(fp).ok()) else {
        tracing::info!(worker_fp, "files: the named worker is not registered");
        return Err(ConnectError::new(ErrorCode::NotFound, "worker not found"));
    };
    if relay.workers().current_routable(&fingerprint).is_none() {
        tracing::info!(worker_fp, "files: the worker has no routable generation");
        return Err(ConnectError::new(
            ErrorCode::FailedPrecondition,
            "worker not connected",
        ));
    }
    Ok(fingerprint)
}

/// The refusal table's send-time rows: a generation that went away between the
/// lookup and the send is the link being down; a socket that dropped the frame
/// is retryable, so `Unavailable` and not `Internal`.
fn refuse_send(refusal: &SendRefusal, worker_fp: &str) -> ConnectError {
    match refusal {
        SendRefusal::NoRoutableGeneration { .. } | SendRefusal::SupersededGeneration { .. } => {
            tracing::info!(worker_fp, "files: the worker has no routable generation");
            ConnectError::new(ErrorCode::FailedPrecondition, "worker not connected")
        }
        SendRefusal::TransportDropped { .. } => {
            tracing::info!(worker_fp, "files: the socket dropped the frame");
            ConnectError::new(ErrorCode::Unavailable, "worker disconnected mid-request")
        }
    }
}

/// The decode policy's first half: a field the worker's contract GUARANTEES.
///
/// `PendingRpc::settle` yields `serde_json::Value` where v2 cast freely on
/// fields that may be absent (`data.size`, `e.resolved_path`), so every
/// extraction is a decision: a guaranteed field is unwrapped and a violation is
/// `Internal` (a coordinator bug, not a caller's); an OPTIONAL field is
/// defaulted the way v2 defaults it, never `unwrap_or_default` on a wrong type.
pub(super) fn required<'a>(reply: &'a Value, field: &str) -> Result<&'a Value, ConnectError> {
    reply.get(field).ok_or_else(|| {
        tracing::error!(
            field,
            "files: the worker's reply is missing a guaranteed field"
        );
        ConnectError::new(
            ErrorCode::Internal,
            format!("the worker's reply has no `{field}`"),
        )
    })
}

/// The decode policy's second half: an OPTIONAL field, defaulted as v2
/// defaults it. `resolved_path ?? req.path` is v2 `:92` and `:104` — the
/// fallback is behaviour, and it is why this is a function rather than a `?`.
pub(super) fn optional_str<'a>(reply: &'a Value, field: &str, fallback: &'a str) -> &'a str {
    reply.get(field).and_then(Value::as_str).unwrap_or(fallback)
}

/// v2 `:59-61`, twice: an absent or empty `content_b64` is an EMPTY ARRAY, not
/// an error. A zero-byte file is a real answer.
pub(super) fn base64_to_bytes(reply: &Value) -> Result<Vec<u8>, ConnectError> {
    let Some(encoded) = reply.get("content_b64").and_then(Value::as_str) else {
        return Ok(Vec::new());
    };
    if encoded.is_empty() {
        return Ok(Vec::new());
    }
    decode_base64(encoded).map_err(|message| {
        tracing::error!(%message, "files: the worker's content was not base64");
        ConnectError::new(
            ErrorCode::Internal,
            "the worker's file content was undecodable",
        )
    })
}

/// A guaranteed numeric field; the worker may send it as a float.
pub(super) fn u64_field(reply: &Value, field: &str) -> Result<u64, ConnectError> {
    let value = required(reply, field)?;
    value
        .as_u64()
        .or_else(|| value.as_f64().map(|f| f.max(0.0) as u64))
        .ok_or_else(|| {
            tracing::error!(field, "files: a numeric field was not a number");
            ConnectError::new(ErrorCode::Internal, "the worker's reply had a bad number")
        })
}

/// Send one command and wait for its reply, applying the refusal table once.
///
/// **THE PENDING IS REGISTERED BEFORE THE FRAME IS SENT**, as v2's
/// `createPendingRpc` precedes `sendBrowserCmd` at every site: a worker reply
/// that arrived before the entry would find nothing to settle and the caller
/// would wait out the whole deadline. A refused send drops the entry with it.
///
/// `create` refuses a duplicate with `AlreadyExists` where v2 silently
/// re-points the completion — the first caller owns it — and that error is
/// already a `ConnectError`, so it propagates unchanged. `browser_id` and
/// `viewer_id` travel as the SAME value, as `rpc_relay::send_browser_command`
/// sends them.
pub(super) async fn relay_once(
    relay: &ScrollbackRelay,
    worker_fp: &WorkerFp,
    viewer_id: &str,
    request_id: &str,
    deadline_ms: u64,
    frame: ClientControlFrame,
) -> Result<Value, ConnectError> {
    let mut pending =
        relay
            .pending()
            .create(request_id, Some(worker_fp.as_str()), relay.now_ms())?;
    let outcome = crate::workers::send::send_browser_command(
        relay.workers(),
        worker_fp,
        viewer_id,
        viewer_id,
        request_id,
        frame,
    );
    if let SendOutcome::Refused(refusal) = &outcome {
        return Err(refuse_send(refusal, worker_fp.as_str()));
    }
    settle_within(&mut pending, deadline_ms).await
}

/// Wait for a pending reply under the caller's deadline. v2's table arms the
/// timer itself and rejects with `DeadlineExceeded` "worker did not reply
/// within {ms}ms" (`router/pending-rpcs.ts`); here the caller owns the timer,
/// so the same answer is produced here.
pub(super) async fn settle_within(
    pending: &mut PendingRpc,
    deadline_ms: u64,
) -> Result<Value, ConnectError> {
    match tokio::time::timeout(Duration::from_millis(deadline_ms), pending.settle()).await {
        Ok(settled) => settled,
        Err(_) => {
            tracing::warn!(
                request_id = pending.request_id(),
                worker_fp = pending.worker_fp(),
                timeout_ms = deadline_ms,
                "files: the worker did not reply before the deadline"
            );
            Err(ConnectError::new(
                ErrorCode::DeadlineExceeded,
                format!("worker did not reply within {deadline_ms}ms"),
            ))
        }
    }
}

/// A minimal base64 decoder, so this file does not add a dependency for one
/// call. Returns the failure message rather than panicking.
fn decode_base64(encoded: &str) -> Result<Vec<u8>, String> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut table = [255u8; 256];
    for (index, byte) in ALPHABET.iter().enumerate() {
        table[*byte as usize] = index as u8;
    }
    let mut out = Vec::with_capacity(encoded.len() / 4 * 3);
    let mut accumulator: u32 = 0;
    let mut bits = 0u32;
    for byte in encoded.bytes() {
        if byte == b'=' || byte == b'\n' || byte == b'\r' {
            continue;
        }
        let value = table[byte as usize];
        if value == 255 {
            return Err("a character outside the base64 alphabet".to_owned());
        }
        accumulator = (accumulator << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((accumulator >> bits) as u8);
        }
    }
    Ok(out)
}
