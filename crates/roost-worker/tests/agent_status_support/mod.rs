//! Collaborators the agent-status tests drive: a settable clock, a recording
//! publisher, a scripted process scanner and session source, and a detector
//! over them with the real pinned manifests. Mirrors the harnesses of v2
//! `apps/worker/tests/agents/agent-status-*.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)]
// Compiled into every `agent_status_*` test binary, and each one reaches for a
// different subset (the registry tests never build a detector), so dead-code
// here is a statement about one binary rather than about the fixture.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use roost_observability::clock::EventClock;
use roost_protocol::wire::agent_status::AgentStatusUpdate;
use roost_protocol::wire::brand::SessionId;
use roost_worker::agents::BuiltinAgentId;
use roost_worker::agents::detector::sessions::{
    AgentSessionSource, AgentSessionView, ScreenEvidence,
};
use roost_worker::agents::detector::{
    AgentReferenceClearDeps, AgentScreenDetector, AgentScreenDetectorDeps,
};
use roost_worker::agents::environment::AgentReportEnvironment;
use roost_worker::agents::manifests::AgentManifests;
use roost_worker::agents::process_scan::{
    AgentProcessIdentity, AgentProcessScan, SessionProcessRoot,
};
use roost_worker::agents::process_snapshot::ScanAbort;
use roost_worker::agents::registry::{
    AgentStatusPublisher, AgentStatusRegistry, AgentStatusRegistryOptions, INTEGRATION_LEASE_MS,
};
use roost_worker::host::local_endpoint::LocalEndpoint;
use roost_worker::uplink::OwnerFuture;

pub const SESSION_ID: &str = "11111111-1111-4111-8111-111111111111";
pub const OTHER_SESSION_ID: &str = "22222222-2222-4222-8222-222222222222";

pub fn session(id: &str) -> SessionId {
    SessionId::try_from(id).unwrap()
}

/// A clock whose wall and monotonic readings the test moves together.
#[derive(Debug, Default)]
pub struct TestClock {
    now_ms: AtomicI64,
}

impl TestClock {
    pub fn at(now_ms: i64) -> Arc<Self> {
        Arc::new(Self {
            now_ms: AtomicI64::new(now_ms),
        })
    }
    pub fn advance(&self, by_ms: i64) {
        self.now_ms.fetch_add(by_ms, Ordering::SeqCst);
    }
}

impl EventClock for TestClock {
    fn now_epoch_ms(&self) -> i64 {
        self.now_ms.load(Ordering::SeqCst)
    }
    fn mono_ns(&self) -> u64 {
        u64::try_from(self.now_ms.load(Ordering::SeqCst)).unwrap() * 1_000_000
    }
}

/// v2 `publish: (status) => published.push(status)`.
#[derive(Debug, Default)]
pub struct Published {
    statuses: Mutex<Vec<AgentStatusUpdate>>,
}

impl Published {
    pub fn all(&self) -> Vec<AgentStatusUpdate> {
        self.statuses.lock().unwrap().clone()
    }
    pub fn len(&self) -> usize {
        self.statuses.lock().unwrap().len()
    }
    pub fn last(&self) -> AgentStatusUpdate {
        self.statuses
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("something was published")
    }
    /// `published.at(-n)` for n ≥ 1.
    pub fn nth_last(&self, n: usize) -> AgentStatusUpdate {
        let statuses = self.statuses.lock().unwrap();
        statuses[statuses.len() - n].clone()
    }
}

impl AgentStatusPublisher for Published {
    fn publish(&self, status: AgentStatusUpdate) {
        self.statuses.lock().unwrap().push(status);
    }
}

pub struct RegistryHarness {
    pub clock: Arc<TestClock>,
    pub published: Arc<Published>,
    pub registry: Arc<AgentStatusRegistry>,
}

/// v2 `registryHarness(startAt)`: `leaseMs: 100`, no lease timer.
pub fn registry_harness(start_at: i64) -> RegistryHarness {
    registry_with_lease(start_at, 100)
}

