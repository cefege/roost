//! Which agent process each session is running, from a throttled, shared
//! process-table snapshot: the scan the detector runs every tick, and the
//! forced re-scan that proves a reporter is the incumbent agent. Ports the
//! scanner half of v2 `apps/worker/src/agents/process-scan.ts`
//! (`AgentProcessScanner`). Called by `agents::detector`; its identities reach
//! `agents::registry`, `agents::report_server` and `agents::prompt_control`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use roost_protocol::wire::brand::SessionId;
use tokio::sync::watch;

use super::BuiltinAgentId;
use super::process_identity::{find_agent_process_identity, find_exact_agent_process_identity};
use super::process_snapshot::{ProcessSnapshotReader, ScanAbort};
use super::process_tree::{AgentForegroundJob, ProcessRecord};
use crate::uplink::OwnerFuture;

/// v2 `SCAN_THROTTLE_MS`: a snapshot younger than this answers a routine scan.
pub const SCAN_THROTTLE: Duration = Duration::from_millis(250);

/// An agent process a session was proved to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProcessIdentity {
    pub agent_id: BuiltinAgentId,
    pub pid: u32,
    /// Foreground job of the pane the identity was proved in. Present only
    /// when a live snapshot row proved this exact pid; a held or reported
    /// identity carries no such proof, which a prompt admission must treat as
    /// unproved.
    pub foreground: Option<AgentForegroundJob>,
}

/// A session and the pid its PTY child runs as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionProcessRoot {
    pub session_id: SessionId,
    pub child_pid: u32,
}

/// The scans the detector drives; [`AgentProcessScanner`] is the production
/// one and a detector test scripts its own.
pub trait AgentProcessScan: Send + Sync {
    fn scan_agents(
        &self,
        roots: Vec<SessionProcessRoot>,
    ) -> OwnerFuture<HashMap<SessionId, AgentProcessIdentity>>;

    fn scan_reporting_agent(
        &self,
        root: SessionProcessRoot,
        reporter_pid: u32,
        abort: Option<ScanAbort>,
    ) -> OwnerFuture<Option<AgentProcessIdentity>>;
}

/// An identity a session keeps through one missed snapshot.
#[derive(Debug, Clone)]
struct HeldIdentity {
    identity: AgentProcessIdentity,
    misses: u32,
}

/// The snapshot in flight, shared by every caller that joins it.
#[derive(Debug, Clone)]
struct InFlight {
    id: u64,
    done: watch::Receiver<Option<bool>>,
    abort: ScanAbort,
}

#[derive(Debug, Default)]
struct ScannerState {
    records: Vec<ProcessRecord>,
    scanned_at: Option<Instant>,
    in_flight: Option<InFlight>,
    next_scan: u64,
    held_by_session: HashMap<SessionId, HeldIdentity>,
}

/// v2 `AgentProcessScanner`. Cheap to clone; clones share one snapshot.
#[derive(Clone)]
pub struct AgentProcessScanner {
    reader: Arc<dyn ProcessSnapshotReader>,
    throttle: Duration,
    runtime: tokio::runtime::Handle,
    state: Arc<Mutex<ScannerState>>,
}

impl std::fmt::Debug for AgentProcessScanner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentProcessScanner")
            .field("throttle", &self.throttle)
            .finish_non_exhaustive()
    }
}

