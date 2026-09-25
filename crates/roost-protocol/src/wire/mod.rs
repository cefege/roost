//! The wire contract in Rust: branded identities, the entities, the append-only
//! session event union, the canonical fold, and the two JSON websocket frame
//! unions.
//!
//! `event` is the reason this module exists. `fold_event` is the only place a
//! session row is ever produced, in every component of every front end. A
//! second fold anywhere else is the defect this module exists to prevent.

pub mod agent_status;
pub mod brand;
pub mod control;
pub mod coord_worker;
pub mod event;
pub mod event_proto;
pub mod headers;
pub mod mcp;
pub mod session;
pub mod session_proto;
pub mod sync_ws;
pub mod task;
pub mod worker;
pub mod workspace;

pub use agent_status::{
    AGENT_ID_MAX_LENGTH, AGENT_STATUS_MESSAGE_MAX_LENGTH, AgentId, AgentOccupantId,
    AgentRuntimeState, AgentStatus, AgentStatusFields, AgentStatusIdentity, AgentStatusSource,
    AgentStatusUpdate, StatusEpoch, is_identified_agent_status,
};
pub use brand::{ChannelId, McpRelayId, SessionId, TaskId, TraceId, WorkerFp, WorkspaceId};
pub use event::{SessionEvent, SessionMap, fold_all, fold_event};
pub use event_proto::{DecodedEvent, event_to_proto, proto_to_event};
pub use mcp::{McpRelay, McpRelayDelta, McpRelayEvent, McpRelayKind, McpStreamMessage};
pub use session::{PullRequestChecks, PullRequestState, Session, SessionKind, SessionStatus};
pub use session_proto::{session_from_proto, session_to_proto};
pub use task::{Task, TaskDelta, TaskState};
pub use worker::{
    HOST_IDENTITY_VALUE_MAX_UTF8_BYTES, HostIdentity, HostMetrics, TerminalCoreCapacityReport,
    Worker, WorkerOs, WorkerPresenceEvent, normalize_host_identity, normalize_host_identity_text,
};
pub use workspace::{Workspace, WorkspaceDelta};
