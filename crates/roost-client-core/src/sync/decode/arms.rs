//! Every `FirehoseFrame` oneof arm, with its proto field and the lane v2 puts
//! it on: an unsequenced control, or a sequenced application frame of one
//! domain.
//!
//! Read by `decode` for the meta rule and by the proto-divergence test, which
//! diffs this table against `protocol/proto/roost/v1/sync.proto`. The lanes are
//! v2's (`apps/coord/src/sync/sync-feed-frames.ts:130-165`) and the Rust
//! coordinator's (`crates/roost-coord/src/sync_ws/frame_meta.rs`); the two agree.

use roost_proto::__buffa::oneof::firehose_frame::Frame;

use crate::sync::link::SyncDomain;

/// Which lane an arm rides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmLane {
    /// `delivery_seq = 0`, `domain = UNSPECIFIED`, `domain_generation = 0`.
    Control,
    /// Sequenced, and owned by one domain.
    Application(SyncDomain),
}

/// One oneof arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirehoseArm {
    /// The proto field number.
    pub field: u32,
    /// The proto field name.
    pub name: &'static str,
    /// The lane it rides.
    pub lane: ArmLane,
}

const fn control(field: u32, name: &'static str) -> FirehoseArm {
    FirehoseArm {
        field,
        name,
        lane: ArmLane::Control,
    }
}

const fn application(field: u32, name: &'static str, domain: SyncDomain) -> FirehoseArm {
    FirehoseArm {
        field,
        name,
        lane: ArmLane::Application(domain),
    }
}

const SESSIONS: FirehoseArm = application(1, "sessions", SyncDomain::Terminal);
const SESSION_PRESENCE: FirehoseArm = application(8, "session_presence", SyncDomain::Terminal);
const AUDIT_ROW: FirehoseArm = application(10, "audit_row", SyncDomain::Audit);
const SESSION_EVENT: FirehoseArm = application(11, "session_event", SyncDomain::Terminal);
const WORKSPACE_DELTA: FirehoseArm = application(12, "workspace_delta", SyncDomain::Workspaces);
const TASK_DELTA: FirehoseArm = application(13, "task_delta", SyncDomain::Tasks);
const MCP_MSG: FirehoseArm = application(16, "mcp_msg", SyncDomain::Mcp);
const WORKER_PRESENCE: FirehoseArm = application(17, "worker_presence", SyncDomain::Workers);
const WORKER_ROUTABLE: FirehoseArm = application(19, "worker_routable", SyncDomain::Workers);
const CELL_GRID: FirehoseArm = application(20, "cell_grid", SyncDomain::Terminal);
const TERMINAL_TITLE: FirehoseArm = application(21, "terminal_title", SyncDomain::Terminal);
const LAST_ACTIVITY: FirehoseArm = application(22, "last_activity", SyncDomain::Terminal);
const PAIR_REQUEST_DELTA: FirehoseArm = application(23, "pair_request_delta", SyncDomain::Pair);
const UI_STATE: FirehoseArm = control(24, "ui_state");
const UI_COMMAND: FirehoseArm = control(25, "ui_command");
const KEEPALIVE: FirehoseArm = control(26, "keepalive");
const COORDINATOR_RELOCATION: FirehoseArm = control(27, "coordinator_relocation");
const AGENT_STATUS: FirehoseArm = application(29, "agent_status", SyncDomain::Terminal);
const CELL_GRID_CHUNK: FirehoseArm = application(34, "cell_grid_chunk", SyncDomain::Terminal);
const SUBSCRIBED: FirehoseArm = control(40, "subscribed");
const DOMAIN_RESET: FirehoseArm = control(41, "domain_reset");
const INPUT_ACCEPTED: FirehoseArm = control(44, "input_accepted");
const INPUT_REJECTED: FirehoseArm = control(45, "input_rejected");
const INPUT_AMBIGUOUS: FirehoseArm = control(46, "input_ambiguous");
const TERMINAL_VIEW_STATE: FirehoseArm =
    application(48, "terminal_view_state", SyncDomain::Terminal);
const INPUT_ROUTE_RESULT: FirehoseArm = control(49, "input_route_result");
const TERMINAL_TRANSPORT_PROBE_RESULT: FirehoseArm = control(50, "terminal_transport_probe_result");
const TERMINAL_CLIPBOARD: FirehoseArm = application(51, "terminal_clipboard", SyncDomain::Terminal);

const COMMAND_FINISHED: FirehoseArm =
    application(52, "terminal_command_finished", SyncDomain::Terminal);
