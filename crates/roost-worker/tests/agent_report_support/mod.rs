//! The collaborators an agent report server test drives: a scratch endpoint, a
//! peer-PID query that answers what the test says, a detector whose identity
//! the test moves, a registry that records what reached it, and a durable sink
//! that records what was appended. Included by `agent_report_server.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use roost_protocol::wire::brand::SessionId;
use roost_protocol::wire::event::SessionEvent;
use roost_worker::agents::BuiltinAgentId;
use roost_worker::agents::environment::{
    AGENT_CAPABILITY_ENV, AgentReportEnvironment, AgentReportSite,
};
use roost_worker::agents::peer_process_id::{LocalPeerProcessIdReader, NativePeerProcessIdQuery};
use roost_worker::agents::process_scan::AgentProcessIdentity;
use roost_worker::agents::reference_admission::AgentReferenceAdmissionGate;
use roost_worker::agents::registry::IntegrationStatusReport;
use roost_worker::agents::report_admission::{IntegrationReportSink, ReportingAgentLookup};
use roost_worker::agents::report_server::{AgentReportServer, AgentReportServerOptions};
use roost_worker::event_store::{DurableEventKind, Reservation, Store};
use roost_worker::session::sinks::{EventFuture, SessionEventError, SessionEventSink};
use roost_worker::uplink::OwnerFuture;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::scratch::Scratch;

pub const SESSION: &str = "11111111-1111-4111-8111-111111111111";
pub const OTHER_SESSION: &str = "22222222-2222-4222-8222-222222222222";

pub fn session(id: &str) -> SessionId {
    SessionId::try_from(id).expect("a uuid")
}

/// The endpoint a real boot would resolve, rooted in scratch.
pub fn environment(scratch: &Scratch) -> Arc<AgentReportEnvironment> {
    Arc::new(AgentReportEnvironment::resolve(&AgentReportSite {
        data_dir: scratch.root().to_path_buf(),
        configured: None,
    }))
}

/// The capability a session's PTY carries.
pub fn capability(environment: &AgentReportEnvironment, session_id: &str) -> String {
    environment
        .session_overlay(session_id)
        .expect("the endpoint resolved")
        .into_iter()
        .find_map(|(key, value)| (key == AGENT_CAPABILITY_ENV).then_some(value))
        .expect("the overlay carries a capability")
}

/// One `agent.report` line: `patch` overrides or adds `params` fields.
pub fn report_line(environment: &AgentReportEnvironment, patch: Value) -> String {
    let mut params = json!({ "session_id": SESSION, "state": "working", "active": true });
    for (key, value) in patch.as_object().expect("a patch object") {
        params[key] = value.clone();
    }
    let claimed = params["session_id"].as_str().unwrap_or(SESSION).to_owned();
    let request = json!({
        "version": 1,
        "capability": capability(environment, &claimed),
        "method": "agent.report",
        "params": params,
    });
    format!("{request}\n")
}

/// One `agent.reference` line for [`SESSION`].
pub fn reference_line(environment: &AgentReportEnvironment, reference: Value) -> String {
    let request = json!({
        "version": 1,
        "capability": capability(environment, SESSION),
        "method": "agent.reference",
        "params": { "session_id": SESSION, "reference": reference },
    });
    format!("{request}\n")
}

/// Send `body` and read the first answer line.
pub async fn request(path: &std::path::Path, body: &str) -> Value {
    let mut stream = UnixStream::connect(path).await.expect("the server listens");
    stream
        .write_all(body.as_bytes())
        .await
        .expect("the request is written");
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .await
        .expect("an answer arrives");
    serde_json::from_str(line.trim_end()).unwrap_or_else(|error| panic!("{line:?}: {error}"))
}

/// A peer-PID query that answers whatever the test last set.
#[derive(Debug, Default)]
pub struct PeerPid(pub AtomicI64);

impl PeerPid {
    pub fn set(&self, pid: i64) {
        self.0.store(pid, Ordering::SeqCst);
    }
}

impl NativePeerProcessIdQuery for PeerPid {
    fn read(&self, _socket: &UnixStream) -> Result<Option<i64>, String> {
        Ok(Some(self.0.load(Ordering::SeqCst)))
    }
}

