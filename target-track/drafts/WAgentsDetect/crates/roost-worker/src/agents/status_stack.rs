//! The agent-status owners, built once at boot: the registry publishing
//! through the uplink, the process scanner, and the screen detector wired to
//! the session layer's terminal-changed and session-closed hooks. Ports the
//! composition in v2 `apps/worker/src/main.ts:220-237` and the registry resend
//! in `transport/coord-link-deps.ts` `onSnapshotReady`. Built by
//! `runtime::owners`; `agents::report_server` and `agents::prompt_control`
//! take their handles from it.

use std::sync::Arc;

use roost_observability::clock::EventClock;
use roost_protocol::wire::agent_status::{AgentStatus, AgentStatusUpdate};
use roost_protocol::wire::coord_worker::{AgentStatusFrame, CoordWorkerUpstream};

use super::detector::sessions::TableAgentSessions;
use super::detector::{AgentReferenceClearDeps, AgentScreenDetector, AgentScreenDetectorDeps};
use super::environment::AgentReportEnvironment;
use super::manifests::AgentManifests;
use super::process_scan::{AgentProcessScanner, SCAN_THROTTLE};
use super::process_snapshot::PsSnapshotReader;
use super::registry::{
    AgentStatusPublisher, AgentStatusRegistry, AgentStatusRegistryOptions, INTEGRATION_LEASE_MS,
};
use crate::link_ports::LinkLifecyclePort;
use crate::session::ids::MintError;
use crate::session::lifecycle::SessionManager;
use crate::session::table::SessionTable;
use crate::session::terminal_changed::TerminalChangedHooks;
use crate::uplink::Uplink;

/// v2 `publish: (status) => coordLink.sendAgentStatus(status)`: every frame
/// the registry publishes goes to the link loop, which owns the agent-status
/// outbox (`runtime::link_loop::agent_status`).
#[derive(Debug, Clone)]
pub struct UplinkAgentStatusPublisher {
    uplink: Uplink,
}

impl UplinkAgentStatusPublisher {
    pub fn new(uplink: Uplink) -> Self {
        Self { uplink }
    }
}

impl AgentStatusPublisher for UplinkAgentStatusPublisher {
    fn publish(&self, status: AgentStatusUpdate) {
        let frame = CoordWorkerUpstream::AgentStatus(AgentStatusFrame {
            status: AgentStatus {
                common: status.common,
                active: status.active,
            },
        });
        if !self.uplink.send(frame) {
            tracing::debug!("an agent status was published with no link loop to carry it");
        }
    }
}

/// What [`AgentStatusStack::start`] composes over.
pub struct AgentStatusStackDeps<'a> {
    pub uplink: Uplink,
    pub table: Arc<SessionTable>,
    pub manager: &'a SessionManager,
    pub terminal_changed: &'a TerminalChangedHooks,
    pub clock: Arc<dyn EventClock>,
    pub manifests: Arc<AgentManifests>,
    pub environment: Arc<AgentReportEnvironment>,
    pub reference_clear: Option<AgentReferenceClearDeps>,
    pub runtime: tokio::runtime::Handle,
}

/// The registry and detector every agent-status consumer shares.
#[derive(Debug, Clone)]
pub struct AgentStatusStack {
    pub registry: Arc<AgentStatusRegistry>,
    pub detector: Arc<AgentScreenDetector>,
}

impl AgentStatusStack {
    /// Build the registry and detector, install the session hooks, and start
    /// the lease sweep and the detector's scan interval. Must run inside the
    /// worker's runtime.
    pub fn start(deps: AgentStatusStackDeps<'_>) -> Result<Self, MintError> {
        let registry = AgentStatusRegistry::new(AgentStatusRegistryOptions {
            publish: Arc::new(UplinkAgentStatusPublisher::new(deps.uplink)),
            clock: Arc::clone(&deps.clock),
            lease_ms: INTEGRATION_LEASE_MS,
        })?;
        registry.spawn_lease_sweep(&deps.runtime);
        let scanner = AgentProcessScanner::new(
            Arc::new(PsSnapshotReader::default()),
            SCAN_THROTTLE,
            deps.runtime.clone(),
        );
        let detector = Arc::new(AgentScreenDetector::new(AgentScreenDetectorDeps {
            sessions: Arc::new(TableAgentSessions::new(deps.table)),
            registry: Arc::clone(&registry),
            scanner: Arc::new(scanner),
            manifests: deps.manifests,
            environment: deps.environment,
            clock: deps.clock,
            reference_clear: deps.reference_clear,
            runtime: deps.runtime,
        }));
        let scheduled = Arc::downgrade(&detector);
        deps.terminal_changed.install(Arc::new(move |channel_id| {
            if let Some(detector) = scheduled.upgrade() {
                detector.schedule(channel_id);
            }
        }));
        let closing = Arc::downgrade(&detector);
        deps.manager.on_session_closed(Arc::new(move |session_id| {
            if let Some(detector) = closing.upgrade() {
                detector.close_session(session_id);
            }
        }));
        detector.start();
        tracing::info!("agent-status detection is composed: registry, scanner and detector");
        Ok(Self { registry, detector })
    }

    /// v2 `onSnapshotReady` → `agentRegistry.resend()`: the coordinator's
    /// barrier cleared, so every retained occupant is asserted again.
    pub fn resend(&self) {
        self.registry.resend();
    }

    /// v2 shutdown: `agentDetector.dispose(); agentRegistry.dispose()`.
    pub fn dispose(&self) {
        self.detector.dispose();
        self.registry.dispose();
    }
}

/// The registry is one owner of the link's lifecycle, and v2 gives it exactly
/// one callback: `onSnapshotReady`. The other transitions change nothing a
/// registry holds — its occupants outlive every socket.
impl LinkLifecyclePort for AgentStatusStack {
    fn on_open(&self) {}
    fn on_hello_ack(&self, _terminal_metadata_negotiated: bool) {}
    fn on_detach(&self) {}
    fn on_writable(&self) {}
    fn on_snapshot_ready(&self) {
        self.resend();
    }
}
