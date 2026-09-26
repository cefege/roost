//! The watchers that follow a session's folder on this host: its git branch, its
//! listening ports and its pull request. Keyed by session id and owned here,
//! NOT hung on the session record. Depends on `host::{git_branch,ports,pr_status}`
//! for the readings and on `roost_host` for the platform; nothing here depends
//! back on `session`.
//!
//! v2 hung a `.git/HEAD` watcher, a PR poller and a ports poller on the record
//! as optional closures. A closed session then kept a file handle and three
//! timers alive, and a record — which is logged, snapshotted and printed by
//! diagnostics — carried three closures it had no business carrying. Here the
//! record is plain data and a caller that wants a watcher stopped asks this
//! module, which joins the thread and drops everything it held.
//!
//! THE HEAD WATCHER POLLS INSTEAD OF NOTIFYING. v2 used an OS watcher; this
//! crate takes no notifier dependency, and a branch switch is a write to one
//! small file, so the thread reads that file. The cost is bounded and it is
//! paid per WATCHED SESSION, not per host, and a folder that is not a
//! repository has no file to read at all.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use roost_host::HostPlatform;

use super::git_branch::GitReader;
use super::ports;
use super::pr_status::PrReader;

/// How often a watched `HEAD` is re-read, and so how long a branch switch takes
/// to reach a browser. The file is tens of bytes; the cost is one read.
pub const HEAD_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// How often the slow facts — ports and pull request — are re-read. Ninety
/// seconds because both are expensive relative to a badge: `ss` walks every
/// listening socket on the host and `gh` is a network round trip.
pub const FACTS_POLL_INTERVAL: Duration = Duration::from_secs(90);

/// How long the watcher sleeps between checks of its stop flag. Short enough
/// that closing a session releases its thread promptly.
const STOP_TICK: Duration = Duration::from_millis(25);

/// What a session's folder is currently known to be.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderFacts {
    pub branch: Option<String>,
    pub remote: Option<String>,
    pub ports: Vec<u16>,
    pub pr: Option<super::PrStatus>,
}

/// Where a reading goes. A sink rather than a trait so the registry does not
/// decide the shape of the session event, which `session` owns.
pub type FactsSink = Arc<dyn Fn(&str, &FolderFacts) + Send + Sync>;

/// One watched session: its thread, and the flag that ends it.
struct Watcher {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// The live watchers, keyed by session id.
#[derive(Default)]
pub struct HostWatchers {
    inner: Mutex<HashMap<String, Watcher>>,
    /// Threads started and not yet joined, for a test and for a diagnostic.
    live: Arc<AtomicUsize>,
}

impl std::fmt::Debug for HostWatchers {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostWatchers")
            .field("watched", &self.watched())
            .field("live_threads", &self.live_watchers())
            .finish()
    }
}