pub fn fixed_peer(peer: &Arc<PeerPid>) -> LocalPeerProcessIdReader {
    LocalPeerProcessIdReader::with_query(Arc::clone(peer) as Arc<dyn NativePeerProcessIdQuery>)
}

/// A detector that knows one agent in [`SESSION`] and records every
/// attested reporter it was asked about.
#[derive(Debug, Default)]
pub struct Detector {
    pub identity: Mutex<Option<(BuiltinAgentId, u32)>>,
    pub attested: Mutex<Vec<u32>>,
}

impl Detector {
    pub fn knowing(agent: BuiltinAgentId, pid: u32) -> Arc<Self> {
        let detector = Arc::new(Self::default());
        detector.set(Some((agent, pid)));
        detector
    }

    pub fn set(&self, identity: Option<(BuiltinAgentId, u32)>) {
        *self.identity.lock().unwrap() = identity;
    }
}

impl ReportingAgentLookup for Detector {
    fn reporting_agent_for_session(
        &self,
        session_id: &SessionId,
        reporter_pid: u32,
    ) -> OwnerFuture<Option<AgentProcessIdentity>> {
        self.attested.lock().unwrap().push(reporter_pid);
        let identity = *self.identity.lock().unwrap();
        let found = identity
            .filter(|(_, pid)| session_id.as_str() == SESSION && *pid == reporter_pid)
            .map(|(agent_id, pid)| AgentProcessIdentity {
                agent_id,
                pid,
                foreground: None,
            });
        Box::pin(std::future::ready(found))
    }
}

/// Records every report that reached it, then forwards or accepts.
#[derive(Default)]
pub struct Reports {
    pub received: Mutex<Vec<IntegrationStatusReport>>,
    pub forward: Option<Arc<dyn IntegrationReportSink>>,
}

impl IntegrationReportSink for Reports {
    fn report_integration(&self, report: IntegrationStatusReport) -> bool {
        self.received.lock().unwrap().push(report.clone());
        self.forward
            .as_ref()
            .is_none_or(|registry| registry.report_integration(report))
    }
}

/// A durable sink over a real claim store, recording what was appended.
#[derive(Default)]
pub struct Ledger {
    store: Mutex<Store>,
    pub events: Mutex<Vec<SessionEvent>>,
}

impl SessionEventSink for Ledger {
    fn reserve(
        &self,
        kind: DurableEventKind,
    ) -> EventFuture<'_, Result<Reservation, SessionEventError>> {
        let reserved = self
            .store
            .lock()
            .unwrap()
            .reserve_default(kind)
            .map_err(SessionEventError::Reserve);
        Box::pin(std::future::ready(reserved))
    }

    fn hold(&self, reservation: Reservation) -> EventFuture<'_, ()> {
        let _ = self.store.lock().unwrap().hold(reservation);
        Box::pin(std::future::ready(()))
    }

    fn release(&self, reservation: Reservation) -> EventFuture<'_, ()> {
        let _ = self.store.lock().unwrap().release(reservation);
        Box::pin(std::future::ready(()))
    }

    fn emit<'a>(
        &'a self,
        event: &'a SessionEvent,
        reservation: Option<Reservation>,
    ) -> EventFuture<'a, Result<(), SessionEventError>> {
        if let Some(reservation) = reservation {
            let bytes = serde_json::to_vec(event)
                .expect("an event serialises")
                .len();
            self.store
                .lock()
                .unwrap()
                .append(reservation, DurableEventKind::AgentReference, bytes)
                .expect("the claim is live");
        }
        self.events.lock().unwrap().push(event.clone());
        Box::pin(std::future::ready(Ok(())))
    }
}

/// A server over `environment` with the given collaborators.
pub fn start(
    environment: &Arc<AgentReportEnvironment>,
    detector: Arc<Detector>,
    reports: Arc<Reports>,
    ledger: Arc<Ledger>,
    peer: Option<LocalPeerProcessIdReader>,
) -> AgentReportServer {
    AgentReportServer::start(AgentReportServerOptions {
        environment: Arc::clone(environment),
        detector,
        registry: reports,
        event_sink: ledger,
        reference_admission: AgentReferenceAdmissionGate::new(),
        peer_process_id_reader: peer,
        socket_path: None,
    })
    .expect("the report server starts")
}
