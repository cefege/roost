//! Snapshot-scanning file watcher (port of
//! `packages/durable/src/env/node-watch.ts` @ `abe508e1b`).
//!
//! Watches by snapshots: native events (from the `notify` crate) only trigger
//! a debounced rescan, and changes are the difference between snapshots plus
//! the event paths. A replaced file, a renamed or recreated ancestor, or a
//! directory created with its contents therefore never depends on which events
//! an operating system sends. New directories get their watchers before they
//! are scanned again, so nothing written into them before the watcher existed
//! is missed.
//!
//! Rust shape notes (registered V17-15 §7.3): the event source is `notify`
//! instead of Node's `fs.watch`; the debounce/poll/settle timers run in one
//! worker task instead of the calling loop.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use sha2::{Digest, Sha256};

use crate::chord::context::Context;
use crate::env::types::{FileWatcher, WatchChange, WatchMode, WatchTarget};
use crate::errors::{FileError, FileErrorCode};

/// How [`NodeFileWatcher`] watches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeWatchMode {
    /// Changes are reported within about two seconds.
    Native,
    /// The environment compares snapshots.
    Polling,
}

/// Options of [`NodeFileWatcher`].
#[derive(Debug, Clone, Default)]
pub struct NodeWatchOptions {
    /// Force a mode. By default `polling` is chosen on Windows and for file
    /// systems that do not report remote changes.
    pub mode: Option<NodeWatchMode>,
    /// Interval between snapshots in `polling` mode; default 2000 ms.
    pub poll_interval_ms: Option<u64>,
    /// Most directories one watcher covers; default 10,000.
    pub max_directories: Option<usize>,
}

const DEBOUNCE_MS: u64 = 50;
/// On macOS, `fs.watch`/FSEvents returns before the stream is live, and
/// changes in between are never reported; a rescan this long after installing
/// watchers catches them.
const FSEVENTS_SETTLE_MS: u64 = 500;
const DEFAULT_POLL_MS: u64 = 2000;
const DEFAULT_MAX_DIRECTORIES: usize = 10_000;
/// In polling mode, recently modified small files are also compared by
/// content: a second write within the file system's timestamp granularity can
/// keep size and modification time.
const HASH_MAX_BYTES: u64 = 256 * 1024;
const HASH_RECENT_MS: u64 = 5000;

/// Linux `statfs` magic numbers of file systems that accept watches but do not
/// report changes made elsewhere.
#[cfg(any(target_os = "linux", target_os = "android"))]
const UNRELIABLE_FILE_SYSTEMS: &[libc::c_long] = &[
    0x6969,     // NFS
    0x517b,     // SMB
    0xff534d42, // CIFS
    0xfe534d42, // SMB2
    0x65735546, // FUSE (sshfs, Android shared storage)
    0x01021997, // 9P (WSL2 Windows drives)
    0x0bd00bd0, // Lustre
    0x47504653, // GPFS
    0x00c36400, // Ceph
    0x5346414f, // OpenAFS
    0x6b414653, // kAFS
    0x5dca2df5, // sdcardfs
];

struct ResolvedTarget {
    path: String,
    recursive: bool,
    hidden: bool,
    names: BTreeSet<String>,
}

/// What a snapshot remembers of one path. Directories and ancestors are
/// compared by identity only.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    kind: EntryKind,
    dev: u64,
    ino: u64,
    size: u64,
    mtime_ms: u64,
    hash: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

fn same_entry(left: &Entry, right: &Entry) -> bool {
    left == right
}

struct InstalledWatcher {
    /// Held for its `Drop`: dropping the watcher closes the OS watch.
    #[allow(dead_code)]
    watcher: RecommendedWatcher,
    dev: u64,
    ino: u64,
}

struct Scan {
    snapshot: BTreeMap<String, Entry>,
    linked_files: BTreeSet<String>,
}

struct BudgetExceeded;

enum Message {
    Event(String),
    WatcherError,
    Close(tokio::sync::oneshot::Sender<()>),
}

struct Worker {
    targets: Vec<ResolvedTarget>,
    on_change: crate::env::WatchCallback,
    poll_interval: Duration,
    max_directories: usize,
    mode: NodeWatchMode,
    snapshot: BTreeMap<String, Entry>,
    watchers: BTreeMap<String, InstalledWatcher>,
    closed: bool,
    sender: tokio::sync::mpsc::Sender<Message>,
}

