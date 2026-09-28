//! Detects which coding agents are actually running by combining a process
//! scan with manifest evaluation over each session's visible grid and OSC
//! evidence, so a pane showing agent activity maps to a concrete
//! `BuiltinAgentId` even when no integration reported in. Ports v2
//! `apps/worker/src/agents/detector.ts` (`AgentScreenDetector`). Built by
//! `agents::status_stack`; fed by the session layer's terminal-changed and
//! session-closed hooks; `agents::report_server` and `agents::prompt_control`
//! call [`AgentScreenDetector::reporting_agent_for_session`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use roost_observability::clock::EventClock;
use roost_protocol::wire::brand::SessionId;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::BuiltinAgentId;
use super::environment::AgentReportEnvironment;
use super::manifests::AgentManifests;
use super::process_scan::{AgentProcessIdentity, AgentProcessScan, SessionProcessRoot};
use super::process_snapshot::ScanAbort;
use super::registry::AgentStatusRegistry;
use super::stable_detection::StableScreenDetector;
use crate::uplink::OwnerFuture;
use sessions::AgentSessionSource;

mod reference;
mod scan;
pub mod sessions;

pub use reference::AgentReferenceClearDeps;

/// v2 `PROCESS_SCAN_INTERVAL_MS`.
pub const PROCESS_SCAN_INTERVAL: Duration = Duration::from_millis(250);
/// v2 `OUTPUT_SCAN_COALESCE_MS`.
pub const OUTPUT_SCAN_COALESCE: Duration = Duration::from_millis(40);
/// v2 `SCREEN_RESCAN_MIN_MS`: minimum gap between visible-grid reads for one
/// session. The grid read is the expensive part of a scan, and output bursts
/// arm scans every coalesce interval — ungated, a fleet of chatty sessions
/// starves PTY parsing and baseline emission.
pub const SCREEN_RESCAN_MIN_MS: i64 = 200;

/// Everything a detector reads and drives.
pub struct AgentScreenDetectorDeps {
    pub sessions: Arc<dyn AgentSessionSource>,
    pub registry: Arc<AgentStatusRegistry>,
    pub scanner: Arc<dyn AgentProcessScan>,
    pub manifests: Arc<AgentManifests>,
    pub environment: Arc<AgentReportEnvironment>,
    /// Monotonic time gates grid reads; wall time stamps screen stability.
    pub clock: Arc<dyn EventClock>,
    pub reference_clear: Option<AgentReferenceClearDeps>,
    pub runtime: tokio::runtime::Handle,
}

impl std::fmt::Debug for AgentScreenDetectorDeps {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentScreenDetectorDeps")
            .finish_non_exhaustive()
    }
}

/// The identity a session's last scan resolved (v2 `ObservedAgentIdentity`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObservedAgentIdentity {
    agent_id: BuiltinAgentId,
    process_id: u32,
}

#[derive(Default)]
struct DetectorState {
    output_timers: HashMap<u16, JoinHandle<()>>,
    last_screen_read_at: HashMap<SessionId, i64>,
    last_observed: HashMap<SessionId, ObservedAgentIdentity>,
    channel_by_session: HashMap<SessionId, u16>,
    running: Option<u64>,
    next_scan: u64,
    rerun: bool,
    disposed: bool,
    interval: Option<JoinHandle<()>>,
}

struct DetectorInner {
    deps: AgentScreenDetectorDeps,
    stable: Mutex<StableScreenDetector>,
    state: Mutex<DetectorState>,
    completed: watch::Sender<u64>,
}

/// v2 `AgentScreenDetector`. Cheap to clone; clones share one detector.
#[derive(Clone)]
pub struct AgentScreenDetector {
    inner: Arc<DetectorInner>,
}

