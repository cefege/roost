//! Where a finished upload lands: its final name, the short path handed back
//! for it, the flushes that make it durable, and the session's dedup manifest.
//! Ports v2 `apps/worker/src/attachments/attachment-file-store.ts`. Called by
//! the operation owner once its journal is ready, and by the browser
//! `attachment-probe` command, which reads the manifest without owning uploads.

use std::collections::HashSet;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use super::file_hash::sha256_attachment_file;
use super::naming::{node_extname, sanitize_attachment_name};
use super::store_paths::{AttachmentBase, MANIFEST_NAME, SHORTCUT_DIR_NAME, join_lexically};

/// The last numbered suffix tried before a clock-stamped one.
const UNIQUE_SUFFIX_LIMIT: u32 = 10_000;

/// The name an upload will occupy, reserved but not yet written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentDestination {
    pub file_name: String,
    pub file_path: PathBuf,
}

/// Where the bytes now are, and whether they were already there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedAttachmentDestination {
    pub file_path: PathBuf,
    /// The final name was already occupied by identical bytes (a recovered
    /// commit whose rename had happened), so nothing was moved.
    pub verified_existing_file: bool,
}

/// The `attachment-probe` answer: the stored path of bytes this session holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentProbe {
    pub hit: bool,
    pub abs_path: String,
}

impl AttachmentProbe {
    fn miss() -> Self {
        Self {
            hit: false,
            abs_path: String::new(),
        }
    }
}

/// Sanitize the client's name and pick the first free variant of it:
/// `name.ext`, then `name (2).ext`, `name (3).ext`, ….
pub fn reserve_attachment_destination(dir: &Path, filename: &str) -> AttachmentDestination {
    let file_name = unique_name(dir, &sanitize_attachment_name(filename));
    AttachmentDestination {
        file_path: dir.join(&file_name),
        file_name,
    }
}

/// Occupy the reserved name with the temp's bytes. An occupant with the same
/// digest is this upload's own earlier rename; one with other bytes is refused
/// rather than overwritten.
pub fn place_attachment_destination(
    dir: &Path,
    temp_path: &Path,
    file_name: &str,
    sha256: &str,
) -> io::Result<CommittedAttachmentDestination> {
    let file_path = dir.join(file_name);
    let final_already_exists = file_path.exists();
    if final_already_exists && sha256_attachment_file(&file_path)? != sha256 {
        return Err(io::Error::other("attachment destination hash mismatch"));
    }
    if !final_already_exists {
        fs::rename(temp_path, &file_path)?;
    }
    Ok(CommittedAttachmentDestination {
        file_path,
        verified_existing_file: final_already_exists,
    })
}

/// The synchronous commit crash recovery uses; a live upload flushes
/// asynchronously instead.
pub fn commit_attachment_destination(
    dir: &Path,
    temp_path: &Path,
    file_name: &str,
    sha256: &str,
) -> io::Result<CommittedAttachmentDestination> {
    let placed = place_attachment_destination(dir, temp_path, file_name, sha256)?;
    File::open(&placed.file_path)?.sync_all()?;
    sync_attachment_directory(dir)?;
    record_attachment_hash(dir, sha256, file_name);
    Ok(placed)
}

pub async fn sync_attachment_file_async(path: &Path) -> io::Result<()> {
    tokio::fs::File::open(path).await?.sync_all().await
}