/// Snapshot-scanning watcher over the local file system.
pub struct NodeFileWatcher {
    sender: tokio::sync::mpsc::Sender<Message>,
    worker: tokio::task::JoinHandle<()>,
    mode: Arc<AtomicU8>,
}

const MODE_NATIVE: u8 = 0;
const MODE_POLLING: u8 = 1;

impl NodeFileWatcher {
    /// Establish coverage (watchers first, then the snapshot later changes are
    /// compared with) and start the watching worker.
    pub async fn open(
        targets: Vec<WatchTarget>,
        resolve_path: Arc<dyn Fn(&str) -> String + Send + Sync>,
        on_change: crate::env::WatchCallback,
        options: NodeWatchOptions,
    ) -> Result<Self, FileError> {
        let resolved: Vec<ResolvedTarget> = targets
            .into_iter()
            .map(|target| ResolvedTarget {
                path: resolve_path(&target.path),
                recursive: target.recursive,
                hidden: target.exclude.hidden,
                names: target.exclude.names.into_iter().collect(),
            })
            .collect();
        // Windows refuses to rename a directory while another directory below
        // it is open, and a native watcher keeps every watched directory open:
        // watching would break renames of their parents, so Windows polls.
        let mode = match options.mode {
            Some(mode) => mode,
            None => {
                if cfg!(windows) || any_unreliable(&resolved).await {
                    NodeWatchMode::Polling
                } else {
                    NodeWatchMode::Native
                }
            }
        };
        let (sender, receiver) = tokio::sync::mpsc::channel::<Message>(1024);
        let mut worker = Worker {
            targets: resolved,
            on_change,
            poll_interval: Duration::from_millis(
                options.poll_interval_ms.unwrap_or(DEFAULT_POLL_MS),
            ),
            max_directories: options.max_directories.unwrap_or(DEFAULT_MAX_DIRECTORIES),
            mode,
            snapshot: BTreeMap::new(),
            watchers: BTreeMap::new(),
            closed: false,
            sender: sender.clone(),
        };
        // Coverage first: watchers are installed during the initial scan.
        match worker.sync(false).await {
            Ok(_) => {}
            Err(error) => {
                worker.close_watchers();
                return Err(error);
            }
        }
        let mode_flag = Arc::new(AtomicU8::new(match worker.mode {
            NodeWatchMode::Native => MODE_NATIVE,
            NodeWatchMode::Polling => MODE_POLLING,
        }));
        let mode_clone = Arc::clone(&mode_flag);
        let handle = tokio::spawn(async move {
            worker.run(receiver, mode_clone).await;
        });
        Ok(Self {
            sender,
            worker: handle,
            mode: mode_flag,
        })
    }

    /// Current watcher mode.
    pub fn mode(&self) -> NodeWatchMode {
        if self.mode.load(Ordering::SeqCst) == MODE_POLLING {
            NodeWatchMode::Polling
        } else {
            NodeWatchMode::Native
        }
    }

    /// Stop watching; idempotent.
    pub async fn close(&self) {
        let (reply, wait) = tokio::sync::oneshot::channel();
        if self.sender.send(Message::Close(reply)).await.is_err() {
            return;
        }
        let _ = wait.await;
    }
}

#[async_trait::async_trait]
impl FileWatcher for NodeFileWatcher {
    fn mode(&self) -> WatchMode {
        match NodeFileWatcher::mode(self) {
            NodeWatchMode::Native => WatchMode::Native,
            NodeWatchMode::Polling => WatchMode::Polling,
        }
    }

    async fn close(&self, _context: &Context) {
        NodeFileWatcher::close(self).await;
    }
}

impl Drop for NodeFileWatcher {
    fn drop(&mut self) {
        // The worker owns every OS watcher; aborting it closes them.
        self.worker.abort();
    }
}

impl Worker {
    fn deliver(&self, change: WatchChange) {
        let callback = Arc::clone(&self.on_change);
        // A throwing callback must not stop watching.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || callback(change)));
    }

    fn close_watchers(&mut self) {
        self.watchers.clear();
    }

