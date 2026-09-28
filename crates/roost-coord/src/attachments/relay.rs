//! The relay under the file and attachment RPCs: one send, one correlation
//! wait, and the decode policy every worker reply goes through.
//!
//! Ported from v2 `apps/coord/src/attachments/handlers-attachments.ts`
//! (`createPendingRpc` + `sendBrowserCmd`). Called by `files` and
//! `session_files`; depends on `ScrollbackRelay` for the correlation table.

use serde_json::Value;

use crate::terminal_screen::scrollback_relay::ScrollbackRelay;
use crate::workers::send::{SendOutcome, SendRefusal};
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::control::ClientControlFrame;

use connectrpc::{ConnectError, ErrorCode};

/// The refusal table's Rust side.
///
/// v2 threw from `requireWorkerHubSocket` (`:29-40`) with two distinct codes,
/// and **a client acts on the difference**: `NotFound` means the MACHINE is gone
/// and it should re-pair; `FailedPrecondition` means the row exists but the
/// LINK is down and a retry is right. Collapsing them costs a user a re-pair
/// they did not need.
///
/// |condition|v2|Rust|why|
/// |---|---|---|---|
/// |fingerprint absent, or `deleted_at_ms` set|NotFound|NotFound|the machine is not there|
/// |row present, no routable generation|FailedPrecondition|FailedPrecondition|the link is down, retry|
/// |socket took the frame and dropped it|Unavailable|Unavailable|retryable, so not a statement failure|
fn refuse_send(outcome: SendOutcome, worker_fp: &str) -> ConnectError {
    let SendOutcome::Refused(refusal) = outcome else {
        return ConnectError::new(ErrorCode::Internal, "a send was expected to be refused");
    };
    match refusal {
        // A row present but no routable generation: the LINK is down.
        SendRefusal::NoRoutableGeneration { .. } | SendRefusal::SupersededGeneration { .. } => {
            tracing::info!(worker_fp, "files: the worker has no routable generation");
            ConnectError::new(ErrorCode::FailedPrecondition, "worker not connected")
        }
        // The socket took it and dropped it: retryable, so NOT a statement
        // failure, which is why this is `Unavailable` and not `Internal`.
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
/// defaults it. `resolved_path ?? req.path` is v2 `:84` and `:98` — the
/// fallback is behaviour, and it is why this is a function rather than a `?`.
pub(super) fn optional_str<'a>(reply: &'a Value, field: &str, fallback: &'a str) -> &'a str {
    reply.get(field).and_then(Value::as_str).unwrap_or(fallback)
}

/// v2 `:60-62`, twice: an absent or empty `content_b64` is an EMPTY ARRAY, not
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

/// v2 `:60` for the falsy-zero mtime and v2 `:84` for the fallback.
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
/// The two refusals v2 draws from `createPendingRpc` are not reproduced here:
/// `create` refuses a duplicate with `AlreadyExists` by Rust's decision, and
/// that error is already a `ConnectError`, so it propagates unchanged.
pub(super) async fn relay_once(
    relay: &ScrollbackRelay,
    worker_fp: &str,
    viewer_id: &str,
    request_id: &str,
    deadline_ms: i64,
    frame: ClientControlFrame,
) -> Result<Value, ConnectError> {
    // `browser_id` and `viewer_id` travel as the SAME value, and that is the
    // established convention rather than a shortcut — `rpc_relay.rs:81-82`
    // under a comment saying both are opaque to the worker.
    // `WorkerFp` is not built from a `&str` by `From`, because a fingerprint
    // that is not one is a caller's error rather than a default.
    let worker_fp_typed = WorkerFp::try_from(worker_fp).map_err(|_| {
        ConnectError::new(ErrorCode::InvalidArgument, "worker_fp is not a fingerprint")
    })?;
    let outcome = crate::workers::send::send_browser_command(
        relay.workers(),
        &worker_fp_typed,
        viewer_id,
        viewer_id,
        request_id,
        frame,
    );
    if let SendOutcome::Refused(_) = outcome {
        return Err(refuse_send(outcome, worker_fp));
    }
    let now_ms = relay.now_ms();
    let mut pending = relay
        .pending()
        .create(request_id, Some(worker_fp), now_ms)?;
    pending.settle().await
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