impl AgentProcessScanner {
    /// A scanner whose snapshots run on `runtime`, so a snapshot outlives the
    /// caller that started it exactly as v2's shared promise does.
    pub fn new(
        reader: Arc<dyn ProcessSnapshotReader>,
        throttle: Duration,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            reader,
            throttle,
            runtime,
            state: Arc::new(Mutex::new(ScannerState::default())),
        }
    }

    fn lock(&self) -> MutexGuard<'_, ScannerState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Wait for `scan`, or give it up when `abort` fires first — which also
    /// kills it if it is still the current snapshot (v2 `awaitScan`).
    async fn await_scan(&self, scan: InFlight, abort: Option<ScanAbort>) -> bool {
        let mut done = scan.done.clone();
        let finished = async move {
            loop {
                if let Some(refreshed) = *done.borrow_and_update() {
                    return refreshed;
                }
                if done.changed().await.is_err() {
                    return false;
                }
            }
        };
        let Some(abort) = abort else {
            return finished.await;
        };
        if abort.is_aborted() {
            self.abort_current(&scan);
            return false;
        }
        tokio::select! {
            biased;
            refreshed = finished => refreshed,
            () = abort.aborted() => {
                self.abort_current(&scan);
                false
            }
        }
    }

    fn abort_current(&self, scan: &InFlight) {
        let mut state = self.lock();
        if state
            .in_flight
            .as_ref()
            .is_some_and(|current| current.id == scan.id)
        {
            state.in_flight = None;
            drop(state);
            scan.abort.abort();
            tracing::debug!(
                scan = scan.id,
                "the process snapshot in flight was abandoned"
            );
        }
    }

    /// v2 `refresh`: reuse a young snapshot, join the one in flight, or start
    /// one. `false` means no fresh snapshot backs the answer.
    async fn refresh(&self, force: bool, abort: Option<ScanAbort>) -> bool {
        if abort.as_ref().is_some_and(ScanAbort::is_aborted) {
            return false;
        }
        let scan = {
            let mut state = self.lock();
            let young = state
                .scanned_at
                .is_some_and(|scanned_at| scanned_at.elapsed() < self.throttle);
            if !force && young {
                return true;
            }
            match state.in_flight.clone() {
                Some(scan) => scan,
                None => self.start_scan(&mut state),
            }
        };
        self.await_scan(scan, abort).await
    }

    fn start_scan(&self, state: &mut ScannerState) -> InFlight {
        state.next_scan += 1;
        let (sender, done) = watch::channel(None);
        let scan = InFlight {
            id: state.next_scan,
            done,
            abort: ScanAbort::new(),
        };
        state.in_flight = Some(scan.clone());
        let this = self.clone();
        let (id, abort) = (scan.id, scan.abort.clone());
        self.runtime.spawn(async move {
            let read = this.reader.read(abort.clone());
            let result = tokio::select! {
                biased;
                () = abort.aborted() => Err(super::process_snapshot::SNAPSHOT_ABORTED.to_owned()),
                result = read => result,
            };
            let refreshed = this.finish_scan(id, &abort, result);
            let _ = sender.send(Some(refreshed));
        });
        scan
    }

    fn finish_scan(
        &self,
        id: u64,
        abort: &ScanAbort,
        result: Result<Vec<ProcessRecord>, String>,
    ) -> bool {
        let mut state = self.lock();
        let refreshed = match result {
            _ if abort.is_aborted() => false,
            Ok(records) => {
                state.records = records;
                state.scanned_at = Some(Instant::now());
                true
            }
            Err(error) => {
                tracing::warn!(%error, "process_scan_failed: the process snapshot could not be read");
                false
            }
        };
        if state
            .in_flight
            .as_ref()
            .is_some_and(|current| current.id == id)
        {
            state.in_flight = None;
        }
        refreshed
    }

    /// v2 `scanAgents`: every session's agent, keeping an identity through one
    /// missed snapshot and through a failed one.
    pub async fn scan_agents(
        &self,
        roots: &[SessionProcessRoot],
    ) -> HashMap<SessionId, AgentProcessIdentity> {
        let refreshed = self.refresh(false, None).await;
        let mut state = self.lock();
        let live: HashSet<&SessionId> = roots.iter().map(|root| &root.session_id).collect();
        state
            .held_by_session
            .retain(|session_id, _| live.contains(session_id));
        let ScannerState {
            records,
            held_by_session,
            ..
        } = &mut *state;
        let mut result = HashMap::new();
        for root in roots {
            if let Some(held) = held_by_session.get_mut(&root.session_id) {
                let live_held =
                    find_exact_agent_process_identity(records, root.child_pid, held.identity.pid);
                if let Some(live_held) =
                    live_held.filter(|live| live.agent_id == held.identity.agent_id)
                {
                    held.misses = 0;
                    result.insert(root.session_id.clone(), live_held);
                    continue;
                }
                if !refreshed || held.misses < 1 {
                    if refreshed {
                        held.misses += 1;
                    }
                    result.insert(root.session_id.clone(), held.identity.clone());
                    continue;
                }
            }
            match find_agent_process_identity(records, root.child_pid) {
                Some(detected) => {
                    held_by_session.insert(
                        root.session_id.clone(),
                        HeldIdentity {
                            identity: detected.clone(),
                            misses: 0,
                        },
                    );
                    result.insert(root.session_id.clone(), detected);
                }
                None => {
                    held_by_session.remove(&root.session_id);
                }
            }
        }
        result
    }

    /// v2 `scanReportingAgent`: a forced fresh snapshot, answered only for the
    /// incumbent a routine scan already holds, and only when it IS the reporter.
    pub async fn scan_reporting_agent(
        &self,
        root: &SessionProcessRoot,
        reporter_pid: u32,
        abort: Option<ScanAbort>,
    ) -> Option<AgentProcessIdentity> {
        let active = self.lock().in_flight.clone();
        if let Some(active) = active
            && !self.await_scan(active, abort.clone()).await
        {
            return None;
        }
        if !self.refresh(true, abort).await {
            return None;
        }
        let state = self.lock();
        let held = state.held_by_session.get(&root.session_id)?;
        let live_held =
            find_exact_agent_process_identity(&state.records, root.child_pid, held.identity.pid)?;
        (live_held.agent_id == held.identity.agent_id && live_held.pid == reporter_pid)
            .then_some(live_held)
    }
}

impl AgentProcessScan for AgentProcessScanner {
    fn scan_agents(
        &self,
        roots: Vec<SessionProcessRoot>,
    ) -> OwnerFuture<HashMap<SessionId, AgentProcessIdentity>> {
        let scanner = self.clone();
        Box::pin(async move { AgentProcessScanner::scan_agents(&scanner, &roots).await })
    }

    fn scan_reporting_agent(
        &self,
        root: SessionProcessRoot,
        reporter_pid: u32,
        abort: Option<ScanAbort>,
    ) -> OwnerFuture<Option<AgentProcessIdentity>> {
        let scanner = self.clone();
        Box::pin(async move {
            AgentProcessScanner::scan_reporting_agent(&scanner, &root, reporter_pid, abort).await
        })
    }
}