const CLIPBOARD_HISTORY: FirehoseArm = application(53, "clipboard_history", SyncDomain::Pair);
const TERMINAL_BELL: FirehoseArm = application(54, "terminal_bell", SyncDomain::Terminal);
const TERMINAL_SIGNALS: FirehoseArm = application(55, "terminal_signals", SyncDomain::Terminal);
const AGENT_CONVERSATION: FirehoseArm = control(56, "agent_conversation");
const AGENT_CHAT_EVENTS: FirehoseArm = control(57, "agent_chat_events");
/// Every arm, in field order. All of them are mapped: `decode::map_arm` has no
/// wildcard, so an arm with no row here cannot have a mapping either.
pub const FIREHOSE_ARMS: [FirehoseArm; 34] = [
    SESSIONS,
    SESSION_PRESENCE,
    AUDIT_ROW,
    SESSION_EVENT,
    WORKSPACE_DELTA,
    TASK_DELTA,
    MCP_MSG,
    WORKER_PRESENCE,
    WORKER_ROUTABLE,
    CELL_GRID,
    TERMINAL_TITLE,
    LAST_ACTIVITY,
    PAIR_REQUEST_DELTA,
    UI_STATE,
    UI_COMMAND,
    KEEPALIVE,
    COORDINATOR_RELOCATION,
    AGENT_STATUS,
    CELL_GRID_CHUNK,
    SUBSCRIBED,
    DOMAIN_RESET,
    INPUT_ACCEPTED,
    INPUT_REJECTED,
    INPUT_AMBIGUOUS,
    TERMINAL_VIEW_STATE,
    INPUT_ROUTE_RESULT,
    TERMINAL_TRANSPORT_PROBE_RESULT,
    TERMINAL_CLIPBOARD,
    CLIPBOARD_HISTORY,
    COMMAND_FINISHED,
    TERMINAL_BELL,
    TERMINAL_SIGNALS,
    AGENT_CONVERSATION,
    AGENT_CHAT_EVENTS,
];

/// The table row for a decoded arm.
pub const fn arm_of(frame: &Frame) -> FirehoseArm {
    match frame {
        Frame::Sessions(_) => SESSIONS,
        Frame::SessionPresence(_) => SESSION_PRESENCE,
        Frame::AuditRow(_) => AUDIT_ROW,
        Frame::SessionEvent(_) => SESSION_EVENT,
        Frame::WorkspaceDelta(_) => WORKSPACE_DELTA,
        Frame::TaskDelta(_) => TASK_DELTA,
        Frame::McpMsg(_) => MCP_MSG,
        Frame::WorkerPresence(_) => WORKER_PRESENCE,
        Frame::WorkerRoutable(_) => WORKER_ROUTABLE,
        Frame::CellGrid(_) => CELL_GRID,
        Frame::TerminalTitle(_) => TERMINAL_TITLE,
        Frame::LastActivity(_) => LAST_ACTIVITY,
        Frame::PairRequestDelta(_) => PAIR_REQUEST_DELTA,
        Frame::UiState(_) => UI_STATE,
        Frame::UiCommand(_) => UI_COMMAND,
        Frame::Keepalive(_) => KEEPALIVE,
        Frame::CoordinatorRelocation(_) => COORDINATOR_RELOCATION,
        Frame::AgentStatus(_) => AGENT_STATUS,
        Frame::CellGridChunk(_) => CELL_GRID_CHUNK,
        Frame::Subscribed(_) => SUBSCRIBED,
        Frame::DomainReset(_) => DOMAIN_RESET,
        Frame::InputAccepted(_) => INPUT_ACCEPTED,
        Frame::InputRejected(_) => INPUT_REJECTED,
        Frame::InputAmbiguous(_) => INPUT_AMBIGUOUS,
        Frame::TerminalViewState(_) => TERMINAL_VIEW_STATE,
        Frame::InputRouteResult(_) => INPUT_ROUTE_RESULT,
        Frame::TerminalTransportProbeResult(_) => TERMINAL_TRANSPORT_PROBE_RESULT,
        Frame::TerminalClipboard(_) => TERMINAL_CLIPBOARD,
        Frame::ClipboardHistory(_) => CLIPBOARD_HISTORY,
        Frame::TerminalCommandFinished(_) => COMMAND_FINISHED,
        Frame::TerminalBell(_) => TERMINAL_BELL,
        Frame::TerminalSignals(_) => TERMINAL_SIGNALS,
        Frame::AgentConversation(_) => AGENT_CONVERSATION,
        Frame::AgentChatEvents(_) => AGENT_CHAT_EVENTS,
    }
}
