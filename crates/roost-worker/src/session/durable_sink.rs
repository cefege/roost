//! The durable session-event sink as the manager shares it with the producers
//! that append beside it: v2 `main.ts` hands its one `sink` to both the session
//! manager and the agent-reference producers. `runtime::owners` reads it for the
//! report server, the detector's agent-exit clear and the reconcile gate, which
//! all append through `agents::reference_admission`.

use std::sync::Arc;

use super::lifecycle::SessionManager;
use super::sinks::SessionEventSink;

impl SessionManager {
    /// The durable session-event sink this manager publishes through, so a
    /// reference append shares the outbox and its claim rules.
    pub fn durable_event_sink(&self) -> Arc<dyn SessionEventSink> {
        Arc::clone(&self.events)
    }
}