    async fn run(
        mut self,
        mut receiver: tokio::sync::mpsc::Receiver<Message>,
        mode_flag: Arc<AtomicU8>,
    ) {
        let mut events: BTreeSet<String> = BTreeSet::new();
        let mut flush_at: Option<tokio::time::Instant> = None;
        let mut settle_at: Option<tokio::time::Instant> = None;
        let mut poll_at: Option<tokio::time::Instant> = if self.mode == NodeWatchMode::Polling {
            Some(tokio::time::Instant::now() + self.poll_interval)
        } else {
            None
        };
        loop {
            if self.closed {
                break;
            }
            // Update the mode flag when the worker switched to polling.
            mode_flag.store(
                match self.mode {
                    NodeWatchMode::Native => MODE_NATIVE,
                    NodeWatchMode::Polling => MODE_POLLING,
                },
                Ordering::SeqCst,
            );
            if self.mode == NodeWatchMode::Polling && poll_at.is_none() {
                poll_at = Some(tokio::time::Instant::now() + self.poll_interval);
            }
            let deadline = [flush_at, settle_at, poll_at].into_iter().flatten().min();
            let message = match deadline {
                Some(deadline) => tokio::select! {
                    message = receiver.recv() => message,
                    _ = tokio::time::sleep_until(deadline) => {
                        let now = tokio::time::Instant::now();
                        if flush_at.is_some_and(|at| now >= at) {
                            flush_at = None;
                            if self.flush(&mut events, &mut settle_at).await.is_err() {
                                break;
                            }
                        } else if settle_at.is_some_and(|at| now >= at) {
                            settle_at = None;
                            if self.flush(&mut events, &mut settle_at).await.is_err() {
                                break;
                            }
                        } else if poll_at.is_some_and(|at| now >= at) {
                            poll_at = Some(now + self.poll_interval);
                            if self.flush(&mut events, &mut settle_at).await.is_err() {
                                break;
                            }
                        }
                        continue;
                    }
                },
                None => receiver.recv().await,
            };
            match message {
                Some(Message::Event(path)) => {
                    // Events about unrelated siblings of an ancestor, or about
                    // excluded entries, are ignored.
                    if self.in_scope(&path) {
                        events.insert(self.reported(&path));
                    }
                    if flush_at.is_none() {
                        flush_at =
                            Some(tokio::time::Instant::now() + Duration::from_millis(DEBOUNCE_MS));
                    }
                }
                Some(Message::WatcherError) => {
                    // A watched directory that disappears or fails: rescan,
                    // which also replaces the watcher.
                    self.close_watchers();
                    flush_at = Some(tokio::time::Instant::now());
                }
                Some(Message::Close(reply)) => {
                    self.closed = true;
                    self.close_watchers();
                    let _ = reply.send(());
                    break;
                }
                None => break,
            }
        }
        self.close_watchers();
    }

    /// Rescan, report differences, and install watchers for new directories,
    /// rescanning until none are new; returns whether the watcher stopped on a
    /// failure.
    async fn flush(
        &mut self,
        events: &mut BTreeSet<String>,
        settle_at: &mut Option<tokio::time::Instant>,
    ) -> Result<(), ()> {
        let mut reported = std::mem::take(events);
        match self.sync(true).await {
            Ok((changed, added)) => {
                reported.extend(changed);
                if !reported.is_empty() {
                    self.deliver(WatchChange::Paths(reported.into_iter().collect()));
                }
                if added && cfg!(target_os = "macos") {
                    *settle_at = Some(
                        tokio::time::Instant::now() + Duration::from_millis(FSEVENTS_SETTLE_MS),
                    );
                }
                Ok(())
            }
            Err(error) => {
                self.deliver(WatchChange::Error(error));
                self.close_watchers();
                self.closed = true;
                Err(())
            }
        }
    }

    async fn sync(&mut self, report: bool) -> Result<(BTreeSet<String>, bool), FileError> {
        let mut changed = BTreeSet::new();
        let mut added = false;
        let mut report = report;
        for _round in 0..10 {
            if self.closed {
                break;
            }
            let scan = self.scan().await.map_err(classify_scan_error)?;
            if self.closed {
                break;
            }
            if report {
                changed.extend(self.diff(&scan.snapshot));
            }
            let wrote = self.reconcile_watchers(&scan).await?;
            self.snapshot = scan.snapshot;
            added = added || wrote;
            if self.mode == NodeWatchMode::Polling || !wrote {
                break;
            }
            // Something written into a new directory before its watcher
            // existed shows up in the next round.
            report = true;
        }
        Ok((changed, added))
    }

