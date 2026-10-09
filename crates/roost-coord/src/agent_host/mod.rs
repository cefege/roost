//! Runtime state shared by the coordinator's built-in agent host features.
//!
//! The listener and worker-link dispatcher use the same tunnel registry so a
//! pipe WebSocket can be paired with exactly one authenticated worker link.

use std::sync::Arc;

mod tunnel_socket;
mod tunnels;

pub use tunnel_socket::agent_env_upgrade;
pub use tunnels::AgentTunnelRegistry;

use crate::coord_core::worker_handle::WorkerHandle;

/// The coordinator's built-in agent-host runtime.
#[derive(Debug, Default)]
pub struct AgentHostRuntime {
    /// Active internal-pipe sessions, indexed by their random tunnel id.
    pub tunnels: AgentTunnelRegistry,
}

impl AgentHostRuntime {
    /// Build empty process-local agent host state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl crate::coord_core::worker_lifecycle::WorkerLifecycleObserver for AgentHostRuntime {
    fn acknowledge_capabilities(
        &self,
        advertised: &std::collections::BTreeSet<String>,
    ) -> Vec<&'static str> {
        let capability = roost_protocol::versioning::CAPABILITY_AGENT_TOOL_TUNNEL_V1;
        advertised
            .contains(capability)
            .then_some(capability)
            .into_iter()
            .collect()
    }
    fn on_superseded(&self, superseded: &Arc<WorkerHandle>) {
        self.tunnels.retire_generation(superseded);
    }

    fn on_closed(
        &self,
        handle: &Arc<WorkerHandle>,
        _end: crate::coord_core::worker_lifecycle::LinkEnd,
    ) {
        self.tunnels.retire_generation(handle);
    }
}
