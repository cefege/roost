//! The memo of gzip bodies the door's SPA responder compresses on demand (v2
//! `packages/host/src/spa.ts` `gzipCached`): a disk build is mutable, so a body
//! is keyed on its file's path, mtime and size and recompressed when either
//! moves. Owned by `door::spa`, its only reader; the coordinator's front door
//! keeps the same memo in `roost-coord` `http::spa_cache`, which a worker may
//! not import.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use axum::body::Bytes;

/// v2's `GZIP_CACHE_MAX`: the four font faces, the shell and a session's
/// bundles fit, and a 33rd entry is a build nobody is looking at any more.
const CACHE_MAX: usize = 32;

#[derive(Debug)]
struct Entry {
    file: PathBuf,
    state: FileState,
    body: Bytes,
}

/// The memo. A poisoned lock degrades to "no memo": a recomputed body is
/// correct, and a poisoned cache is no reason to 404 a stylesheet.
#[derive(Debug, Default)]
pub(crate) struct SpaCache {
    entries: Mutex<VecDeque<Entry>>,
}

impl SpaCache {
    /// The compressed body for `file` if it is held for this exact file state.
    pub(crate) fn get(&self, file: &Path, state: FileState) -> Option<Bytes> {
        let entries = self.entries.lock().ok()?;
        entries
            .iter()
            .find(|entry| entry.file == file && entry.state == state)
            .map(|entry| entry.body.clone())
    }

    /// Hold `body` for `file`, evicting the oldest entry once the memo is full.
    pub(crate) fn put(&self, file: &Path, state: FileState, body: Bytes) {
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        entries.retain(|entry| entry.file != file);
        if entries.len() >= CACHE_MAX {
            entries.pop_front();
        }
        entries.push_back(Entry {
            file: file.to_path_buf(),
            state,
            body,
        });
    }
}

/// The two cheap facts a memo entry is valid for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileState {
    modified: Option<SystemTime>,
    len: u64,
}

impl FileState {
    /// This file's state, or `None` when it went away after it was resolved.
    pub(crate) fn of(file: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(file).ok()?;
        Some(Self {
            modified: metadata.modified().ok(),
            len: metadata.len(),
        })
    }
}
