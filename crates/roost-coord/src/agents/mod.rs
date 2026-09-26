//! The agents domain: per-session agent status, its ordering and wait queues,
//! the operator's agent configuration, and the fenced prompt control.
//!
//! One field on `CoordServices`, reached as `core.services.agents`. The status
//! hub, the wait queue and the push scheduler all read one table, so a second
//! instance would be a second answer to "is that agent still running".
//!
//! `new()` takes nothing and must keep taking nothing: anything the domain
//! needs from configuration is read at call time from `core.services.boot`.

/// The agent state one coordinator process holds.
#[derive(Debug, Default)]
pub struct AgentsRuntime;

impl AgentsRuntime {
    /// A coordinator that has never seen an agent.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}
