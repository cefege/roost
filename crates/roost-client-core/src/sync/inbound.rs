//! The typed frame vocabulary a Sync socket delivers, and the rule that governs
//! what happens to one: apply it, then acknowledge it.
//!
//! `sync::decode` turns the protobuf into this enum; the core decides. That split
//! is why this enum exists rather than `roost_proto::FirehoseFrame` in the public
//! API — the state machine's inputs are named, and a frame kind nobody has a
//! rule for is a compile error here instead of a silently dropped `case`. Every
//! `FirehoseFrame` arm has a variant; `sync::decode::arms` is the table.
//!
//! `delivery_seq` is carried beside every frame by the host, not in here,
//! because it is a property of the transport sequence rather than of the frame:
//! it is `0` for a control frame, since controls never consume the
//! coordinator's application window (`protocol/spec/sync.md:29`).

mod payloads;

use roost_proto::{PbCellGridChunk, PbCellGridFrame};
use roost_protocol::wire::WorkspaceDelta as WireWorkspaceDelta;
use roost_protocol::wire::{McpStreamMessage, SessionMap, TaskDelta, WorkerPresenceEvent};

pub use self::payloads::{
    AuditEntry, CoordinatorRelocation, InputRouteResult, PairRequestChange, PairedBrowser,
    RoutableChunk, SessionViewer, TransportProbeResult,
};
use crate::sessions::WireEvent;
use crate::sync::link::SyncDomain;
use crate::terminal::input::InputOutcome;

