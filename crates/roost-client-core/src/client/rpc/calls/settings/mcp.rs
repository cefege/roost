//! The MCP relay registry: list, create, delete.
//!
//! Called by roost-web's Settings MCP pane. v2 call sites:
//! `apps/web/src/components/Settings/McpPane.tsx:29-72` (`mcpList`,
//! `mcpCreate`, `mcpDelete`). The rows convert through the shared
//! `codec::wire_rows::mcp_relay_from_proto`, so a relay's wire shape is read in
//! one place.

use roost_proto::{
    McpCreateRequest, McpCreateResponse, McpDeleteRequest, McpDeleteResponse, McpListRequest,
    McpListResponse,
};
use roost_protocol::wire::McpRelay;

use crate::client::rpc::codec::wire_rows::mcp_relay_from_proto;
use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;

/// `McpList`: the relays the coordinator published.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ListMcpRelays;

impl UnaryMethod for ListMcpRelays {
    const METHOD: &'static str = "McpList";
    type Response = Vec<McpRelay>;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(Self::METHOD, &McpListRequest::default())
    }

    fn decode_response(body: &[u8]) -> Result<Vec<McpRelay>, RpcCodecError> {
        let response: McpListResponse = decode_message(Self::METHOD, body)?;
        Ok(response
            .relays
            .iter()
            .filter_map(|row| mcp_relay_from_proto(row).ok())
            .collect())
    }
}

/// `McpCreate`: register one relay and return it as stored.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CreateMcpRelay {
    /// The relay's display name.
    pub label: String,
    /// `stdio` or `sse`.
    pub kind: String,
    /// The relay's free-form configuration, as JSON.
    pub config_json: String,
}

impl UnaryMethod for CreateMcpRelay {
    const METHOD: &'static str = "McpCreate";
    type Response = McpRelay;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &McpCreateRequest {
                label: self.label.clone(),
                kind: self.kind.clone(),
                config_json: self.config_json.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<McpRelay, RpcCodecError> {
        let response: McpCreateResponse = decode_message(Self::METHOD, body)?;
        let row = response
            .relay
            .as_option()
            .ok_or_else(|| RpcCodecError::MalformedResponse {
                method: Self::METHOD,
                detail: "the answer carried no relay".to_owned(),
            })?;
        mcp_relay_from_proto(row).map_err(|error| RpcCodecError::MalformedResponse {
            method: Self::METHOD,
            detail: error.to_string(),
        })
    }
}

/// `McpDelete`: drop one relay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteMcpRelay {
    /// The relay's id.
    pub id: String,
}

impl UnaryMethod for DeleteMcpRelay {
    const METHOD: &'static str = "McpDelete";
    type Response = bool;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &McpDeleteRequest {
                id: self.id.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<bool, RpcCodecError> {
        let response: McpDeleteResponse = decode_message(Self::METHOD, body)?;
        Ok(response.ok)
    }
}
