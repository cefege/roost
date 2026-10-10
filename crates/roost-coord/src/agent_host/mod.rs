//! Runtime state shared by the coordinator's built-in agent host features.
//!
//! The listener and worker-link dispatcher use the same tunnel registry so a
//! pipe WebSocket can be paired with exactly one authenticated worker link.

use std::sync::Arc;

mod cache;
mod client;
mod follow;
pub mod rpc_auth;
pub mod rpc_chat;
mod tunnel_socket;
mod tunnels;

pub use cache::{AgentChatUpdate, ChatCache};
pub use client::{AgentHostClient, AgentHostError, to_connect};
pub use follow::{FollowerHandle, spawn_follower};

use std::sync::{Mutex, OnceLock};

use connectrpc::{ConnectError, ErrorCode};

pub use tunnel_socket::agent_env_upgrade;
pub use tunnels::AgentTunnelRegistry;

use crate::coord_core::worker_handle::WorkerHandle;
use crate::events::bus_messages::{AgentChatEventsUpdate, AgentConversationUpdate};

/// The coordinator's built-in agent-host runtime.
#[derive(Debug, Default)]
pub struct AgentHostRuntime {
    /// Active internal-pipe sessions, indexed by their random tunnel id.
    pub tunnels: AgentTunnelRegistry,
    /// Host stream's current conversation and transcript projection.
    pub cache: Mutex<ChatCache>,
    client: OnceLock<AgentHostClient>,
}

impl AgentHostRuntime {
    /// Build empty process-local agent host state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    /// Return the configured HTTP client, or `Unavailable` when disabled.
    pub fn require_client(
        &self,
        services: &crate::services::CoordServices,
    ) -> Result<&AgentHostClient, ConnectError> {
        let config = services.boot.require_config()?;
        let (Some(base), Some(secret)) = (&config.agent_host_url, &config.agent_host_secret) else {
            return Err(ConnectError::new(
                ErrorCode::Unavailable,
                "built-in agent is not configured",
            ));
        };
        Ok(self
            .client
            .get_or_init(|| AgentHostClient::new(base.clone(), secret.clone())))
    }

    /// Publish one cache update to its install-wide Sync bus.
    pub fn publish(&self, services: &crate::services::CoordServices, update: AgentChatUpdate) {
        match update {
            AgentChatUpdate::Conversations { conversations } => {
                for conversation in conversations {
                    services
                        .buses
                        .agent_conversation_bus
                        .publish(AgentConversationUpdate {
                            conversation_id: conversation.id.clone(),
                            removed: false,
                            conversation: Some(conversation),
                            host_connected: None,
                        });
                }
            }
            AgentChatUpdate::Conversation { conversation } => services
                .buses
                .agent_conversation_bus
                .publish(AgentConversationUpdate {
                    conversation_id: conversation.id.clone(),
                    removed: false,
                    conversation: Some(*conversation),
                    host_connected: None,
                }),
            AgentChatUpdate::ConversationRemoved { id } => services
                .buses
                .agent_conversation_bus
                .publish(AgentConversationUpdate {
                    conversation_id: id,
                    removed: true,
                    conversation: None,
                    host_connected: None,
                }),
            AgentChatUpdate::HostConnected { connected } => services
                .buses
                .agent_conversation_bus
                .publish(AgentConversationUpdate {
                    conversation_id: String::new(),
                    removed: false,
                    conversation: None,
                    host_connected: Some(connected),
                }),
            AgentChatUpdate::Events {
                conversation_id,
                seq,
                events_json,
            } => services
                .buses
                .agent_chat_bus
                .publish(AgentChatEventsUpdate {
                    conversation_id,
                    seq,
                    events_json,
                }),
        }
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