    fn diff(&self, next: &BTreeMap<String, Entry>) -> BTreeSet<String> {
        let mut changed = BTreeSet::new();
        for (path, entry) in next {
            match self.snapshot.get(path) {
                Some(before) if same_entry(before, entry) => {}
                _ => {
                    changed.insert(self.reported(path));
                }
            }
        }
        for path in self.snapshot.keys() {
            if !next.contains_key(path) {
                changed.insert(self.reported(path));
            }
        }
        changed
    }

    /// An ancestor that changed identity moved every target below it; report
    /// those targets.
    fn reported(&self, path: &str) -> String {
        if self
            .targets
            .iter()
            .any(|target| is_within(path, &target.path))
        {
            return path.to_string();
        }
        self.targets
            .iter()
            .find(|target| is_within(&target.path, path))
            .map(|target| target.path.clone())
            .unwrap_or_else(|| path.to_string())
    }

    async fn scan(&self) -> Result<Scan, ScanError> {
        let mut snapshot: BTreeMap<String, Entry> = BTreeMap::new();
        let mut linked_files: BTreeSet<String> = BTreeSet::new();
        let mut counted: BTreeSet<String> = BTreeSet::new();
        let mut traversed: BTreeSet<(usize, String)> = BTreeSet::new();
        let mut listed: BTreeMap<String, EntryKind> = BTreeMap::new();
        for (index, target) in self.targets.iter().enumerate() {
            for ancestor in ancestors_of(&target.path) {
                if snapshot.contains_key(&ancestor) {
                    continue;
                }
                if let Ok(metadata) = tokio::fs::symlink_metadata(&ancestor).await
                    && let Some(kind) = kind_of(&metadata)
                {
                    // Identity only: an ancestor's own timestamps change with
                    // every unrelated sibling.
                    snapshot.insert(
                        ancestor,
                        Entry {
                            kind,
                            dev: dev_of(&metadata),
                            ino: ino_of(&metadata),
                            size: 0,
                            mtime_ms: 0,
                            hash: None,
                        },
                    );
                }
            }
            // The target itself may be a symbolic link to what is watched.
            let stats = match tokio::fs::metadata(&target.path).await {
                Ok(stats) => stats,
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                    return Err(ScanError::Denied(error.to_string()));
                }
                Err(_) => continue,
            };
            if let Some(kind) = kind_of(&stats) {
                let hash = self.hash_if_needed(&target.path, &stats).await;
                snapshot.insert(
                    target.path.clone(),
                    Entry {
                        kind,
                        dev: dev_of(&stats),
                        ino: ino_of(&stats),
                        size: if kind == EntryKind::Directory {
                            0
                        } else {
                            stats.len()
                        },
                        mtime_ms: if kind == EntryKind::Directory {
                            0
                        } else {
                            mtime_ms_of(&stats)
                        },
                        hash,
                    },
                );
            }
            if stats.is_file()
                && tokio::fs::symlink_metadata(&target.path)
                    .await
                    .map(|metadata| metadata.file_type().is_symlink())
                    .unwrap_or(false)
            {
                linked_files.insert(target.path.clone());
            }
            if stats.is_dir() {
                count_directory(&mut counted, &target.path, self.max_directories)?;
                self.scan_directory(
                    index,
                    target,
                    &target.path,
                    &mut snapshot,
                    &mut listed,
                    &mut counted,
                    &mut traversed,
                )
                .await?;
            }
        }
        Ok(Scan {
            snapshot,
            linked_files,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn scan_directory(
        &self,
        index: usize,
        target: &ResolvedTarget,
        directory: &str,
        snapshot: &mut BTreeMap<String, Entry>,
        listed: &mut BTreeMap<String, EntryKind>,
        counted: &mut BTreeSet<String>,
        traversed: &mut BTreeSet<(usize, String)>,
    ) -> Result<(), ScanError> {
        let key = (index, directory.to_string());
        if traversed.contains(&key) {
            return Ok(());
        }
        traversed.insert(key);
        let mut reader = match tokio::fs::read_dir(directory).await {
            Ok(reader) => reader,
            Err(error) => {
                let denied = error.kind() == std::io::ErrorKind::PermissionDenied;
                // The watched directory itself must be readable; below it,
                // unreadable directories are skipped.
                if directory == target.path && denied {
                    return Err(ScanError::Denied(error.to_string()));
                }
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound
                        | std::io::ErrorKind::PermissionDenied
                        | std::io::ErrorKind::NotADirectory
                ) {
                    return Ok(());
                }
                return Err(ScanError::Io(error.to_string()));
            }
        };
        loop {
            let entry = match reader.next_entry().await {
                Ok(entry) => entry,
                Err(error) => {
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound
                            | std::io::ErrorKind::PermissionDenied
                            | std::io::ErrorKind::NotADirectory
                    ) {
                        return Ok(());
                    }
                    return Err(ScanError::Io(error.to_string()));
                }
            };
            let Some(entry) = entry else {
                return Ok(());
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if excluded(target, &name) {
                continue;
            }
            let path = if directory.ends_with(std::path::MAIN_SEPARATOR) {
                format!("{directory}{name}")
            } else {
                format!("{directory}{}{name}", std::path::MAIN_SEPARATOR)
            };
            let kind = match listed.get(&path) {
                Some(kind) => *kind,
                None => {
                    let metadata = match tokio::fs::symlink_metadata(&path).await {
                        Ok(metadata) => metadata,
                        Err(_) => continue,
                    };
                    let Some(kind) = kind_of(&metadata) else {
                        continue;
                    };
                    listed.insert(path.clone(), kind);
                    // A target's own entry (following links) wins over its
                    // listing by another target.
                    if !snapshot.contains_key(&path) {
                        let hash = self.hash_if_needed(&path, &metadata).await;
                        snapshot.insert(
                            path.clone(),
                            Entry {
                                kind,
                                dev: dev_of(&metadata),
                                ino: ino_of(&metadata),
                                size: if kind == EntryKind::Directory {
                                    0
                                } else {
                                    metadata.len()
                                },
                                mtime_ms: if kind == EntryKind::Directory {
                                    0
                                } else {
                                    mtime_ms_of(&metadata)
                                },
                                hash,
                            },
                        );
                    }
                    kind
                }
            };
            if target.recursive && kind == EntryKind::Directory {
                count_directory(counted, &path, self.max_directories)?;
                Box::pin(
                    self.scan_directory(index, target, &path, snapshot, listed, counted, traversed),
                )
                .await?;
            }
        }
    }

    async fn hash_if_needed(&self, path: &str, metadata: &std::fs::Metadata) -> Option<String> {
        if self.mode != NodeWatchMode::Polling {
            return None;
        }
        if !metadata.is_file() || metadata.len() > HASH_MAX_BYTES {
            return None;
        }
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0);
        if now.saturating_sub(mtime) >= HASH_RECENT_MS {
            return None;
        }
        let content = tokio::fs::read(path).await.ok()?;
        let digest = Sha256::digest(&content);
        Some(format!("{digest:x}"))
    }

