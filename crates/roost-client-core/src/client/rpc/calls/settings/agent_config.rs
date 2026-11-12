//! OMP auto-launch configuration shared across paired devices.
//! The Settings launcher pane reads/writes this coordinator-owned switch.
//! Agent selection and custom commands are not part of the contract.

use roost_proto::{AgentConfig, AgentConfigGetRequest, AgentConfigSetRequest};

use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;

/// The launch configuration, as the coordinator holds it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentLauncherConfig {
    /// Whether a new terminal auto-launches the agent.
    pub auto_launch: bool,
}

impl AgentLauncherConfig {
    /// The coordinator's own answer, unchanged.
    fn from_proto(config: AgentConfig) -> Self {
        Self {
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
