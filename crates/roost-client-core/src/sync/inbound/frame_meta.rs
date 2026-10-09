//! What a decoded Sync frame says about itself: the domain whose readiness
//! gates it, a short name for the incident log, and the session it routes to.
//! Split from `inbound.rs`, which owns the frame enum itself.

use super::SyncFrame;
use crate::sync::link::SyncDomain;

impl SyncFrame {
    /// The domain this frame belongs to, or `None` when it is not domain-bound.
    ///
    /// `None` means the frame rides whatever domain the host already
    /// established — it is not an exemption from the readiness gate, and
    /// `SyncState::may_apply` still consults the domain table for it. The
    /// assignment is v2's (`apps/coord/src/sync/sync-feed-frames.ts:130-165`):
    /// session-keyed metadata is terminal, registry deltas are their own
    /// domain, and every control is `None`. The session PLANE — its snapshot
    /// and its events — belongs to the terminal domain too, because the
    /// terminal domain's `SessionsList` is what seeds it.
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
            | Self::TerminalClipboard { .. }
            | Self::CommandFinished { .. }
            | Self::TerminalBell { .. }
            | Self::TerminalSignals { .. }
            | Self::LastActivity { .. } => Some(SyncDomain::Terminal),
            Self::WorkerPresence { .. } | Self::WorkerRoutable { .. } => Some(SyncDomain::Workers),
            Self::WorkspaceDelta { .. } => Some(SyncDomain::Workspaces),
            Self::TaskDelta { .. } => Some(SyncDomain::Tasks),
            Self::McpMessage { .. } => Some(SyncDomain::Mcp),
            Self::PairRequestDelta { .. } => Some(SyncDomain::Pair),
            Self::AuditRow { .. } => Some(SyncDomain::Audit),
            // The session plane IS the terminal domain's snapshot and its live
            // deltas. Answering `None` for them put them on the "any ready
            // domain" arm of `may_apply`, so a session event could be folded
            // while the terminal domain was still waiting for the very snapshot
            // that seeds the plane — and that snapshot then replaced the plane
            // and deleted it. Naming the domain puts them back under the gate
            // that already exists for exactly this.
            Self::SessionEvent { .. }
            | Self::SessionEventRejected { .. }
            | Self::SessionsSnapshot { .. } => Some(SyncDomain::Terminal),
            Self::ClipboardHistory { .. } => Some(SyncDomain::Pair),
            Self::Subscribed { .. }
            | Self::InputRouteResult { .. }
            | Self::ViewState { .. }
            | Self::InputResult { .. }
            | Self::UiState
            | Self::UiCommand { .. }
            | Self::CoordinatorRelocation { .. }
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
            Self::TerminalClipboard { .. } => "terminal_clipboard",
            Self::ClipboardHistory { .. } => "clipboard_history",
            Self::CommandFinished { .. } => "terminal_command_finished",
            Self::TerminalBell { .. } => "terminal_bell",
            Self::TerminalSignals { .. } => "terminal_signals",
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
            | Self::TerminalClipboard { session_id, .. }
            | Self::CommandFinished { session_id, .. }
            | Self::TerminalBell { session_id }
            | Self::TerminalSignals { session_id, .. }
            | Self::LastActivity { session_id, .. } => Some(session_id),
            Self::InputRouteResult { result } => Some(result.session_id.as_str()),
            Self::AgentStatus { update, .. } => Some(update.common.session_id.as_str()),
            _ => None,
        }
    }
}