    /// Watch every existing ancestor of each target, each target directory,
    /// each target that is a symbolic link to a file, and on Linux each
    /// directory below a recursive target. Returns whether a watcher was
    /// added.
    async fn reconcile_watchers(&mut self, scan: &Scan) -> Result<bool, FileError> {
        let mut wanted: BTreeMap<String, bool> = BTreeMap::new();
        for path in &scan.linked_files {
            wanted.insert(path.clone(), false);
        }
        let per_directory = cfg!(any(target_os = "linux", target_os = "android"));
        for target in &self.targets {
            for ancestor in ancestors_of(&target.path) {
                if scan
                    .snapshot
                    .get(&ancestor)
                    .is_some_and(|entry| entry.kind == EntryKind::Directory)
                {
                    wanted.entry(ancestor).or_insert(false);
                }
            }
            if scan.snapshot.get(&target.path).map(|entry| entry.kind) != Some(EntryKind::Directory)
            {
                continue;
            }
            let recursive_root = target.recursive && !per_directory;
            wanted.insert(target.path.clone(), recursive_root);
            if target.recursive && per_directory {
                for (path, entry) in &scan.snapshot {
                    if entry.kind == EntryKind::Directory
                        && path != &target.path
                        && is_within(path, &target.path)
                    {
                        wanted.insert(path.clone(), false);
                    }
                }
            }
        }
        let stale: Vec<String> = self
            .watchers
            .iter()
            .filter(|(path, installed)| {
                let entry = scan.snapshot.get(*path);
                !wanted.contains_key(*path)
                    || entry.is_none()
                    || entry.is_some_and(|entry| {
                        entry.dev != installed.dev || entry.ino != installed.ino
                    })
            })
            .map(|(path, _)| path.clone())
            .collect();
        for path in stale {
            self.watchers.remove(&path);
        }
        let mut added = false;
        for (path, recursive) in wanted {
            if self.watchers.contains_key(&path) {
                continue;
            }
            let Some(entry) = scan.snapshot.get(&path) else {
                continue;
            };
            let linked = scan.linked_files.contains(&path);
            let sender = self.sender.clone();
            // The file a target links to changed: report the target (upstream
            // `#onLinkedFileEvent`).
            let linked_path = linked.then(|| path.clone());
            let mut watcher =
                match notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
                    match result {
                        Ok(event) => {
                            if let Some(link) = &linked_path {
                                let _ = sender.try_send(Message::Event(link.clone()));
                                return;
                            }
                            for path in event.paths {
                                let _ = sender
                                    .try_send(Message::Event(path.to_string_lossy().into_owned()));
                            }
                        }
                        Err(_) => {
                            let _ = sender.try_send(Message::WatcherError);
                        }
                    }
                }) {
                    Ok(watcher) => watcher,
                    Err(_) => {
                        self.switch_to_polling();
                        return Ok(false);
                    }
                };
            let mode = if recursive {
                RecursiveMode::Recursive
            } else {
                RecursiveMode::NonRecursive
            };
            let watch_path = if linked {
                tokio::fs::canonicalize(&path)
                    .await
                    .unwrap_or_else(|_| PathBuf::from(&path))
            } else {
                PathBuf::from(&path)
            };
            match watcher.watch(&watch_path, mode) {
                Ok(()) => {
                    self.watchers.insert(
                        path,
                        InstalledWatcher {
                            watcher,
                            dev: entry.dev,
                            ino: entry.ino,
                        },
                    );
                    added = true;
                }
                Err(error) => {
                    if is_skippable_watch_error(&error) {
                        continue;
                    }
                    // Out of watches or unsupported: compare snapshots from
                    // now on, and say coverage was uncertain.
                    self.switch_to_polling();
                    return Ok(false);
                }
            }
        }
        Ok(added)
    }

    /// Whether a changed path is inside a watched target (or an ancestor of
    /// one that a directory-level event may concern).
    fn in_scope(&self, path: &str) -> bool {
        for target in &self.targets {
            // The path is at or above the target: creating or renaming an
            // ancestor moves the target.
            if is_within(&target.path, path) {
                return true;
            }
            if path == target.path || !is_within(path, &target.path) {
                continue;
            }
            let Ok(relative) = Path::new(path).strip_prefix(&target.path) else {
                continue;
            };
            let components: Vec<&std::ffi::OsStr> = relative.iter().collect();
            if !target.recursive && components.len() > 1 {
                continue;
            }
            if components.iter().any(|component| {
                component
                    .to_str()
                    .map(|name| excluded(target, name))
                    .unwrap_or(false)
            }) {
                continue;
            }
            return true;
        }
        false
    }

    fn switch_to_polling(&mut self) {
        if self.mode == NodeWatchMode::Polling {
            return;
        }
        self.mode = NodeWatchMode::Polling;
        self.close_watchers();
        self.deliver(WatchChange::Overflow);
    }
}

