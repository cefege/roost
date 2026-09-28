//! The detector's durable conversation-reference clear: when an omp process
//! leaves a still-live session, exactly one `reference: null` is appended.
//! Ports v2 `detector.ts` `AgentReferenceClearDeps` and
//! `clearReferenceOnAgentExit`; the gate and the append are
//! `agents::reference_admission`'s. Called by the detector's scan only.

use std::sync::Arc;

use roost_protocol::wire::brand::SessionId;

use super::super::BuiltinAgentId;
use super::super::reference_admission::{
    AgentReferenceAdmissionGate, emit_durable_agent_reference,
};
use super::{AgentScreenDetector, lock};
use crate::session::sinks::SessionEventSink;

/// v2 `AgentReferenceClearDeps`: the durable clear path for the conversation
/// reference of a session whose reporting agent is gone.
#[derive(Clone)]
pub struct AgentReferenceClearDeps {
    pub event_sink: Arc<dyn SessionEventSink>,
    pub reference_admission: AgentReferenceAdmissionGate,
}

impl std::fmt::Debug for AgentReferenceClearDeps {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentReferenceClearDeps")
            .finish_non_exhaustive()
    }
}

impl AgentScreenDetector {
    /// v2 `clearReferenceOnAgentExit`: an agent process that disappears from
    /// a live session takes its conversation reference with it — a reference
    /// left behind would later be typed into that session's shell as a resume
    /// for a conversation the user already ended.
    pub(super) fn clear_reference_on_agent_exit(&self, session_id: &SessionId) {
        let Some(clear) = self.inner.deps.reference_clear.clone() else {
            return;
        };
        {
            let mut state = lock(&self.inner.state);
            let was_omp = state
                .last_observed
                .get(session_id)
                .is_some_and(|observed| observed.agent_id == BuiltinAgentId::Omp);
            if !was_omp {
                return;
            }
            // Forgotten before the append is attempted, so one exit clears
            // exactly once.
            state.last_observed.remove(session_id);
        }
        let session_id = session_id.clone();
        self.inner.deps.runtime.spawn(async move {
            let sink = Arc::clone(&clear.event_sink);
            let target = session_id.clone();
            let cleared = clear
                .reference_admission
                .run_exclusive(|| async move { emit_durable_agent_reference(&*sink, &target, None).await })
                .await;
            match cleared {
                Ok(()) => tracing::info!(session_id = %session_id, agent_id = "omp", "reference_cleared_on_agent_exit"),
                Err(error) => tracing::warn!(session_id = %session_id, %error, "reference_clear_failed"),
            }
        });
    }
}
