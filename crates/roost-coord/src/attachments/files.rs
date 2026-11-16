//! The four worker-keyed file RPCs: whole-file read, paged read, list-dir and
//! mkdir, relayed to a worker and back.
//!
//! Called by `rpc_files`; the send, wait and decode policy are `attachments::relay`.
//!
//! No state: every method is a request/response relay whose only durable thing
//! is the correlation entry, and that belongs to `ScrollbackRelay::pending`.
//!
//! Named hazards, each a case where a plausible simplification changes behaviour:
//! - The `resolved_path` fallback to the request path -- twice, list-dir and
//!   mkdir. It only shows up when a worker omits the field.
//! - A zero `mtime_ms` is a REAL zero, not a missing value.
//! - An absent or empty `content_b64` is an EMPTY ARRAY, not an error, twice.

use serde_json::Value;

use super::relay::{base64_to_bytes, optional_str, relay_once, required, u64_field};
use crate::terminal_screen::scrollback_relay::ScrollbackRelay;
use roost_proto::{
    FilesListDirEntry, FilesListDirResponse, FilesMkdirResponse, FilesReadChunkResponse,
    FilesReadResponse,
};
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::control::ClientControlFrame;

use connectrpc::{ConnectError, ErrorCode};

/// The deadline for a whole-file read, a list-dir and a mkdir: `10_000` ms.
const DEADLINE_MS: u64 = 10_000;
/// The paged read's own deadline — `30_000` ms.
const CHUNK_DEADLINE_MS: u64 = 30_000;
/// `len > 4 * 1024 * 1024` is refused.
const MAX_CHUNK_LEN: i64 = 4 * 1024 * 1024;

/// Whole-file read.
pub async fn files_read(
    relay: &ScrollbackRelay,
    worker_fp: &WorkerFp,
    viewer_id: &str,
    path: &str,
    revision: &str,
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
            revision: (!revision.is_empty()).then(|| revision.to_owned()),
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

/// The paged read behind the download's progress bar.
pub async fn files_read_chunk(
    relay: &ScrollbackRelay,
    worker_fp: &WorkerFp,
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

/// Create a directory. The `resolved_path` fallback applies here too.
pub async fn files_mkdir(
    relay: &ScrollbackRelay,
    worker_fp: &WorkerFp,
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

/// The list-dir decoder.
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
            // ONE spelling, the one the contract uses. Tolerating a second
            // spelling is worse than the bug it hides: a misspelling makes a
            // proto bool default to FALSE, so every subdirectory would arrive
            // as a FILE while the surrounding chrome stayed correct. A field
            // name is not a compatibility surface.
            is_dir: entry.get("isDir").and_then(Value::as_bool).unwrap_or(false),
            // A ZERO is a real zero, so this is an unwrap of the option, not of
            // the value.
            mtime_ms: mtime,
        });
    }
    Ok(entries)
}

/// List a directory; the `resolved_path` fallback applies.
pub async fn files_list_dir(
    relay: &ScrollbackRelay,
    worker_fp: &WorkerFp,
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
