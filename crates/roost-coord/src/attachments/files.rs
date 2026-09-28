//! The seven file RPCs, relayed to a worker and back. Ports v2's
//! `apps/coord/src/attachments/handlers-attachments.ts` (193 lines).
//!
//! NO STATE, and that is the design claim rather than a description: every
//! method here is a request/response relay whose only durable thing is the
//! correlation entry, and that belongs to `ScrollbackRelay::pending`. So this
//! file adds no field to `AttachmentsRuntime` — **and the test for that claim
//! is one question: what concern is any field here?** A field that cannot
//! answer it is a second concern wearing this slice's clothes.
//!
//! # THE DECODE IS THE SLICE, not the refusals
//!
//! `PendingRpc::settle` yields `serde_json::Value` where v2 cast freely on
//! fields that may be absent (`data.size`, `e.resolved_path`). **Every
//! extraction is therefore a decision, and the policy is stated here so the
//! seven sites cannot each invent an answer:** a field the worker's contract
//! guarantees is unwrapped and a violation is `Internal` (a coordinator bug, not
//! a caller's); a field the contract makes OPTIONAL is defaulted the way v2
//! defaults it, never `unwrap_or_default` on a wrong type. Seven methods that
//! each invented a malformed-reply answer is the failure mode this prevents.
//!
//! # THE REFUSAL TABLE, once, because seven re-derivations disagree by the fourth
//!
//! v2 threw from `requireWorkerHubSocket` (`:29-40`) with two distinct codes,
//! and **a client acts on the difference**: `NotFound` means the MACHINE is gone
//! and it should re-pair; `FailedPrecondition` means the row exists but the
//! LINK is down and a retry is right. Collapsing them costs a user a re-pair
//! they did not need.
//!
//! |condition|v2|Rust|why|
//! |---|---|---|---|
//! |fingerprint absent, or `deleted_at_ms` set|NotFound|NotFound|the machine is not there|
//! |row present, no routable generation|FailedPrecondition|FailedPrecondition|the link is down, retry|
//! |socket took the frame and dropped it|Unavailable|Unavailable|retryable, so not a statement failure|
//!
//! # NAMED HAZARDS, each a case where a faithful-looking port changes behaviour
//!
//! - `resolved_path ?? req.path` — **twice**, list-dir and mkdir (v2 `:84`,
//!   `:98`). The fallback is behaviour, not convenience, and it only shows up
//!   when a worker omits the field.
//! - `mtime_ms ? BigInt(mtime_ms) : 0n` (v2 `:83`) — a falsy zero is a REAL
//!   zero, not a missing value.
//! - `Math.trunc(e.mtime_ms)` on list-attachments (v2 `:151`) — the worker
//!   sends a float and the proto field is an integer.
//! - `atob("")` yields an EMPTY ARRAY, not an error (v2 `:60-62`), twice.
//! - non-final upload chunks are fire-and-forget and answer `{absPath: ""}` by
//!   design, not by omission (v2 `:112`).
//!
//! # TWO WHY COMMENTS A HURRIED READER DROPS
//!
//! `PendingRpcs::create` refuses a duplicate with `AlreadyExists` where v2
//! silently re-points the completion. **Rust decided this: the first caller
//! owns the completion, and silently re-pointing it would let a replayed
//! request steal another call's result.**
//!
//! The final chunk's pending is registered BEFORE the chunk is sent, because
//! the worker's `rpc-ok` would otherwise race ahead of the entry (v2 `:96-98`).
//! Invisible when it is right; it loses the last chunk when it is wrong.
//!
//! # ONE CLOCK
//!
//! `relay.now_ms()` (`scrollback_relay.rs:93`), not the free `now_ms()` the
//! crate also has: a slice that reads the wall clock in one place and the relay
//! clock in another produces timestamps that disagree under load for reasons
//! nobody will find.
//!
//! # WHAT IT AWAITS
//!
//! Two things, not one — a send lane and a REPLY correlation, and the second is
//! the one easily mistaken for the first (v2 `:1-5`). Both live on
//! `ScrollbackRelay`: `pending()` (`scrollback_relay.rs:87`) and
//! `session_worker_socket` (`:174`).

use serde_json::Value;

use crate::terminal_screen::scrollback_relay::ScrollbackRelay;
use crate::workers::send::{SendOutcome, SendRefusal};
use roost_proto::{
    AttachFileChunkResponse, AttachmentEntry, AttachmentProbeResponse, DeleteAttachmentResponse,
    FilesListDirEntry, FilesListDirResponse, FilesMkdirResponse, FilesReadChunkResponse,
    FilesReadResponse, ListAttachmentsResponse,
};
use roost_protocol::wire::SessionId;
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;

use connectrpc::{ConnectError, ErrorCode};

/// The v2 deadline for a whole-file read, a list-dir, a mkdir, a probe, a
/// listing and a delete: `10_000` ms at v2 `:60`, `:74`, `:94`, `:131`, `:141`.
const DEADLINE_MS: i64 = 10_000;
/// The paged read's own deadline — `30_000` ms at v2 `:73`.
const CHUNK_DEADLINE_MS: i64 = 30_000;
/// The final upload chunk's deadline, "5 min covers a multi-GB tailnet upload"
/// (v2 `:99`).
const UPLOAD_DEADLINE_MS: i64 = 300_000;
/// `len > 4 * 1024 * 1024` is refused (v2 `:70-72`).
const MAX_CHUNK_LEN: i64 = 4 * 1024 * 1024;