/// v2's harness with the default lease (`new AgentStatusRegistry({ publish })`).
pub fn default_registry() -> RegistryHarness {
    registry_with_lease(1_000, INTEGRATION_LEASE_MS)
}

fn registry_with_lease(start_at: i64, lease_ms: i64) -> RegistryHarness {
    let clock = TestClock::at(start_at);
    let published = Arc::new(Published::default());
    let registry = AgentStatusRegistry::new(AgentStatusRegistryOptions {
        publish: Arc::clone(&published) as Arc<dyn AgentStatusPublisher>,
        clock: Arc::clone(&clock) as Arc<dyn EventClock>,
        lease_ms,
    })
    .unwrap();
    RegistryHarness {
        clock,
        published,
        registry,
    }
}

/// No wire status may name a process id.
pub fn assert_no_process_id(status: &AgentStatusUpdate) {
    let json = serde_json::to_value(status).unwrap();
    for key in ["pid", "processId", "process_id"] {
        assert!(json.get(key).is_none(), "a wire status carried {key}");
    }
}

/// v2's scripted `scanner`: fixed identities, counted scans.
#[derive(Debug, Default)]
pub struct ScriptedScanner {
    pub identities: Mutex<HashMap<SessionId, AgentProcessIdentity>>,
    pub scans: AtomicUsize,
}

impl ScriptedScanner {
    pub fn set(&self, session_id: &str, agent_id: BuiltinAgentId, pid: u32) {
        self.identities.lock().unwrap().insert(
            session(session_id),
            AgentProcessIdentity {
                agent_id,
                pid,
                foreground: None,
            },
        );
    }
    pub fn remove(&self, session_id: &str) {
        self.identities.lock().unwrap().remove(&session(session_id));
    }
}

impl AgentProcessScan for ScriptedScanner {
    fn scan_agents(
        &self,
        _: Vec<SessionProcessRoot>,
    ) -> OwnerFuture<HashMap<SessionId, AgentProcessIdentity>> {
        self.scans.fetch_add(1, Ordering::SeqCst);
        let identities = self.identities.lock().unwrap().clone();
        Box::pin(async move { identities })
    }
    fn scan_reporting_agent(
        &self,
        _: SessionProcessRoot,
        _: u32,
        _: Option<ScanAbort>,
    ) -> OwnerFuture<Option<AgentProcessIdentity>> {
        Box::pin(async { None })
    }
}

/// One scripted session record (v2's `records` entries).
#[derive(Debug, Clone)]
pub struct ScriptedRecord {
    pub session_id: SessionId,
    pub channel_id: u16,
    pub child_pid: u32,
    pub screen: String,
    pub osc_title: String,
    pub osc_progress: String,
}

/// v2's scripted `sessions`: `allSessions`, `getBySessionId`, and a grid
/// whose every read is counted.
#[derive(Debug, Default)]
pub struct ScriptedSessions {
    pub records: Mutex<Vec<ScriptedRecord>>,
    pub reads: AtomicUsize,
}

impl ScriptedSessions {
    pub fn add(&self, osc_title: &str) {
        self.add_with_child(osc_title, 4_321);
    }
    pub fn add_with_child(&self, osc_title: &str, child_pid: u32) {
        self.records.lock().unwrap().push(ScriptedRecord {
            session_id: session(SESSION_ID),
            channel_id: 7,
            child_pid,
            screen: "\n".to_owned(),
            osc_title: osc_title.to_owned(),
            osc_progress: String::new(),
        });
    }
    pub fn clear(&self) {
        self.records.lock().unwrap().clear();
    }
    pub fn osc_title(&self) -> String {
        self.records.lock().unwrap()[0].osc_title.clone()
    }
    pub fn set_osc_title(&self, title: &str) {
        self.records.lock().unwrap()[0].osc_title = title.to_owned();
    }
}

