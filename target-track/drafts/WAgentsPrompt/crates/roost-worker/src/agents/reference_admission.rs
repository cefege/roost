//! Serializes durable agent-reference reports with complete boot
//! reconciliation, and appends one reference set or clear to the durable
//! session-event outbox. The integration report server, the detector's
//! agent-exit clear and the boot reconcile gate all run under the one gate
//! `runtime::owners` builds, and every append goes through
//! [`emit_durable_agent_reference`], so the reservation rules exist once.
//! Ports `apps/worker/src/agents/reference-admission.ts`.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use roost_protocol::agent_conversation_reference::AgentConversationReferenceV1;
use roost_protocol::wire::brand::SessionId;
use roost_protocol::wire::event::SessionEvent;

use crate::event_store::DurableEventKind;
use crate::session::sinks::{SessionEventError, SessionEventSink};

/// One turn at a time, in the order the turns were asked for. A queued
/// reporter is process-revalidated only after adoption, respawn and restore
/// have settled, while ordinary session lifecycle admission stays independent.
/// Clones share the one queue.
#[derive(Debug, Clone, Default)]
pub struct AgentReferenceAdmissionGate {
    turn: Arc<tokio::sync::Mutex<()>>,
}

impl AgentReferenceAdmissionGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run `operation` once every earlier turn has finished. The operation is
    /// not started until its turn comes, and the turn is handed on even when
    /// it fails.
    pub async fn run_exclusive<F, Fut, T>(&self, operation: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let _turn = self.turn.lock().await;
        tracing::trace!("an agent reference turn holds the admission gate");
        operation().await
    }
}

/// Append one durable reference set (`Some`) or clear (`None`) for a session.
///
/// The reservation is released only when the emit failed: the durable sink
/// consumes a claim in the same transaction that writes its row, so a failed
/// emit left the claim live, and a leaked claim is how a store eventually
/// refuses every write.
pub async fn emit_durable_agent_reference(
    sink: &dyn SessionEventSink,
    session_id: &SessionId,
    reference: Option<&AgentConversationReferenceV1>,
) -> Result<(), SessionEventError> {
    let reservation = sink.reserve(DurableEventKind::AgentReference).await?;
    let event = SessionEvent::AgentReference {
        session_id: session_id.clone(),
        reference: reference.cloned(),
        ts: wall_clock_ms(),
        trace_id: None,
    };
    // The store re-parses a durable event before it is written: the reference's
    // own rules and the envelope bound are checked here for the same reason.
    if let Err(error) = checked_event(&event) {
        sink.release(reservation).await;
        tracing::warn!(session = %session_id, %error, "an agent reference was refused before the outbox");
        return Err(error);
    }
    match sink.emit(&event, Some(reservation)).await {
        Ok(()) => {
            tracing::info!(
                session = %session_id,
                cleared = reference.is_none(),
                "an agent reference entered the durable outbox"
            );
            Ok(())
        }
        Err(error) => {
            sink.release(reservation).await;
            tracing::warn!(session = %session_id, %error, "an agent reference did not reach the durable outbox");
            Err(error)
        }
    }
}

fn checked_event(event: &SessionEvent) -> Result<(), SessionEventError> {
    let value = serde_json::to_value(event)
        .map_err(|error| SessionEventError::Unclassifiable(error.to_string()))?;
    SessionEvent::parse(value)
        .map(|_| ())
        .map_err(|error| SessionEventError::Unclassifiable(format!("agent_reference: {error}")))
}

fn wall_clock_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
}