/// One decoded frame from the Sync socket.
/// Only `PartialEq`: the cell messages and the session rows it carries are
/// protobuf and wire types, neither of which is `Eq`. A test that needs to
/// compare two frames compares the fields it cares about.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncFrame {
    /// The v2 announcement: this socket's identity and every domain generation.
    Subscribed {
        /// The coordinator's identity for this socket.
        socket_id: String,
        /// The worker process epoch behind it.
        process_epoch: String,
        /// `(domain, generation, subscribed)`, as announced.
        domains: Vec<(SyncDomain, u64, bool)>,
    },
    /// A domain was reset by the coordinator; its retained snapshot is gone.
    DomainReset {
        /// Which domain.
        domain: SyncDomain,
        /// The generation the reset established.
        generation: u64,
        /// The coordinator's reason string, carried through to the log.
        reason: String,
        /// Whether this client is still subscribed to the domain on this socket
        /// (`SyncDomainResetFrame.subscribed`, proto field 4). Only a subscribed
        /// domain is re-hydrated; an unsubscribed one just stops being ready.
        subscribed: bool,
    },
    /// A session-plane event, already decoded to the shared wire shape.
    SessionEvent {
        /// The event. Folded by `roost_protocol::wire::fold_event` and never by
        /// a switch in this crate.
        event: WireEvent,
        /// The durable event id, for the recovery watermark.
        event_id: u64,
    },
    /// An authoritative full session set: bootstrap, or a re-hydration.
    ///
    /// Carries the shared `SessionMap` rather than a `BTreeMap<String, _>` the
    /// core would have to re-key: converting a keyed map into branded ids is a
    /// second parse of the same rows, and a row whose id fails the brand check
    /// would be dropped silently instead of refused loudly.
    SessionsSnapshot {
        /// The complete session rows.
        sessions: SessionMap,
    },
    /// One authoritative cell frame for a session replica.
    CellGrid {
        /// The session the frame belongs to.
        session_id: String,
        /// The wire frame, still a proto message: the shared chunk assembler
        /// speaks that type, and re-encoding it here would be a second codec.
        frame: PbCellGridFrame,
    },
    /// One part of a chunked baseline.
    CellGridChunk {
        /// The session the part belongs to.
        session_id: String,
        /// The wire part.
        chunk: PbCellGridChunk,
    },
    /// A generation-matched view-state result: the authority acknowledged a
    /// view, or refused it.
    ViewState {
        /// The session.
        session_id: String,
        /// The view the command named.
        view_id: String,
        /// The generation the acknowledgement belongs to.
        generation: u64,
        /// Whether the authority holds the view.
        accepted: bool,
    },
    /// A truthful terminal-write result for one admitted input batch.
    InputResult {
        /// The session the batch was for.
        session_id: String,
        /// The batch's own sequence, which the client allocated.
        input_seq: u64,
        /// Whether the write happened, was refused, or is unknown.
        outcome: InputOutcome,
        /// The domain generation the result belongs to.
        generation: u64,
    },
    /// One agent-status report. Proto field 29, `sync.proto:286`.
    ///
    /// Carries the SHARED wire type rather than the raw proto message,
    /// because the freshness fence operates on the validated type and a
    /// second parse of the same frame in this crate is exactly what
    /// `client::agents::status_projection` refuses to do.
    AgentStatus {
        /// The report, already shape-checked.
        update: roost_protocol::wire::AgentStatusUpdate,
    },
    /// An agent-status report the shared schema refused. Consumed and applied
    /// to nothing: v2 `applyAgentStatusFrame` returns false and
    /// `_dispatchSyncFrame` ignores that return (`sync-frame.ts:274-276`), so
    /// the link stays up and the frame is acknowledged.
    AgentStatusRefused {
        /// The session the report named.
        session_id: String,
        /// The schema's refusal.
        reason: String,
    },
    /// A `sessions` JSON event that parsed as JSON but not as a session event.
    /// The recovery cursor still advances past it and nothing is folded: v2
    /// moves `_lastSeenEventId` before `foldEventIntoStore` rejects the shape
    /// (`sync-frame.ts:115-122`, `projector.ts:97-108`).
    SessionEventRejected {
        /// The payload's `_event_id`, or `0` when it carried none.
        event_id: u64,
        /// The schema's refusal.
        reason: String,
    },
    /// Who is looking at a session: `session_presence` of kind `viewers`.
    SessionViewers {
        /// The session.
        session_id: String,
        /// Every viewer, replacing the previous list.
        viewers: Vec<SessionViewer>,
    },
    /// Any other `session_presence` notice, opaque by construction: v2 hands
    /// the parsed payload to the session's registered presence handler
    /// (`sync-dispatch.ts:18-20`).
    SessionPresence {
        /// The session.
        session_id: String,
        /// The parsed payload.
        payload: serde_json::Value,
    },
    /// One `audit_log` insert.
    AuditRow {
        /// The row.
        row: AuditEntry,
    },
    /// One workspace change, in the shared wire shape.
    WorkspaceDelta {
        /// The change.
        delta: WireWorkspaceDelta,
    },
    /// One task row change, in the shared wire shape.
    TaskDelta {
        /// The change.
        delta: TaskDelta,
    },
    /// One MCP registry change or relay event, in the shared wire shape.
    McpMessage {
        /// The change or event.
        message: McpStreamMessage,
    },
    /// One worker registration, heartbeat or removal, in the shared wire shape.
    WorkerPresence {
        /// The presence event.
        event: WorkerPresenceEvent,
    },
    /// The routable worker set: a live full replacement, or one chunk of a
    /// retained seed.
    WorkerRoutable {
        /// The fingerprints this frame carries.
        fps: Vec<String>,
        /// Where the fingerprints sit in a chunked seed, or `None` for a live
        /// full-set replacement.
        chunk: Option<RoutableChunk>,
    },
    /// The coordinator-parsed OSC title of a session's terminal.
    TerminalTitle {
        /// The session.
        session_id: String,
        /// The title.
        title: String,
    },
    /// The coordinator-stamped last-activity time of a session.
    LastActivity {
        /// The session.
        session_id: String,
        /// Milliseconds since the epoch.
        ts_ms: i64,
    },
    /// One pair-request change.
    PairRequestDelta {
        /// The change.
        change: PairRequestChange,
    },
    /// A peer tab's UI report. Browser tabs deliberately do not project peer
    /// UI state: routing and discarding it is its full consumption
    /// (`sync-frame.ts:329-333`).
    UiState,
    /// A UI command for this tab's UI bridge, still the proto message: v2 hands
    /// the frame itself to `_dispatchUiCommand` (`sync-frame.ts:334-340`).
    UiCommand {
        /// The command frame.
        command: roost_proto::UiCommandFrame,
    },
    /// A coordinator relocation notice.
    CoordinatorRelocation {
        /// The handoff.
        relocation: CoordinatorRelocation,
    },
    /// The answer to an input-route claim this socket sent.
    InputRouteResult {
        /// The answer.
        result: InputRouteResult,
    },
    /// The answer to a transport probe this socket sent.
    TransportProbeResult {
        /// The answer.
        result: TransportProbeResult,
    },
    /// A timestamp-only liveness frame.
    Keepalive,
}

