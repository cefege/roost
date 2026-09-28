//! What the report server does with an authenticated request: prove the
//! reporter is the agent the worker itself sees in that session (kernel peer
//! PID plus a fresh process scan), then order a status into the registry or
//! append a conversation reference durably. Ports the admission half of
//! `apps/worker/src/agents/report-server.ts`; called by
//! `agents::report_connection` for the one request a connection carries.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use roost_protocol::wire::brand::SessionId;

use crate::agents::BuiltinAgentId;
use crate::agents::detector::AgentScreenDetector;
use crate::agents::process_scan::AgentProcessIdentity;
use crate::agents::reference_admission::{
    AgentReferenceAdmissionGate, emit_durable_agent_reference,
};
use crate::agents::registry::{AgentStatusRegistry, IntegrationStatusReport};
use crate::agents::report_protocol::{AgentReferenceReportRequest, AgentStatusReportRequest};
use crate::session::sinks::{SessionEventError, SessionEventSink};
use crate::uplink::OwnerFuture;

/// The largest sequence a JavaScript peer can hold without losing precision;
/// the registry's revisions travel to the browser.
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Who, if anyone, is the reporting agent a session's reporter proved to be.
pub trait ReportingAgentLookup: Send + Sync {
    /// `reporter_pid` is the kernel-attested peer of the reporting socket; the
    /// answer is `None` unless a FRESH scan finds that process as the session's
    /// agent, so a caller can never select its own identity.
    fn reporting_agent_for_session(
        &self,
        session_id: &SessionId,
        reporter_pid: u32,
    ) -> OwnerFuture<Option<AgentProcessIdentity>>;
}

/// Where an admitted integration status goes.
pub trait IntegrationReportSink: Send + Sync {
    /// `false` when the report is stale against what the registry holds.
    fn report_integration(&self, report: IntegrationStatusReport) -> bool;
}

/// Why an admission could not complete. The two request kinds answer it
/// differently: a status gets `internal_error`, while a reference that may or
/// may not have been appended gets no answer at all.
#[derive(Debug, thiserror::Error)]
pub(super) enum AdmissionFault {
    #[error("agent report sequence exhausted")]
    SequenceExhausted,
    #[error("the durable agent reference could not be appended: {0}")]
    ReferenceAppend(#[from] SessionEventError),
}

/// A refusal the reporter reads as `error`.
pub(super) type AdmissionRefusal = &'static str;

pub(super) struct ReportAdmission {
    detector: Arc<dyn ReportingAgentLookup>,
    registry: Arc<dyn IntegrationReportSink>,
    event_sink: Arc<dyn SessionEventSink>,
    reference_admission: AgentReferenceAdmissionGate,
    /// The last sequence handed to the registry. Held across the identity scan
    /// so status admissions complete strictly in arrival order.
    integration_seq: tokio::sync::Mutex<u64>,
}

impl ReportAdmission {
    pub(super) fn new(
        detector: Arc<dyn ReportingAgentLookup>,
        registry: Arc<dyn IntegrationReportSink>,
        event_sink: Arc<dyn SessionEventSink>,
        reference_admission: AgentReferenceAdmissionGate,
    ) -> Self {
        Self {
            detector,
            registry,
            event_sink,
            reference_admission,
            integration_seq: tokio::sync::Mutex::new(wall_clock_micros()),
        }
    }

    /// Admit one status. The sequence is the worker's, never the reporter's:
    /// it is monotonic across reports and never behind the wall clock, so a
    /// restarted worker's first report outranks what it reported before.
    pub(super) async fn admit_report(
        &self,
        request: AgentStatusReportRequest,
        reporter_pid: u32,
    ) -> Result<Option<AdmissionRefusal>, AdmissionFault> {
        let mut last_seq = self.integration_seq.lock().await;
        let identity = self
            .detector
            .reporting_agent_for_session(&request.session_id, reporter_pid)
            .await;
        let Some(identity) = identity else {
            return Ok(Some("reporter_identity_mismatch"));
        };
        let seq = last_seq.saturating_add(1).max(wall_clock_micros());
        if seq > MAX_SAFE_INTEGER {
            return Err(AdmissionFault::SequenceExhausted);
        }
        *last_seq = seq;
        let accepted = self.registry.report_integration(IntegrationStatusReport {
            session_id: request.session_id,
            agent_id: identity.agent_id,
            process_id: identity.pid,
            state: request.state,
            message: request.message,
            seq,
            active: request.active,
        });
        Ok((!accepted).then_some("stale_report"))
    }

    /// Admit one durable reference set or clear, serialized with boot
    /// reconciliation so a reporter is re-proven only after adoption settles.
    /// The acknowledgement follows the append, never mere receipt.
    pub(super) async fn admit_reference(
        &self,
        request: AgentReferenceReportRequest,
        reporter_pid: u32,
    ) -> Result<Option<AdmissionRefusal>, AdmissionFault> {
        self.reference_admission
            .run_exclusive(|| async {
                let identity = self
                    .detector
                    .reporting_agent_for_session(&request.session_id, reporter_pid)
                    .await;
                let Some(identity) = identity else {
                    return Ok(Some("reporter_identity_mismatch"));
                };
                if identity.agent_id != BuiltinAgentId::Omp {
                    return Ok(Some("unsupported_agent"));
                }
                emit_durable_agent_reference(
                    self.event_sink.as_ref(),
                    &request.session_id,
                    request.reference.as_ref(),
                )
                .await?;
                tracing::info!(
                    session_id = %request.session_id,
                    agent_id = identity.agent_id.as_str(),
                    action = if request.reference.is_none() { "clear" } else { "set" },
                    "an agent conversation reference report was committed durably"
                );
                Ok::<_, AdmissionFault>(None)
            })
            .await
    }
}

/// The wall clock in milliseconds, scaled to microseconds: the sequence's unit
/// leaves room for a thousand reports per millisecond before it runs ahead of
/// the clock. A clock before the epoch reads as zero.
fn wall_clock_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
        .saturating_mul(1_000)
}

impl ReportingAgentLookup for AgentScreenDetector {
    fn reporting_agent_for_session(
        &self,
        session_id: &SessionId,
        reporter_pid: u32,
    ) -> OwnerFuture<Option<AgentProcessIdentity>> {
        // No abort: a report waits for its scan like any other caller.
        AgentScreenDetector::reporting_agent_for_session(self, session_id, reporter_pid, None)
    }
}

impl IntegrationReportSink for AgentStatusRegistry {
    fn report_integration(&self, report: IntegrationStatusReport) -> bool {
        AgentStatusRegistry::report_integration(self, report)
    }
}
