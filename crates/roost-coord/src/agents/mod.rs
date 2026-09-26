//! The agents domain: per-session agent status, its ordering and wait queues,
//! the operator's agent configuration, and the fenced prompt control.
//!
//! One field on `CoordServices`, reached as `core.services.agents`. The status
//! hub, the wait queue and the push scheduler all read one table, so a second
//! instance would be a second answer to "is that agent still running".
//!
//! `new()` takes nothing and must keep taking nothing: anything the domain
//! needs from configuration is read at call time from `core.services.boot`.
//!
//! AG1 owns the six status, push and config files below and the `status` field
//! on the runtime; AG2 owns `prompt_control` and `rpc_prompt` and adds the
//! `prompt` field. Each names the other's claim in this header, so the next
//! reader knows which slice is mid-flight rather than guessing.

pub mod config;
pub mod rpc_status;
pub mod status_hub;
pub mod status_order;
pub mod status_push;
pub mod status_wait;

/// The agent state one coordinator process holds.
#[derive(Debug, Default)]
pub struct AgentsRuntime {
    /// The one table of what every agent in the fleet is doing: retained
    /// status, its admission order, its close tombstones, its wait queue and
    /// the debounced push schedule.
    pub status: crate::agents::status_hub::AgentStatusHub,
}

impl AgentsRuntime {
    /// A coordinator that has never seen an agent.
    #[must_use]
    pub fn new() -> Self {
        Self {
            status: crate::agents::status_hub::AgentStatusHub::new(),
        }
    }
}