impl SyncFrame {
    /// The domain this frame belongs to, or `None` when it is not domain-bound.
    ///
    /// `None` means the frame rides whatever domain the host already
    /// established — it is not an exemption from the readiness gate, and
    /// `SyncState::may_apply` still consults the domain table for it. The
    /// assignment is v2's (`apps/coord/src/sync/sync-feed-frames.ts:130-165`):
    /// session-keyed metadata is terminal, registry deltas are their own
    /// domain, and every control is `None`.
    pub const fn domain(&self) -> Option<SyncDomain> {
        match self {
            Self::DomainReset { domain, .. } => Some(*domain),
            Self::CellGrid { .. }
            | Self::CellGridChunk { .. }
            | Self::AgentStatus { .. }
            | Self::AgentStatusRefused { .. }
            | Self::SessionViewers { .. }
            | Self::SessionPresence { .. }
            | Self::TerminalTitle { .. }
            | Self::LastActivity { .. } => Some(SyncDomain::Terminal),
            Self::WorkerPresence { .. } | Self::WorkerRoutable { .. } => Some(SyncDomain::Workers),
            Self::WorkspaceDelta { .. } => Some(SyncDomain::Workspaces),
            Self::TaskDelta { .. } => Some(SyncDomain::Tasks),
            Self::McpMessage { .. } => Some(SyncDomain::Mcp),
            Self::PairRequestDelta { .. } => Some(SyncDomain::Pair),
            Self::AuditRow { .. } => Some(SyncDomain::Audit),
            Self::Subscribed { .. }
            | Self::SessionEvent { .. }
            | Self::SessionEventRejected { .. }
            | Self::SessionsSnapshot { .. }
            | Self::ViewState { .. }
            | Self::InputResult { .. }
            | Self::UiState
            | Self::UiCommand { .. }
            | Self::CoordinatorRelocation { .. }
            | Self::InputRouteResult { .. }
            | Self::TransportProbeResult { .. }
            | Self::Keepalive => None,
        }
    }

    /// A short name for the incident log.
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::Subscribed { .. } => "subscribed",
            Self::DomainReset { .. } => "domain_reset",
            Self::SessionEvent { .. } => "session_event",
            Self::SessionEventRejected { .. } => "session_event_rejected",
            Self::SessionsSnapshot { .. } => "sessions_snapshot",
            Self::CellGrid { .. } => "cell_grid",
            Self::CellGridChunk { .. } => "cell_grid_chunk",
            Self::ViewState { .. } => "view_state",
            Self::InputResult { .. } => "input_result",
            Self::AgentStatus { .. } => "agent_status",
            Self::AgentStatusRefused { .. } => "agent_status_refused",
            Self::SessionViewers { .. } => "session_viewers",
            Self::SessionPresence { .. } => "session_presence",
            Self::AuditRow { .. } => "audit_row",
            Self::WorkspaceDelta { .. } => "workspace_delta",
            Self::TaskDelta { .. } => "task_delta",
            Self::McpMessage { .. } => "mcp_msg",
            Self::WorkerPresence { .. } => "worker_presence",
            Self::WorkerRoutable { .. } => "worker_routable",
            Self::TerminalTitle { .. } => "terminal_title",
            Self::LastActivity { .. } => "last_activity",
            Self::PairRequestDelta { .. } => "pair_request_delta",
            Self::UiState => "ui_state",
            Self::UiCommand { .. } => "ui_command",
            Self::CoordinatorRelocation { .. } => "coordinator_relocation",
            Self::InputRouteResult { .. } => "input_route_result",
            Self::TransportProbeResult { .. } => "terminal_transport_probe_result",
            Self::Keepalive => "keepalive",
        }
    }

    /// The session this frame names, for routing it to a replica.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::CellGrid { session_id, .. }
            | Self::CellGridChunk { session_id, .. }
            | Self::ViewState { session_id, .. }
            | Self::InputResult { session_id, .. }
            | Self::AgentStatusRefused { session_id, .. }
            | Self::SessionViewers { session_id, .. }
            | Self::SessionPresence { session_id, .. }
            | Self::TerminalTitle { session_id, .. }
            | Self::LastActivity { session_id, .. } => Some(session_id),
            Self::InputRouteResult { result } => Some(result.session_id.as_str()),
            Self::AgentStatus { update, .. } => Some(update.common.session_id.as_str()),
            _ => None,
        }
    }
}