impl HostWatchers {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Follow `folder` on behalf of `session_id`, emitting a reading whenever
    /// one changes. `true` when this started a new watcher.
    ///
    /// Re-watching a session that is already watched is refused rather than
    /// restarted: two threads reading the same folder would each emit, and the
    /// second reading would be stale by the time it arrived.
    pub fn watch(
        &self,
        session_id: &str,
        folder: &str,
        root_pid: Option<u32>,
        platform: HostPlatform,
        sink: FactsSink,
    ) -> bool {
        let mut watchers = self.lock();
        if watchers.contains_key(session_id) {
            tracing::debug!(%session_id, "the folder is already being watched");
            return false;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let live = Arc::clone(&self.live);
        let session = session_id.to_string();
        let folder = folder.to_string();
        live.fetch_add(1, Ordering::SeqCst);
        let thread = std::thread::Builder::new()
            .name(format!("roost-folder-{session_id}"))
            .spawn(move || {
                watch_folder(session, folder, root_pid, platform, sink, thread_stop);
                live.fetch_sub(1, Ordering::SeqCst);
            });
        match thread {
            Ok(thread) => {
                watchers.insert(
                    session_id.to_string(),
                    Watcher {
                        stop,
                        thread: Some(thread),
                    },
                );
                tracing::info!(%session_id, %folder, "a session folder is now being watched");
                true
            }
            Err(error) => {
                live.fetch_sub(1, Ordering::SeqCst);
                tracing::error!(%session_id, %error, "a session folder could not be watched");
                false
            }
        }
    }

    /// Stop the watcher for `session_id`, joining its thread. `true` when there
    /// was one.
    ///
    /// The join is the whole contract: a stopped watcher that is merely flagged
    /// is a thread that still holds what it read until it next wakes, and a
    /// closed session that keeps reading is a poll nobody turned off.
    pub fn stop(&self, session_id: &str) -> bool {
        let Some(mut watcher) = self.lock().remove(session_id) else {
            return false;
        };
        watcher.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = watcher.thread.take() {
            let _ = thread.join();
        }
        tracing::info!(%session_id, "a session folder is no longer being watched");
        true
    }

    /// Stop every watcher. Called when the worker itself is shutting down.
    pub fn stop_all(&self) -> usize {
        let sessions: Vec<String> = self.lock().keys().cloned().collect();
        let stopped = sessions.iter().filter(|session| self.stop(session)).count();
        tracing::info!(
            stopped,
            watched = sessions.len(),
            "folder watchers were stopped"
        );
        stopped
    }

    #[must_use]
    pub fn is_watching(&self, session_id: &str) -> bool {
        self.lock().contains_key(session_id)
    }

    /// How many sessions are being watched.
    #[must_use]
    pub fn watched(&self) -> usize {
        self.lock().len()
    }

    /// How many watcher threads have started and not yet exited.
    #[must_use]
    pub fn live_watchers(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Watcher>> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// One reading of everything a folder is known by.
#[must_use]
pub fn read_folder_facts(
    git: &GitReader,
    pr: &PrReader,
    folder: &str,
    root_pid: Option<u32>,
    platform: HostPlatform,
) -> FolderFacts {
    // The branch is read ONCE and used for both facts: `gh pr list --head`
    // needs it, and reading it twice is two process spawns per poll for an
    // answer that cannot differ between the two.
    let branch = git.branch(folder);
    FolderFacts {
        remote: git.remote(folder),
        pr: branch
            .as_deref()
            .and_then(|branch| pr.status(folder, branch)),
        branch,
        ports: ports::read_listening_ports(root_pid, platform),
    }
}

/// The watch loop for one session, until stopped.
fn watch_folder(
    session_id: String,
    folder: String,
    root_pid: Option<u32>,
    platform: HostPlatform,
    sink: FactsSink,
    stop: Arc<AtomicBool>,
) {
    let git = GitReader::from_tool_path(None, platform);
    let pr = PrReader::from_tool_path(None, platform);
    // Resolved once: a repository's HEAD does not move to a different file when
    // a branch is created, so asking git again every tick would be two process
    // spawns per second per session for an answer that cannot change.
    let head = git.head_path(&folder);
    let mut facts = read_folder_facts(&git, &pr, &folder, root_pid, platform);
    let mut head_contents = head
        .as_deref()
        .and_then(|path| std::fs::read_to_string(path).ok());
    sink(&session_id, &facts);

    let mut since_facts = Duration::ZERO;
    while !stop.load(Ordering::SeqCst) {
        if !sleep_until_stop(&stop, HEAD_POLL_INTERVAL) {
            break;
        }
        let branch_moved = match (&head, &head_contents) {
            (Some(path), Some(previous)) => std::fs::read_to_string(path)
                .ok()
                .is_some_and(|current| &current != previous),
            _ => false,
        };
        if branch_moved {
            head_contents = head
                .as_deref()
                .and_then(|path| std::fs::read_to_string(path).ok());
            facts.branch = git.branch(&folder);
        }
        since_facts += HEAD_POLL_INTERVAL;
        if branch_moved || since_facts >= FACTS_POLL_INTERVAL {
            since_facts = Duration::ZERO;
            let next = read_folder_facts(&git, &pr, &folder, root_pid, platform);
            if next != facts {
                tracing::debug!(%session_id, "a watched folder's facts changed");
                facts = next;
            }
        }
        if branch_moved {
            sink(&session_id, &facts);
        }
    }
    tracing::debug!(%session_id, "a folder watcher stopped");
}

/// Sleep, waking early if the watcher is stopped. `false` when it was.
fn sleep_until_stop(stop: &Arc<AtomicBool>, total: Duration) -> bool {
    let deadline = std::time::Instant::now() + total;
    while std::time::Instant::now() < deadline {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        std::thread::sleep(STOP_TICK.min(deadline - std::time::Instant::now()));
    }
    !stop.load(Ordering::SeqCst)
}
