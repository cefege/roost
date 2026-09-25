//! The MCP relay registry: coordinator-persisted rows plus the opaque payloads
//! workers publish onto the `mcp` stream.
//!
//! Registry deltas and relay events travel on the same stream, so the union is
//! discriminated the way the wire discriminates it: by the presence of `kind`.
//! An event carries no registry identity of its own beyond its `relay_id`.
//!
//! `config` and `payload` stay free-form. The coordinator stores a relay's
//! configuration and never reads a field out of it; only the worker that
//! subscribed to the relay interprets either.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::validate::{integer_in_range, non_empty};
use crate::wire::brand::McpRelayId;
use crate::{ProtocolError, ProtocolResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpRelayKind {
    Stdio,
    Sse,
}

impl McpRelayKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::Sse => "sse",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpRelay {
    pub id: McpRelayId,
    pub label: String,
    pub kind: McpRelayKind,
    /// Free-form JSON the worker interprets.
    pub config: Map<String, Value>,
    pub created_at_ms: i64,
}

impl McpRelay {
    pub fn parse(value: Value) -> ProtocolResult<Self> {
        let relay: McpRelay = serde_json::from_value(value)
            .map_err(|error| ProtocolError::new("mcp_relay", error.to_string()))?;
        relay.check()?;
        Ok(relay)
    }

    pub fn check(&self) -> ProtocolResult<()> {
        non_empty("mcp_relay.label", &self.label)?;
        integer_in_range("mcp_relay.created_at_ms", self.created_at_ms, 1, i64::MAX)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpRelayEvent {
    pub relay_id: McpRelayId,
    pub payload: Value,
    pub ts: i64,
}

impl McpRelayEvent {
    pub fn parse(value: Value) -> ProtocolResult<Self> {
        let event: McpRelayEvent = serde_json::from_value(value)
            .map_err(|error| ProtocolError::new("mcp_relay_event", error.to_string()))?;
        integer_in_range("mcp_relay_event.ts", event.ts, 1, i64::MAX)?;
        Ok(event)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpRelayDelta {
    Created { relay: McpRelay },
    Updated { relay: McpRelay },
    Deleted { id: McpRelayId },
}

impl McpRelayDelta {
    pub fn parse(value: Value) -> ProtocolResult<Self> {
        let delta: McpRelayDelta = serde_json::from_value(value)
            .map_err(|error| ProtocolError::new("mcp_relay_delta", error.to_string()))?;
        match &delta {
            McpRelayDelta::Created { relay } | McpRelayDelta::Updated { relay } => relay.check()?,
            McpRelayDelta::Deleted { .. } => {}
        }
        Ok(delta)
    }
}

/// The carrier type for the relay event bus: either a registry delta or a
/// published relay event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum McpStreamMessage {
    Delta(McpRelayDelta),
    Event(McpRelayEvent),
}

impl McpStreamMessage {
    /// Discriminate the way the wire does, by the presence of `kind`, so a
    /// malformed message is reported against the shape it actually claimed.
    pub fn parse(value: Value) -> ProtocolResult<Self> {
        if value.get("kind").is_some() {
            McpRelayDelta::parse(value).map(Self::Delta)
        } else {
            McpRelayEvent::parse(value).map(Self::Event)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELAY: &str = "00000000-0000-4000-8000-0000000000c1";

    fn relay_json() -> Value {
        serde_json::json!({
            "id": RELAY,
            "label": "docs",
            "kind": "stdio",
            "config": { "command": "docs-mcp" },
            "created_at_ms": 5,
        })
    }

    #[test]
    fn a_relay_row_parses() {
        let relay = McpRelay::parse(relay_json()).unwrap();
        assert_eq!(relay.kind, McpRelayKind::Stdio);
        assert_eq!(relay.config["command"], Value::from("docs-mcp"));
    }

    #[test]
    fn an_unlabelled_relay_is_rejected() {
        let mut value = relay_json();
        value["label"] = Value::from("");
        assert_eq!(McpRelay::parse(value).unwrap_err().field, "mcp_relay.label");
    }

    #[test]
    fn a_registry_delta_and_a_relay_event_share_one_stream() {
        let delta = McpStreamMessage::parse(serde_json::json!({
            "kind": "deleted",
            "id": RELAY,
        }))
        .unwrap();
        assert!(matches!(
            delta,
            McpStreamMessage::Delta(McpRelayDelta::Deleted { .. })
        ));

        let event = McpStreamMessage::parse(serde_json::json!({
            "relay_id": RELAY,
            "payload": { "any": [1, 2] },
            "ts": 7,
        }))
        .unwrap();
        let McpStreamMessage::Event(event) = event else {
            panic!("a payload without kind is a relay event");
        };
        assert_eq!(event.ts, 7);
        assert_eq!(
            serde_json::to_value(&event).unwrap()["payload"],
            serde_json::json!({"any": [1, 2]})
        );
    }

    #[test]
    fn a_relay_event_without_a_timestamp_is_rejected() {
        let value = serde_json::json!({ "relay_id": RELAY, "payload": null, "ts": 0 });
        assert_eq!(
            McpStreamMessage::parse(value).unwrap_err().field,
            "mcp_relay_event.ts"
        );
    }
}