enum ScanError {
    Budget(BudgetExceeded),
    Denied(String),
    Io(String),
}

fn classify_scan_error(error: ScanError) -> FileError {
    match error {
        ScanError::Budget(_) => FileError::new(
            FileErrorCode::Invalid,
            "Watched paths exceed the directory budget".to_string(),
        ),
        ScanError::Denied(message) => FileError::new(FileErrorCode::PermissionDenied, message),
        ScanError::Io(message) => FileError::new(FileErrorCode::Invalid, message),
    }
}

fn count_directory(
    counted: &mut BTreeSet<String>,
    path: &str,
    max_directories: usize,
) -> Result<(), ScanError> {
    counted.insert(path.to_string());
    if counted.len() > max_directories {
        return Err(ScanError::Budget(BudgetExceeded));
    }
    Ok(())
}

fn kind_of(metadata: &std::fs::Metadata) -> Option<EntryKind> {
    if metadata.is_file() {
        Some(EntryKind::File)
    } else if metadata.is_dir() {
        Some(EntryKind::Directory)
    } else if metadata.file_type().is_symlink() {
        Some(EntryKind::Symlink)
    } else {
        Some(EntryKind::Other)
    }
}

#[cfg(unix)]
fn dev_of(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.dev()
}

// ROOST PATCH: stable Rust has no Windows volume/file-index accessor
// (`windows_by_handle` is unstable), so Windows reports no device or inode
// identity, like other non-Unix targets; change detection falls back to
// kind, size and mtime.
#[cfg(not(unix))]
fn dev_of(_metadata: &std::fs::Metadata) -> u64 {
    0
}