/// Flush a rename's directory entry. Failures propagate, so no receipt can
/// outrun the disk.
pub fn sync_attachment_directory(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

pub async fn sync_attachment_directory_async(dir: &Path) -> io::Result<()> {
    tokio::fs::File::open(dir).await?.sync_all().await
}

/// The path a receipt names: a `.shortcuts/pN` link when the client asked for
/// a short one, else the file itself.
pub fn attachment_reply_path(dir: &Path, file_path: &Path, short_path: bool) -> PathBuf {
    if short_path {
        return short_attachment_path(dir, file_path);
    }
    file_path.to_path_buf()
}

/// Point `sha256` at `file_name`, dropping any older digest that named the same
/// file so a re-upload under one name never leaves a stale hit.
pub fn record_attachment_hash(dir: &Path, sha256: &str, file_name: &str) {
    let mut manifest = load_manifest(dir);
    manifest.retain(|_, recorded| recorded.as_str() != Some(file_name));
    manifest.insert(sha256.to_owned(), Value::String(file_name.to_owned()));
    save_manifest(dir, &manifest);
}

/// Whether this session already holds bytes with this digest, answered from
/// the manifest: nothing a caller sends chooses which path is read.
pub fn probe_attachment(
    base: &AttachmentBase,
    session_id: &str,
    sha256: &str,
    short_path: bool,
) -> AttachmentProbe {
    let Some(dir) = base.resolve_session_dir(session_id) else {
        return AttachmentProbe::miss();
    };
    let manifest = load_manifest(&dir);
    let Some(file_name) = manifest
        .get(sha256)
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
    else {
        return AttachmentProbe::miss();
    };
    let file_path = join_lexically(&dir, file_name);
    // A sweep or a manual deletion can leave the manifest naming a file this
    // worker no longer holds; a miss is what lets the browser upload again.
    if !file_path.exists() {
        return AttachmentProbe::miss();
    }
    let reply_path = attachment_reply_path(&dir, &file_path, short_path);
    AttachmentProbe {
        hit: true,
        abs_path: reply_path.to_string_lossy().into_owned(),
    }
}

/// `fs.writeFileSync(path, bytes, { mode: 0o600 })`: created owner-only,
/// truncated if it exists.
pub fn write_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?
        .write_all(bytes)
}

/// `fs.mkdirSync(dir, { recursive: true, mode: 0o700 })`.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

fn unique_name(dir: &Path, sanitized: &str) -> String {
    if !dir.join(sanitized).exists() {
        return sanitized.to_owned();
    }
    let extension = node_extname(sanitized);
    let stem = &sanitized[..sanitized.len() - extension.len()];
    for index in 2..UNIQUE_SUFFIX_LIMIT {
        let candidate = format!("{stem} ({index}){extension}");
        if !dir.join(&candidate).exists() {
            return candidate;
        }
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis());
    format!("{stem} ({stamp}){extension}")
}

/// A fresh `.shortcuts/pN` link to the file, the lowest free `N`; the full path
/// when no link can be made, because failing an upload whose bytes are already
/// stored is worse than answering with a long path.
fn short_attachment_path(dir: &Path, file_path: &Path) -> PathBuf {
    match link_shortcut(dir, file_path) {
        Ok(shortcut) => shortcut,
        Err(error) => {
            tracing::warn!(%error, dir = %dir.display(), "short_attachment_path_failed: answering with the full path");
            file_path.to_path_buf()
        }
    }
}

fn link_shortcut(dir: &Path, file_path: &Path) -> io::Result<PathBuf> {
    let shortcuts = dir.join(SHORTCUT_DIR_NAME);
    create_private_dir(&shortcuts)?;
    let mut occupied = HashSet::new();
    for entry in fs::read_dir(&shortcuts)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if is_shortcut_name(&name) {
            occupied.insert(name);
        }
    }
    let mut index: u64 = 1;
    while occupied.contains(&format!("p{index}")) {
        index += 1;
    }
    let shortcut = shortcuts.join(format!("p{index}"));
    std::os::unix::fs::symlink(file_path, &shortcut)?;
    Ok(shortcut)
}

/// v2's `/^p\d+$/`.
fn is_shortcut_name(name: &str) -> bool {
    name.strip_prefix('p').is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}

/// The manifest, or an empty one: an unreadable or corrupt index only costs a
/// re-upload, never a failed one.
fn load_manifest(dir: &Path) -> Map<String, Value> {
    fs::read(dir.join(MANIFEST_NAME))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| match value {
            Value::Object(manifest) => Some(manifest),
            _ => None,
        })
        .unwrap_or_default()
}

fn save_manifest(dir: &Path, manifest: &Map<String, Value>) {
    let written = serde_json::to_vec(manifest)
        .map_err(io::Error::other)
        .and_then(|bytes| write_private_file(&dir.join(MANIFEST_NAME), &bytes));
    if let Err(error) = written {
        tracing::warn!(%error, dir = %dir.display(), "manifest_write_failed");
    }
}
