//! A session's attachment directory as browser commands see it: what is in it,
//! removing one file from it, and whether bytes the browser is about to upload
//! are already there. Ports v2
//! `apps/worker/src/attachments/browser-command-attachments.ts`; the directory
//! rules and the dedup manifest are `crate::attachments` (`store_paths`,
//! `file_store`), the one owner every upload path shares.

use std::path::Path;
use std::time::UNIX_EPOCH;

use roost_protocol::wire::brand::SessionId;
use roost_protocol::wire::control::ClientControlFrame;
use serde_json::Value;

use super::{Answered, Boxed, Command, Deps, Refusal, Reply};
use crate::attachments::file_store::probe_attachment;
use crate::attachments::store_paths::{AttachmentBase, MANIFEST_NAME, join_lexically};

/// What an attachment command answers with.
pub type AttachmentOutcome = Result<Value, Refusal>;

/// The on-disk store behind one session's attachment directory.
pub trait AttachmentStore: Send + Sync {
    /// The files a session holds, newest first, the dedup manifest excluded.
    fn list(&self, session_id: SessionId) -> Boxed<AttachmentOutcome>;

    /// Remove one file. A file that is already gone is a success: what the
    /// caller asked for is that it is not there.
    fn delete(&self, session_id: SessionId, filename: String) -> Boxed<AttachmentOutcome>;

    /// Whether this session's manifest already holds bytes with this digest.
    fn probe(
        &self,
        session_id: SessionId,
        sha256: String,
        short_path: bool,
    ) -> Boxed<AttachmentOutcome>;
}

/// Run whichever attachment command arrived.
pub async fn execute(command: &Command, deps: &Deps) -> Result<Answered, Refusal> {
    let store = deps.attachments.as_ref();
    let request_id = command.request_id.as_str();
    let outcome = match &command.frame {
        ClientControlFrame::ListAttachments { session_id, .. } => {
            store.list(session_id.clone()).await
        }
        ClientControlFrame::DeleteAttachment {
            session_id,
            filename,
            ..
        } => store.delete(session_id.clone(), filename.clone()).await,
        ClientControlFrame::AttachmentProbe {
            session_id,
            sha256,
            short_path,
            ..
        } => {
            store
                .probe(session_id.clone(), sha256.clone(), *short_path)
                .await
        }
        other => {
            return Err(Refusal::failed(
                "attachments",
                format!("{} is not an attachment command", other.kind()),
            ));
        }
    };
    outcome.map(|data| Answered::Reply(Reply::ok(request_id, data)))
}

/// The store this worker serves from: the same base the upload owner writes.
#[derive(Debug, Clone)]
pub struct SessionAttachments {
    base: AttachmentBase,
}

impl SessionAttachments {
    pub fn new(base: AttachmentBase) -> Self {
        Self { base }
    }
}

/// v2's `rpc-error` for a session id that resolves outside the base.
fn invalid_session(kind: &'static str) -> Refusal {
    Refusal::failed(kind, "invalid session_id")
}

/// v2 `handleDeleteAttachment`: a bare name, never a separator or a directory
/// reference.
fn check_leaf(filename: &str) -> Result<(), Refusal> {
    if filename.contains(['/', '\\']) || filename == ".." || filename == "." {
        return Err(Refusal::failed("delete-attachment", "invalid filename"));
    }
    Ok(())
}

impl AttachmentStore for SessionAttachments {
    fn list(&self, session_id: SessionId) -> Boxed<AttachmentOutcome> {
        let base = self.base.clone();
        Box::pin(async move {
            let dir = base
                .resolve_session_dir(session_id.as_str())
                .ok_or_else(|| invalid_session("list-attachments"))?;
            tokio::task::spawn_blocking(move || list_session_dir(&dir))
                .await
                .map_err(|error| Refusal::failed("list-attachments", error.to_string()))?
        })
    }

    fn delete(&self, session_id: SessionId, filename: String) -> Boxed<AttachmentOutcome> {
        let base = self.base.clone();
        Box::pin(async move {
            check_leaf(&filename)?;
            let dir = base
                .resolve_session_dir(session_id.as_str())
                .ok_or_else(|| invalid_session("delete-attachment"))?;
            let target = join_lexically(&dir, &filename);
            match tokio::fs::remove_file(&target).await {
                Ok(()) => {
                    tracing::info!(session_id = %session_id, filename = %filename, "a browser command deleted an attachment");
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(Refusal::failed(
                        "delete-attachment",
                        format!("{}: {error}", target.display()),
                    ));
                }
            }
            Ok(serde_json::json!({ "ok": true }))
        })
    }

    fn probe(
        &self,
        session_id: SessionId,
        sha256: String,
        short_path: bool,
    ) -> Boxed<AttachmentOutcome> {
        let base = self.base.clone();
        Box::pin(async move {
            let probe = tokio::task::spawn_blocking(move || {
                probe_attachment(&base, session_id.as_str(), &sha256, short_path)
            })
            .await
            .map_err(|error| Refusal::failed("attachment-probe", error.to_string()))?;
            Ok(serde_json::json!({ "hit": probe.hit, "abs_path": probe.abs_path }))
        })
    }
}

/// Every regular file but the manifest, newest first. A session that never
/// took an upload has no directory, and "no attachments" is the truthful
/// answer; one unreadable entry never fails a listing.
fn list_session_dir(dir: &Path) -> AttachmentOutcome {
    let mut entries = Vec::new();
    if dir.exists() {
        let reader = std::fs::read_dir(dir).map_err(|error| {
            Refusal::failed("list-attachments", format!("{}: {error}", dir.display()))
        })?;
        for entry in reader.filter_map(Result::ok) {
            let filename = entry.file_name().to_string_lossy().into_owned();
            if filename == MANIFEST_NAME {
                continue;
            }
            let path = entry.path();
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            let mtime_ms = metadata
                .modified()
                .ok()
                .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                .map_or(0.0, |since| {
                    since.as_secs() as f64 * 1_000.0 + f64::from(since.subsec_nanos()) / 1_000_000.0
                });
            entries.push((
                mtime_ms,
                serde_json::json!({
                    "filename": filename,
                    "size_bytes": metadata.len(),
                    "mtime_ms": mtime_ms,
                    "abs_path": path.to_string_lossy(),
                }),
            ));
        }
    }
    entries.sort_by(|left, right| right.0.total_cmp(&left.0));
    let entries: Vec<Value> = entries.into_iter().map(|(_, entry)| entry).collect();
    Ok(serde_json::json!({ "entries": entries }))
}
