//! The default-agent launch configuration, read and written as one value.
//!
//! Called by roost-web's Settings launcher pane. v2 call sites:
//! `apps/web/src/lib/agents.ts:84-120` (`loadAgentConfig`, `saveAgentConfig`,
//! `saveAutoLaunch`). One request carries all three fields because the
//! coordinator overwrites each of them, so a partial write would clear the two
//! it left out.

use roost_proto::{AgentConfig, AgentConfigGetRequest, AgentConfigSetRequest};

use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;

/// The launch configuration, as the coordinator holds it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentLauncherConfig {
    /// A built-in agent id, or `"custom"`.
    pub selected: String,
    /// The custom launch string; empty when unset.
    pub custom_command: String,
    /// Whether a new terminal auto-launches the agent.
    pub auto_launch: bool,
}

impl AgentLauncherConfig {
    /// The coordinator's own answer, unchanged.
    fn from_proto(config: AgentConfig) -> Self {
        Self {
            selected: config.selected,
            custom_command: config.custom_command,
            auto_launch: config.auto_launch,
        }
    }
}

/// `AgentConfigGet`: the stored launch configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GetAgentConfig;

impl UnaryMethod for GetAgentConfig {
    const METHOD: &'static str = "AgentConfigGet";
    type Response = AgentLauncherConfig;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(Self::METHOD, &AgentConfigGetRequest::default())
    }

    fn decode_response(body: &[u8]) -> Result<AgentLauncherConfig, RpcCodecError> {
        let config: AgentConfig = decode_message(Self::METHOD, body)?;
        Ok(AgentLauncherConfig::from_proto(config))
    }
}

/// `AgentConfigSet`: overwrite the launch configuration and return it as it now
/// stands.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SetAgentConfig {
    /// A built-in agent id, or `"custom"`.
    pub selected: String,
    /// The custom launch string; empty clears it.
    pub custom_command: String,
    /// Whether a new terminal auto-launches the agent.
    pub auto_launch: bool,
}

impl UnaryMethod for SetAgentConfig {
    const METHOD: &'static str = "AgentConfigSet";
    type Response = AgentLauncherConfig;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &AgentConfigSetRequest {
                selected: self.selected.clone(),
                custom_command: self.custom_command.clone(),
                auto_launch: self.auto_launch,
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<AgentLauncherConfig, RpcCodecError> {
        let config: AgentConfig = decode_message(Self::METHOD, body)?;
        Ok(AgentLauncherConfig::from_proto(config))
    }
}
