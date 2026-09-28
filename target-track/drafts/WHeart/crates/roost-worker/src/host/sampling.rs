//! The watchers that follow a session's folder on this host — its git branch
//! and remote, its pull request and its listening ports — keyed by session id
//! and owned here, NOT hung on the session record. Ports the polling half of v2
//! `apps/worker/src/session/session-git-ports.ts` and `host/git-branch.ts`
//! `watchGitBranch`; `session::git_ports` is the sink that compares each
//! reading to the record and emits the `git`/`pr`/`ports` events.
//!
//! THE SCHEDULE IS v2's: at start the branch, the remote and the ports are read
//! once; afterwards a branch switch re-reads the branch, and every ninety
//! seconds the pull request and the ports are re-read. A branch or remote the
//! sink says CHANGED re-resolves the pull request at once, as v2's
//! `_resolvePr` after an emit did. The HEAD watcher polls the file instead of
//! subscribing to an OS notifier (no notifier dependency; a branch switch is a
//! write to one small file). Stopping flags the thread and detaches it: a
//! thread inside a `gh` call may run up to the tool timeout, and a close path
//! must not wait on it — its late reading is refused by the sink's record
//! check, as v2's `sessions.get(channelId) !== rec` guard refused it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use roost_host::HostPlatform;

use super::PrStatus;
use super::git_branch::GitReader;
use super::ports;
use super::pr_status::PrReader;
use super::tool_path::process_tool_path;

/// How often a watched `HEAD` is re-read, and so how long a branch switch takes
/// to reach a browser. The file is tens of bytes; the cost is one read.
pub const HEAD_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// v2's `setInterval(.., 90_000)` for both the PR poll and the ports poll: `ss`
/// walks every listening socket on the host and `gh` is a network round trip.
pub const FACTS_POLL_INTERVAL: Duration = Duration::from_secs(90);

/// How long the watcher sleeps between checks of its stop flag.
const STOP_TICK: Duration = Duration::from_millis(25);

/// One reading of one folder fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderReading {
    /// The current branch, `None` for a folder that is not a repository.
    Branch(Option<String>),
    /// The GitHub `owner/repo` of `origin`, `None` when there is none.
    Remote(Option<String>),
    /// The branch's pull request, `None` for every reason there is not one.
    PullRequest(Option<PrStatus>),
    /// The reachable LISTEN ports of the session's process tree, ascending.
    Ports(Vec<u16>),
}

/// Where one session's readings go. The session layer implements it, because
/// the record and the event shape are the session's, not this module's.
pub trait FolderFactsSink: Send + Sync {
    /// Apply one reading; `true` when it changed what the session shows.
    fn apply(&self, reading: FolderReading) -> bool;
    /// The branch a pull request lookup should ask about now, or `None` when
    /// the session is gone, the branch is unknown, or no GitHub remote is known
    /// (v2 `_resolvePr`'s guard).
    fn pull_request_branch(&self) -> Option<String>;
}

/// One watched session: the flag that ends its thread.
struct Watcher {
    stop: Arc<AtomicBool>,
}

/// The live watchers, keyed by session id.
#[derive(Default)]
pub struct HostWatchers {
    inner: Mutex<HashMap<String, Watcher>>,
    /// Threads started and not yet exited, for a test and for a diagnostic.
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