impl std::fmt::Debug for AgentScreenDetector {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentScreenDetector")
            .finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl AgentScreenDetector {
    /// A detector with no timers running; [`AgentScreenDetector::start`] arms
    /// v2's constructor behaviour (an immediate scan, then one per interval).
    pub fn new(deps: AgentScreenDetectorDeps) -> Self {
        let (completed, _) = watch::channel(0);
        Self {
            inner: Arc::new(DetectorInner {
                deps,
                stable: Mutex::new(StableScreenDetector::new()),
                state: Mutex::new(DetectorState::default()),
                completed,
            }),
        }
    }

    /// Scan now and every [`PROCESS_SCAN_INTERVAL`] until disposed.
    pub fn start(&self) {
        let detector = Arc::downgrade(&self.inner);
        let task = self.inner.deps.runtime.spawn(async move {
            let mut ticker = tokio::time::interval(PROCESS_SCAN_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let Some(inner) = detector.upgrade() else {
                    return;
                };
                drop(AgentScreenDetector { inner }.scan_now());
            }
        });
        let mut state = lock(&self.inner.state);
        if let Some(previous) = state.interval.replace(task) {
            previous.abort();
        }
        tracing::info!("agent screen detection started");
    }

    /// v2 `schedule`: a channel produced output; scan once it settles.
    pub fn schedule(&self, channel_id: u16) {
        let mut state = lock(&self.inner.state);
        if state.disposed || state.output_timers.contains_key(&channel_id) {
            return;
        }
        let detector = self.clone();
        let timer = self.inner.deps.runtime.spawn(async move {
            tokio::time::sleep(OUTPUT_SCAN_COALESCE).await;
            lock(&detector.inner.state)
                .output_timers
                .remove(&channel_id);
            detector.scan_now().await;
        });
        state.output_timers.insert(channel_id, timer);
    }

    /// v2 `scanNow`: join the scan in flight (asking for one more after it),
    /// or start one. The scan itself runs on the runtime, so dropping the
    /// returned future never cancels it.
    pub fn scan_now(&self) -> OwnerFuture<()> {
        let mut state = lock(&self.inner.state);
        if state.disposed {
            return Box::pin(async {});
        }
        let awaited = match state.running {
            Some(running) => {
                state.rerun = true;
                running
            }
            None => self.start_scan(&mut state),
        };
        drop(state);
        let mut completed = self.inner.completed.subscribe();
        Box::pin(async move {
            let _ = completed.wait_for(|done| *done >= awaited).await;
        })
    }

    fn start_scan(&self, state: &mut DetectorState) -> u64 {
        state.next_scan += 1;
        let scan = state.next_scan;
        state.running = Some(scan);
        let detector = self.clone();
        self.inner.deps.runtime.spawn(async move {
            detector.scan_once().await;
            {
                // The follow-up scan starts under the same lock that clears
                // this one, so no caller can slip a second scan in between.
                let mut state = lock(&detector.inner.state);
                state.running = None;
                if std::mem::take(&mut state.rerun) && !state.disposed {
                    detector.start_scan(&mut state);
                }
            }
            detector
                .inner
                .completed
                .send_modify(|done| *done = (*done).max(scan));
        });
        scan
    }

    /// v2 `reportingAgentForSession`: the reporter's identity, proved by a
    /// forced scan, and only while the session still runs the same child.
    pub fn reporting_agent_for_session(
        &self,
        session_id: &SessionId,
        reporter_pid: u32,
        abort: Option<ScanAbort>,
    ) -> OwnerFuture<Option<AgentProcessIdentity>> {
        let sessions = Arc::clone(&self.inner.deps.sessions);
        let scanner = Arc::clone(&self.inner.deps.scanner);
        let session_id = session_id.clone();
        Box::pin(async move {
            let child_pid = sessions
                .session(&session_id)
                .and_then(|record| record.child_pid)
                .filter(|pid| *pid > 0)?;
            let root = SessionProcessRoot {
                session_id: session_id.clone(),
                child_pid,
            };
            let identity = scanner
                .scan_reporting_agent(root, reporter_pid, abort)
                .await;
            let current = sessions
                .session(&session_id)
                .and_then(|record| record.child_pid);
            if current == Some(child_pid) {
                identity
            } else {
                None
            }
        })
    }

    /// v2 `closeSession`: cancel a coalesce timer that could only scan a
    /// session that is gone, and drop everything held for it.
    pub fn close_session(&self, session_id: &SessionId) {
        let deps = &self.inner.deps;
        let resolved = deps
            .sessions
            .session(session_id)
            .map(|record| record.channel_id);
        {
            let mut state = lock(&self.inner.state);
            let channel_id = resolved.or_else(|| state.channel_by_session.get(session_id).copied());
            if let Some(timer) =
                channel_id.and_then(|channel_id| state.output_timers.remove(&channel_id))
            {
                timer.abort();
            }
            state.last_screen_read_at.remove(session_id);
            state.last_observed.remove(session_id);
            state.channel_by_session.remove(session_id);
        }
        // Respawns mint fresh session ids forever, so a cached capability for
        // a closed session can never be read again — pure memory growth.
        let released = deps
            .environment
            .release_agent_status_capabilities(session_id);
        lock(&self.inner.stable).release(session_id);
        deps.registry.close_session(session_id);
        tracing::debug!(session_id = %session_id, released, "agent detection forgot a closed session");
    }

    /// v2 `dispose`.
    pub fn dispose(&self) {
        let mut state = lock(&self.inner.state);
        state.disposed = true;
        if let Some(interval) = state.interval.take() {
            interval.abort();
        }
        for (_, timer) in state.output_timers.drain() {
            timer.abort();
        }
        state.last_screen_read_at.clear();
        state.last_observed.clear();
        tracing::info!("agent screen detection stopped");
    }
}