impl AgentSessionSource for ScriptedSessions {
    fn all_sessions(&self) -> Vec<AgentSessionView> {
        let records = self.records.lock().unwrap();
        records.iter().map(view_of).collect()
    }
    fn session(&self, session_id: &SessionId) -> Option<AgentSessionView> {
        let records = self.records.lock().unwrap();
        records
            .iter()
            .find(|record| record.session_id == *session_id)
            .map(view_of)
    }
    fn screen_evidence(&self, session_id: &SessionId) -> Option<ScreenEvidence> {
        let records = self.records.lock().unwrap();
        let record = records
            .iter()
            .find(|record| record.session_id == *session_id)?;
        self.reads.fetch_add(1, Ordering::SeqCst);
        Some(ScreenEvidence {
            screen: record.screen.clone(),
            osc_title: record.osc_title.clone(),
            osc_progress: record.osc_progress.clone(),
        })
    }
    fn clear_osc_evidence(&self, session_id: &SessionId) {
        let mut records = self.records.lock().unwrap();
        if let Some(record) = records
            .iter_mut()
            .find(|record| record.session_id == *session_id)
        {
            record.osc_title.clear();
            record.osc_progress.clear();
        }
    }
}

fn view_of(record: &ScriptedRecord) -> AgentSessionView {
    AgentSessionView {
        session_id: record.session_id.clone(),
        channel_id: record.channel_id,
        child_pid: Some(record.child_pid),
    }
}

/// A report environment over a made-up endpoint; nothing touches the disk.
pub fn test_environment() -> Arc<AgentReportEnvironment> {
    Arc::new(AgentReportEnvironment::for_endpoint(LocalEndpoint {
        address: PathBuf::from("/tmp/roost-agent-status-test.sock"),
        capability: "a".repeat(64),
        capability_path: PathBuf::from("/tmp/roost-agent-status-test.cap"),
    }))
}

pub struct DetectorHarness {
    pub clock: Arc<TestClock>,
    pub published: Arc<Published>,
    pub registry: Arc<AgentStatusRegistry>,
    pub scanner: Arc<ScriptedScanner>,
    pub sessions: Arc<ScriptedSessions>,
    pub environment: Arc<AgentReportEnvironment>,
    pub detector: AgentScreenDetector,
}

/// v2 `makeDetector`/`makeHarness`: a detector over the scripted scanner and
/// sessions, the real manifests, and a registry with the default lease.
pub fn detector_harness(reference_clear: Option<AgentReferenceClearDeps>) -> DetectorHarness {
    let RegistryHarness {
        clock,
        published,
        registry,
    } = default_registry();
    let scanner = Arc::new(ScriptedScanner::default());
    let sessions = Arc::new(ScriptedSessions::default());
    let environment = test_environment();
    let detector = detector_over(DetectorParts {
        sessions: Arc::clone(&sessions) as Arc<dyn AgentSessionSource>,
        scanner: Arc::clone(&scanner) as Arc<dyn AgentProcessScan>,
        registry: Arc::clone(&registry),
        clock: Arc::clone(&clock),
        environment: Arc::clone(&environment),
        reference_clear,
    });
    DetectorHarness {
        clock,
        published,
        registry,
        scanner,
        sessions,
        environment,
        detector,
    }
}

/// The collaborators a detector is built over, for a test that brings its own.
pub struct DetectorParts {
    pub sessions: Arc<dyn AgentSessionSource>,
    pub scanner: Arc<dyn AgentProcessScan>,
    pub registry: Arc<AgentStatusRegistry>,
    pub clock: Arc<TestClock>,
    pub environment: Arc<AgentReportEnvironment>,
    pub reference_clear: Option<AgentReferenceClearDeps>,
}

pub fn detector_over(parts: DetectorParts) -> AgentScreenDetector {
    AgentScreenDetector::new(AgentScreenDetectorDeps {
        sessions: parts.sessions,
        registry: parts.registry,
        scanner: parts.scanner,
        manifests: Arc::new(AgentManifests::pinned().unwrap()),
        environment: parts.environment,
        clock: parts.clock as Arc<dyn EventClock>,
        reference_clear: parts.reference_clear,
        runtime: tokio::runtime::Handle::current(),
    })
}
