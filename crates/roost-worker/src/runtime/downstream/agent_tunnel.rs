//! The worker-owned agent daemon tunnel frames.
//!
//! Each frame is handed to the sole process owner; the owner sends stdout and
//! lifecycle frames through the link's fenced uplink.

use roost_proto::{
    DAgentTunnelClose, DAgentTunnelDaemonChunk, DAgentTunnelInput, DAgentTunnelOpen,
};

use super::Dispatcher;

impl Dispatcher {
    pub(super) fn agent_tunnel_open(&self, request: DAgentTunnelOpen) {
        if let Some(owner) = self
            .owners
            .as_ref()
            .and_then(|owners| owners.agent_tunnel.as_ref())
        {
            owner.open(request);
        }
    }
    pub(super) fn agent_tunnel_input(&self, request: DAgentTunnelInput) {
        if let Some(owner) = self
            .owners
            .as_ref()
            .and_then(|owners| owners.agent_tunnel.as_ref())
        {
            owner.input(request);
        }
    }
    pub(super) fn agent_tunnel_daemon_chunk(&self, request: DAgentTunnelDaemonChunk) {
        if let Some(owner) = self
            .owners
            .as_ref()
            .and_then(|owners| owners.agent_tunnel.as_ref())
        {
            owner.daemon_chunk(request);
        }
    }
    pub(super) fn agent_tunnel_close(&self, request: DAgentTunnelClose) {
        if let Some(owner) = self
            .owners
            .as_ref()
            .and_then(|owners| owners.agent_tunnel.as_ref())
        {
            owner.close(request);
        }
    }
}