    /// Follow `folder` on behalf of `session_id`. `true` when this started a
    /// new watcher; re-watching a session that is already watched is refused,
    /// because two threads reading one folder would each emit.
    pub fn watch(
        &self,
        session_id: &str,
        folder: &str,
        root_pid: Option<u32>,
        platform: HostPlatform,
        sink: Arc<dyn FolderFactsSink>,
    ) -> bool {
        let mut watchers = self.lock();
        if watchers.contains_key(session_id) {
            tracing::debug!(%session_id, "the folder is already being watched");
            return false;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread_live = Arc::clone(&self.live);
        let unwatched_live = Arc::clone(&self.live);
        let watched_session = session_id.to_string();
        let watched_folder = folder.to_string();
        thread_live.fetch_add(1, Ordering::SeqCst);
        let thread = std::thread::Builder::new()
            .name(format!("roost-folder-{session_id}"))
            .spawn(move || {
                let reader = FolderReader {
                    git: GitReader::system(),
                    pr: PrReader::on_path(process_tool_path(platform)),
                    folder: watched_folder,
                    root_pid,
                    platform,
                    sink,
                    stop: thread_stop,
                };
                reader.follow(&watched_session);
                thread_live.fetch_sub(1, Ordering::SeqCst);
            });
        match thread {
            Ok(_detached) => {
                watchers.insert(session_id.to_string(), Watcher { stop });
                tracing::info!(%session_id, %folder, "a session folder is now being watched");
                true
            }
            Err(error) => {
                unwatched_live.fetch_sub(1, Ordering::SeqCst);
                tracing::error!(%session_id, %error, "a session folder could not be watched");
                false
            }
        }
    }

    /// Stop the watcher for `session_id`. `true` when there was one. The
    /// thread is flagged and detached (see the header).
    pub fn stop(&self, session_id: &str) -> bool {
        let Some(watcher) = self.lock().remove(session_id) else {
            return false;
        };
        watcher.stop.store(true, Ordering::SeqCst);
        tracing::info!(%session_id, "a session folder is no longer being watched");
        true
    }

    /// Stop every watcher. Called when the worker itself is shutting down.
    pub fn stop_all(&self) -> usize {
        let sessions: Vec<String> = self.lock().keys().cloned().collect();
        let stopped = sessions.iter().filter(|session| self.stop(session)).count();
        tracing::info!(stopped, "folder watchers were stopped");
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

/// Everything one watcher thread reads with.
struct FolderReader {
    git: GitReader,
    pr: PrReader,
    folder: String,
    root_pid: Option<u32>,
    platform: HostPlatform,
    sink: Arc<dyn FolderFactsSink>,
    stop: Arc<AtomicBool>,
}

impl FolderReader {
    /// The watch loop for one session, until stopped.
    fn follow(&self, session_id: &str) {
        // v2 `_startGitBranch`: branch, HEAD watch, remote; then `_startPorts`.
        self.apply_branch();
        let head = self.git.head_path(&self.folder);
        let mut head_contents = head
            .as_deref()
            .and_then(|path| std::fs::read_to_string(path).ok());
        if self.stopped() {
            return;
        }
        let remote = self.git.remote(&self.folder);
        if self.sink.apply(FolderReading::Remote(remote)) {
            self.resolve_pull_request();
        }
        self.resolve_ports();
        let mut since_poll = Duration::ZERO;
        while sleep_until_stop(&self.stop, HEAD_POLL_INTERVAL) {
            if let Some(path) = head.as_deref() {
                let current = std::fs::read_to_string(path).ok();
                if current.is_some() && current != head_contents {
                    head_contents = current;
                    self.apply_branch();
                }
            }
            since_poll += HEAD_POLL_INTERVAL;
            if since_poll >= FACTS_POLL_INTERVAL && !self.stopped() {
                since_poll = Duration::ZERO;
                self.resolve_pull_request();
                self.resolve_ports();
            }
        }
        tracing::debug!(%session_id, "a folder watcher stopped");
    }

    /// v2 `readGitBranch(..).then(apply)`: a changed branch re-resolves the PR.
    fn apply_branch(&self) {
        let branch = self.git.branch(&self.folder);
        if !self.stopped() && self.sink.apply(FolderReading::Branch(branch)) {
            self.resolve_pull_request();
        }
    }

    /// v2 `_resolvePr`: only for a known branch in a known GitHub repository.
    fn resolve_pull_request(&self) {
        let Some(branch) = self.sink.pull_request_branch() else {
            return;
        };
        let status = self.pr.status(&self.folder, &branch);
        if !self.stopped() {
            self.sink.apply(FolderReading::PullRequest(status));
        }
    }

    /// v2 `_resolvePorts`.
    fn resolve_ports(&self) {
        let ports = ports::read_listening_ports(self.root_pid, self.platform);
        if !self.stopped() {
            self.sink.apply(FolderReading::Ports(ports));
        }
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
}

/// Sleep, waking early if the watcher is stopped. `false` when it was.
fn sleep_until_stop(stop: &AtomicBool, total: Duration) -> bool {
    let deadline = std::time::Instant::now() + total;
    while std::time::Instant::now() < deadline {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        std::thread::sleep(STOP_TICK.min(deadline - std::time::Instant::now()));
    }
    !stop.load(Ordering::SeqCst)
}
