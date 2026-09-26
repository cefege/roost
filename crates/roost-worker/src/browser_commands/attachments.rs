//! A session's attachment directory: what is in it, removing one file from it,
//! and whether a file the browser is about to upload is already there. Owned by
//! the worker.
//!
//! Three rules, all of them about what a browser may name.
//!
//! A SESSION'S DIRECTORY IS THE BASE PLUS ITS OWN ID. A session id is a
//! branded UUID, so the join cannot climb, and the result is checked against
//! the base anyway — the check makes the property a fact about this function
//! rather than a fact about the type that called it.
//!
//! A FILENAME IS A LEAF, NOT A PATH. No separator, no `.` or `..`, and no
//! leading dot at all: the dotfiles in a session's directory are the dedup
//! manifest and the private operation and shortcut directories, and a delete
//! that accepts one of their names destroys state the browser cannot rebuild.
//!
//! THE PROBE ANSWERS ABOUT A HASH, NEVER ABOUT A PATH. A caller sends a digest
//! and gets back the file the manifest already holds. Nothing a caller sends
//! chooses which file is read, so a probe cannot be turned into a read of an
//! arbitrary path.

use std::path::{Path, PathBuf};

use roost_platform::HostPlatform;
use roost_protocol::wire::brand::SessionId;
use roost_protocol::wire::control::ClientControlFrame;
use serde_json::Value;

use super::{Answered, Boxed, Command, Deps, Refusal, Reply};

/// The dedup index inside a session's directory: hash to filename.
///
/// It is not an attachment and never appears in a listing — it is the worker's
/// private record of which bytes it already holds, and a browser that deleted
/// it would only make itself re-upload everything.
pub const MANIFEST_NAME: &str = ".roost-manifest.json";

/// The directory name, beneath the worker's own data directory, that holds
/// every session's attachments.
pub const ATTACHMENT_DIR_NAME: &str = "attachments";

/// The most bytes a session's dedup index may hold.
///
/// It is rewritten whole on every commit, so an unbounded one is a memory cost
/// on a path that runs at upload frequency, and far above a real session's
/// needs: the index only grows when the same bytes arrive under new names.
pub const MANIFEST_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// What an attachment command answers with.
pub type AttachmentOutcome = Result<Value, Refusal>;

/// The on-disk store behind one session's attachment directory.
pub trait AttachmentStore: Send + Sync {
    /// The files a session holds, newest first, with the worker's own state
    /// excluded.
    fn list(&self, session_id: SessionId) -> Boxed<AttachmentOutcome>;

    /// Remove one file. A filename that does not resolve inside the session's
    /// directory is refused, and a file that is already gone is a success:
    /// what the caller asked for is that it is not there.
    fn delete(&self, session_id: SessionId, filename: String) -> Boxed<AttachmentOutcome>;

    /// Whether this session's directory already holds a file with this digest.
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

/// The store this worker serves from.
pub struct SessionAttachments {
    base: PathBuf,
    platform: HostPlatform,
}

impl std::fmt::Debug for SessionAttachments {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionAttachments")
            .field("base", &self.base)
            .field("platform", &self.platform)
            .finish()
    }
}

impl SessionAttachments {
    pub fn new(base: PathBuf, platform: HostPlatform) -> Self {
        Self { base, platform }
    }

    /// A copy that owns its base, so a request can move into a future without
    /// borrowing the store it came from.
    fn owned(&self) -> Self {
        Self {
            base: self.base.clone(),
            platform: self.platform,
        }
    }

    /// A session's directory, or the refusal for an id that does not name one
    /// inside this store.
    fn session_dir(&self, session_id: &SessionId) -> Result<PathBuf, Refusal> {
        let dir = self.base.join(session_id.as_str());
        if dir.parent() != Some(self.base.as_path()) {
            return Err(Refusal::failed(
                "attachments",
                "the session id does not name a directory inside the attachment base",
            ));
        }
        Ok(dir)
    }

    /// A short, stable name for a file already on this machine, so a viewer is
    /// handed `…/.shortcuts/p7` rather than a path into a session's private
    /// directory.
    ///
    /// It falls back to the full path when no link can be made, because the
    /// alternative is failing an upload that has already been deduplicated.
    async fn shortcut(&self, dir: &Path, path: &Path) -> Result<PathBuf, Refusal> {
        let shortcuts = dir.join(SHORTCUT_DIR_NAME);
        if let Err(error) = tokio::fs::create_dir_all(&shortcuts).await {
            tracing::warn!(%error, "the attachment shortcut directory could not be created");
            return Ok(path.to_path_buf());
        }
        for index in 1..10_000u32 {
            let candidate = shortcuts.join(format!("p{index}"));
            if tokio::fs::symlink(path, &candidate).await.is_ok() {
                return Ok(candidate);
            }
        }
        tracing::warn!("no attachment shortcut name was free");
        Ok(path.to_path_buf())
    }
}