#[cfg(unix)]
fn ino_of(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.ino()
}

#[cfg(not(unix))]
fn ino_of(_metadata: &std::fs::Metadata) -> u64 {
    0
}

fn mtime_ms_of(metadata: &std::fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Ancestors of `path`, nearest first, up to the root.
fn ancestors_of(path: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = Path::new(path).parent().map(Path::to_path_buf);
    while let Some(parent) = current {
        result.push(parent.to_string_lossy().into_owned());
        let next = parent.parent().map(Path::to_path_buf);
        if next.as_deref() == Some(parent.as_path()) {
            break;
        }
        current = next;
    }
    result
}

/// Whether `path` is `ancestor` or below it.
fn is_within(path: &str, ancestor: &str) -> bool {
    if path == ancestor {
        return true;
    }
    let separator = std::path::MAIN_SEPARATOR;
    let prefix = if ancestor.ends_with(separator) {
        ancestor.to_string()
    } else {
        format!("{ancestor}{separator}")
    };
    let alternate = if separator == '\\' { '/' } else { '\\' };
    let prefix_alt = prefix.replace(separator, &alternate.to_string());
    path.starts_with(&prefix) || path.starts_with(&prefix_alt)
}

fn excluded(target: &ResolvedTarget, name: &str) -> bool {
    (target.hidden && name.starts_with('.')) || target.names.contains(name)
}

fn is_skippable_watch_error(error: &notify::Error) -> bool {
    match &error.kind {
        notify::ErrorKind::PathNotFound | notify::ErrorKind::WatchNotFound => true,
        notify::ErrorKind::Io(io) => matches!(
            io.kind(),
            std::io::ErrorKind::NotFound
                | std::io::ErrorKind::PermissionDenied
                | std::io::ErrorKind::NotADirectory
        ),
        _ => false,
    }
}

/// Whether any path, or its nearest existing ancestor, is on a file system
/// that does not report remote changes.
async fn any_unreliable(targets: &[ResolvedTarget]) -> bool {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        for target in targets {
            for candidate in std::iter::once(target.path.clone()).chain(ancestors_of(&target.path))
            {
                let Ok(info) = statfs_type(&candidate).await else {
                    continue;
                };
                if UNRELIABLE_FILE_SYSTEMS.contains(&info) {
                    return true;
                }
                break;
            }
        }
        false
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let _ = targets;
        false
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
async fn statfs_type(path: &str) -> Result<libc::c_long, ()> {
    use std::ffi::CString;
    let Ok(pointer) = CString::new(path) else {
        return Err(());
    };
    let path = pointer;
    tokio::task::spawn_blocking(move || {
        let mut info: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: `statfs` writes into the provided buffer; the path is a
        // valid NUL-terminated string for the call's duration.
        let result = unsafe { libc::statfs(path.as_ptr(), &mut info) };
        if result == 0 {
            Ok(info.f_type)
        } else {
            Err(())
        }
    })
    .await
    .unwrap_or(Err(()))
}
