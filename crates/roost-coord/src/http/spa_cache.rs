//! The memo of compressed SPA bodies, keyed on the file it came from.
//!
//! Owned by `http::spa`, which is the only reader. v2's `gzipCached` and its
//! note — "source-run disk assets remain mutable, so their gzip bodies are
//! memoized and invalidated on an mtime/size mismatch" (`spa.ts:32-51`) — is
//! the reason this exists: a two-day deploy replaces the tree under a running
//! coordinator, and re-compressing four woff2 faces and every bundle on every
//! page load costs the coordinator CPU that the terminal links need.
//!
//! The key is `(path, mtime, size)`, not `path`, because a build served from a
//! working tree is edited in place: a cache keyed on the name alone would serve
//! yesterday's JavaScript from today's filename.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use axum::body::Bytes;

/// How many compressed bodies one process holds. v2's `GZIP_CACHE_MAX`, kept
/// as the same number: the four faces, the shell and a session's bundles fit,
/// and a 33rd entry is a build nobody is looking at any more.
const CACHE_MAX: usize = 32;

/// One memoized body, valid only for the file state it was taken from.
#[derive(Debug)]
struct Entry {
    file: PathBuf,
    modified: Option<std::time::SystemTime>,
    len: u64,
    body: Bytes,
}

/// The process's memo. A poisoned lock degrades to "no memo", never to a
/// refusal: a body that is recomputed is correct, and a poisoned mutex in a
/// cache is not a reason to 404 a stylesheet.
#[derive(Debug, Default)]
pub struct SpaCache {
    entries: Mutex<VecDeque<Entry>>,
}

impl SpaCache {
    /// The compressed body for `file` if it is already held for this exact
    /// file state.
    pub fn get(&self, file: &Path, state: &FileState) -> Option<Bytes> {
        let entries = self.entries.lock().ok()?;
        entries
            .iter()
            .find(|entry| entry.matches(file, state))
            .map(|e| e.body.clone())
    }

    /// Hold `body` for `file`, evicting the oldest entry once the memo is full.
    pub fn put(&self, file: &Path, state: FileState, body: Bytes) {
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        entries.retain(|entry| !entry.matches(file, &state));
        if entries.len() >= CACHE_MAX {
            entries.pop_front();
        }
        entries.push_back(Entry {
            file: file.to_path_buf(),
            modified: state.modified,
            len: state.len,
            body,
        });
    }
}

/// The two cheap facts a cache key is built from, read once per request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileState {
    /// The file's modification time, or `None` when the platform does not
    /// report one — which is a key that still distinguishes size.
    pub modified: Option<std::time::SystemTime>,
    /// The file's length in bytes.
    pub len: u64,
}

impl FileState {
    /// Read this file's state, or `None` when it is gone between the resolve
    /// and this read.
    #[must_use]
    pub fn of(file: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(file).ok()?;
        Some(Self {
            modified: metadata.modified().ok(),
            len: metadata.len(),
        })
    }

    /// A weak validator over this file's length and modification time. A
    /// precompressed sibling is its own file, so it carries its own tag.
    #[must_use]
    pub fn etag(&self) -> String {
        let modified_ms = self
            .modified
            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_millis());
        format!("W/\"{:x}-{modified_ms:x}\"", self.len)
    }
}

impl Entry {
    fn matches(&self, file: &Path, state: &FileState) -> bool {
        self.file == file && self.modified == state.modified && self.len == state.len
    }
}