/// The private directory a short-path link is created in.
pub const SHORTCUT_DIR_NAME: &str = ".shortcuts";

/// A filename that may be removed from a session's directory.
fn check_leaf(filename: &str) -> Result<(), Refusal> {
    let refused = |why: &str| Refusal::failed("delete-attachment", why.to_owned());
    if filename.is_empty() {
        return Err(refused("a filename is required"));
    }
    if filename == "." || filename == ".." {
        return Err(refused("a filename is a leaf, not a directory reference"));
    }
    if filename.contains('/') || filename.contains('\\') {
        return Err(refused("a filename may not contain a path separator"));
    }
    if filename.starts_with('.') {
        return Err(refused(
            "a filename may not begin with a dot; that is the worker's own state",
        ));
    }
    if filename.contains('\0') {
        return Err(refused("a filename may not contain a NUL byte"));
    }
    Ok(())
}

impl AttachmentStore for SessionAttachments {
    fn list(&self, session_id: SessionId) -> Boxed<AttachmentOutcome> {
        let store = self.owned();
        Box::pin(async move {
            let dir = store.session_dir(&session_id)?;
            let mut reader = match tokio::fs::read_dir(&dir).await {
                Ok(reader) => reader,
                // A session that has never taken an upload has no directory,
                // and "no attachments" is the truthful answer rather than an
                // error the browse pane has to special-case.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(serde_json::json!({ "entries": [] }));
                }
                Err(error) => {
                    return Err(Refusal::failed(
                        "list-attachments",
                        format!("{}: {error}", dir.display()),
                    ));
                }
            };
            let mut entries = Vec::new();
            while let Some(entry) = reader.next_entry().await.map_err(|error| {
                Refusal::failed("list-attachments", format!("{}: {error}", dir.display()))
            })? {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                // A directory is not an attachment, and a single unreadable
                // entry must not fail a listing the user can otherwise see.
                let Ok(metadata) = entry.metadata().await else {
                    continue;
                };
                if !metadata.is_file() {
                    continue;
                }
                let mtime_ms = metadata
                    .modified()
                    .ok()
                    .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |since| since.as_millis() as u64);
                entries.push(serde_json::json!({
                    "filename": name,
                    "size_bytes": metadata.len(),
                    "mtime_ms": mtime_ms,
                    "abs_path": entry.path().to_string_lossy(),
                }));
            }
            entries.sort_by(|left, right| {
                right["mtime_ms"]
                    .as_u64()
                    .cmp(&left["mtime_ms"].as_u64())
                    .then_with(|| left["filename"].as_str().cmp(&right["filename"].as_str()))
            });
            Ok(serde_json::json!({ "entries": entries }))
        })
    }

    fn delete(&self, session_id: SessionId, filename: String) -> Boxed<AttachmentOutcome> {
        let store = self.owned();
        Box::pin(async move {
            check_leaf(&filename)?;
            let target = store.session_dir(&session_id)?.join(&filename);
            match tokio::fs::remove_file(&target).await {
                Ok(()) => {
                    tracing::info!(
                        session_id = %session_id,
                        filename = %filename,
                        "a browser command deleted an attachment"
                    );
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
        let store = self.owned();
        Box::pin(async move {
            let dir = store.session_dir(&session_id)?;
            let filename = match manifest_lookup(&dir, &sha256).await? {
                Some(filename) => filename,
                None => return Ok(serde_json::json!({ "hit": false, "abs_path": "" })),
            };
            let path = dir.join(&filename);
            if tokio::fs::metadata(&path).await.is_err() {
                // The index names a file this worker no longer holds, which a
                // sweep or a manual deletion can both cause. A miss is what
                // lets the browser upload again.
                return Ok(serde_json::json!({ "hit": false, "abs_path": "" }));
            }
            let reply = if short_path {
                store.shortcut(&dir, &path).await?
            } else {
                path.clone()
            };
            Ok(serde_json::json!({
                "hit": true,
                "abs_path": reply.to_string_lossy(),
            }))
        })
    }
}

/// The filename the session's dedup index holds for a digest.
async fn manifest_lookup(dir: &Path, sha256: &str) -> Result<Option<String>, Refusal> {
    let manifest = dir.join(MANIFEST_NAME);
    let bytes = match tokio::fs::read(&manifest).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(Refusal::failed(
                "attachment-probe",
                format!("{}: {error}", manifest.display()),
            ));
        }
    };
    if bytes.len() as u64 > MANIFEST_MAX_BYTES {
        return Err(Refusal::failed(
            "attachment-probe",
            "the session's dedup index is larger than this worker will read",
        ));
    }
    let index: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Ok(index.get(sha256).and_then(Value::as_str).map(str::to_owned))
}
