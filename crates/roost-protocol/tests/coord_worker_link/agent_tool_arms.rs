//! Fixture vectors for coordinator-worker agent-tool call and result frames.
//! The shared codec fixture list calls these to round-trip both directions.
//! These fixtures depend only on the protocol wire enums.

use roost_protocol::wire::coord_worker::{CoordWorkerDownstream, CoordWorkerUpstream};

pub fn upstream_arms() -> Vec<(&'static str, CoordWorkerUpstream)> {
    vec![
        (
            "agent-tool-output",
            CoordWorkerUpstream::AgentToolOutput(Default::default()),
        ),
        (
            "agent-tool-result",
            CoordWorkerUpstream::AgentToolResult(Default::default()),
        ),
    ]
}

pub fn downstream_arms() -> Vec<(&'static str, CoordWorkerDownstream)> {
    vec![
        (
            "agent-tool-call",
            CoordWorkerDownstream::AgentToolCall(Default::default()),
        ),
        (
            "agent-tool-cancel",
            CoordWorkerDownstream::AgentToolCancel(Default::default()),
        ),
        (
            "agent-conversation-closed",
            CoordWorkerDownstream::AgentConversationClosed(Default::default()),
        ),
    ]
}