/// The refusal table's Rust side. See the module header for the v2 column.
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
fn required<'a>(reply: &'a Value, field: &str) -> Result<&'a Value, ConnectError> {
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
fn optional_str<'a>(reply: &'a Value, field: &str, fallback: &'a str) -> &'a str {
    reply.get(field).and_then(Value::as_str).unwrap_or(fallback)
}

/// v2 `:60-62`, twice: an absent or empty `content_b64` is an EMPTY ARRAY, not
/// an error. A zero-byte file is a real answer.
fn base64_to_bytes(reply: &Value) -> Result<Vec<u8>, ConnectError> {
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

/// Whole-file read. v2 `:56-67`.
pub async fn files_read(
    relay: &ScrollbackRelay,
    worker_fp: &str,
    viewer_id: &str,
    path: &str,
    request_id: &str,
) -> Result<FilesReadResponse, ConnectError> {
    let reply = relay_once(
        relay,
        worker_fp,
        viewer_id,
        request_id,
        DEADLINE_MS,
        ClientControlFrame::ReadFile {
            request_id: request_id.to_owned(),
            path: path.to_owned(),
            max_lines: None,
            trace_id: None,
        },
    )
    .await?;
    Ok(FilesReadResponse {
        __buffa_unknown_fields: Default::default(),
        data: base64_to_bytes(&reply)?,
        size: u64_field(&reply, "size")?,
    })
}

/// The paged read behind the download's progress bar. v2 `:69-84`.
pub async fn files_read_chunk(
    relay: &ScrollbackRelay,
    worker_fp: &str,
    viewer_id: &str,
    path: &str,
    offset: i64,
    len: i64,
    request_id: &str,
) -> Result<FilesReadChunkResponse, ConnectError> {
    if len <= 0 || len > MAX_CHUNK_LEN {
        return Err(ConnectError::new(
            ErrorCode::InvalidArgument,
            "file chunk length must be between 1 and 4194304 bytes",
        ));
    }
    let reply = relay_once(
        relay,
        worker_fp,
        viewer_id,
        request_id,
        CHUNK_DEADLINE_MS,
        ClientControlFrame::ReadFileChunk {
            request_id: request_id.to_owned(),
            path: path.to_owned(),
            offset,
            len,
            trace_id: None,
        },
    )
    .await?;
    Ok(FilesReadChunkResponse {
        __buffa_unknown_fields: Default::default(),
        data: base64_to_bytes(&reply)?,
        size: u64_field(&reply, "size")?,
        eof: reply.get("eof").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// v2 `:86-99`. The `resolved_path` fallback is v2 `:98`.
pub async fn files_mkdir(
    relay: &ScrollbackRelay,
    worker_fp: &str,
    viewer_id: &str,
    path: &str,
    request_id: &str,
) -> Result<FilesMkdirResponse, ConnectError> {
    let reply = relay_once(
        relay,
        worker_fp,
        viewer_id,
        request_id,
        DEADLINE_MS,
        ClientControlFrame::Mkdir {
            request_id: request_id.to_owned(),
            path: path.to_owned(),
            trace_id: None,
        },
    )
    .await?;
    Ok(FilesMkdirResponse {
        __buffa_unknown_fields: Default::default(),
        resolved_path: optional_str(&reply, "resolved_path", path).to_owned(),
    })
}

/// v2 `:60` for the falsy-zero mtime and v2 `:84` for the fallback.
fn u64_field(reply: &Value, field: &str) -> Result<u64, ConnectError> {
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
async fn relay_once(
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

/// The list-dir and listing decoders, kept beside the shared policy.
pub fn files_list_dir_entries(reply: &Value) -> Result<Vec<FilesListDirEntry>, ConnectError> {
    let mut entries = Vec::new();
    for entry in required(reply, "entries")?
        .as_array()
        .ok_or_else(|| ConnectError::new(ErrorCode::Internal, "entries was not a list"))?
    {
        let mtime = entry.get("mtime_ms").and_then(Value::as_u64).unwrap_or(0);
        entries.push(FilesListDirEntry {
            __buffa_unknown_fields: Default::default(),
            name: entry
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            is_dir: entry
                .get("isDir")
                .or_else(|| entry.get("is_dir"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            // v2 `:83`: `mtime_ms ? BigInt(mtime_ms) : 0n` — a falsy ZERO is a
            // real zero, so this is an unwrap of the option, not of the value.
            mtime_ms: mtime,
        });
    }
    Ok(entries)
}

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

/// List a directory. v2 `:86-95`, and the `resolved_path` fallback is v2 `:95`.
pub async fn files_list_dir(
    relay: &ScrollbackRelay,
    worker_fp: &str,
    viewer_id: &str,
    path: &str,
    request_id: &str,
) -> Result<FilesListDirResponse, ConnectError> {
    let reply = relay_once(
        relay,
        worker_fp,
        viewer_id,
        request_id,
        DEADLINE_MS,
        ClientControlFrame::ListDir {
            request_id: request_id.to_owned(),
            path: path.to_owned(),
            trace_id: None,
        },
    )
    .await?;
    Ok(FilesListDirResponse {
        __buffa_unknown_fields: Default::default(),
        entries: files_list_dir_entries(&reply)?,
        resolved_path: optional_str(&reply, "resolved_path", path).to_owned(),
    })
}

/// The content-dedup probe, so the SPA skips a byte upload it already has.
/// v2 `:126-140`.
pub async fn attachment_probe(
    relay: &ScrollbackRelay,
    worker_fp: &str,
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
    worker_fp: &str,
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
    worker_fp: &str,
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
