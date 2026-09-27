//! Bus payload → `FirehoseFrame` for the domains whose bus message is already a
//! domain value: durable sessions, workspaces, tasks, MCP relays, pair
//! requests, audit rows, agent status and terminal titles.
//!
//! Ported from `apps/coord/src/sync/sync-feed-frames.ts:169-393`. Each function
//! is TOTAL where v2's was partial: `workspaceFrame`, `mcpFrame` and
//! `presenceFrame` returned `null` for a payload they did not recognise, because
//! the bus carried a union typed `unknown` at that boundary. The Rust buses
//! carry closed enums, so every message they accept has a frame and the
//! `null`-and-fall-back-to-`JsonEvent` path has no states left to serve.
//!
//! The worker registry's two frames are in `worker_frames.rs` and the browser
//! UI stream's in `ui.rs`, because those two fan out under rules the plain
//! domains do not have.

use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::__buffa::oneof::mcp_stream_message_proto::Kind as McpKind;
use roost_proto::__buffa::oneof::pair_request_delta_proto::Kind as PairKind;
use roost_proto::__buffa::oneof::task_delta_proto::Kind as TaskKind;
use roost_proto::__buffa::oneof::workspace_delta_proto::Kind as WorkspaceKind;
use roost_proto::{
    AgentStatusFrame, AuditRow as PbAuditRow, FirehoseFrame, McpRelayEvent, McpStreamMessageProto,
    PairCompleted, PairRequest, PairRequestDeltaProto, TaskDeltaProto, TerminalTitleFrame,
    Workspace as PbWorkspace, WorkspaceDeltaProto, WorkspaceSessionsSet,
};
use roost_protocol::wire::{
    AgentStatusUpdate, McpRelayDelta, McpStreamMessage, WorkspaceDelta, event_to_proto,
};

use crate::events::bus_messages::{
    AuditRow, PairRequestDelta, SessionBusMessage, SessionTitleUpdate, TaskBusMsg, TaskBusMsgKind,
};
use crate::events::visibility::kind_is_public;
use crate::sync_ws::feed::{FeedFrame, FeedRefusal, as_f64, as_u32, as_u64};

/// One durable session event as the frame a browser folds.
///
/// The private kind is refused HERE rather than at the publisher, because this
/// is the boundary a value crosses into a browser lane and a caller holding a
/// batch of events must be able to drop this one and keep the rest
/// (`sync-feed-frames.ts:170-180`).
pub fn session_message_frame(message: &SessionBusMessage) -> Result<FeedFrame, FeedRefusal> {
    let kind = message.event.kind_name();
    if !kind_is_public(kind) {
        return Err(FeedRefusal::PrivateSessionEvent { kind });
    }
    let event = event_to_proto(&message.event, message.event_id.unwrap_or(0))?;
    Ok(FeedFrame::of(FirehoseFrame {
        frame: Some(Frame::SessionEvent(Box::new(event))),
        ..FirehoseFrame::default()
    }))
}

/// One workspace change as its frame.
pub fn workspace_frame(delta: &WorkspaceDelta) -> FeedFrame {
    let kind = match delta {
        WorkspaceDelta::Created { workspace } => {
            WorkspaceKind::Created(Box::new(workspace_to_proto(workspace)))
        }
        WorkspaceDelta::Updated { workspace } => {
            WorkspaceKind::Updated(Box::new(workspace_to_proto(workspace)))
        }
        WorkspaceDelta::Deleted { id } => WorkspaceKind::DeletedId(id.as_str().to_owned()),
        WorkspaceDelta::SessionsSet {
            id,
            session_ids,
            version,
        } => WorkspaceKind::SessionsSet(Box::new(WorkspaceSessionsSet {
            workspace_id: id.as_str().to_owned(),
            session_ids: session_ids
                .iter()
                .map(|id| id.as_str().to_owned())
                .collect(),
            version: as_u64(*version),
            ..WorkspaceSessionsSet::default()
        })),
    };
    FeedFrame::of(FirehoseFrame {
        frame: Some(Frame::WorkspaceDelta(Box::new(WorkspaceDeltaProto {
            kind: Some(kind),
            ..WorkspaceDeltaProto::default()
        }))),
        ..FirehoseFrame::default()
    })
}

/// One task row change as its frame. The row arrives already in its generated
/// wire shape, so this is a wrapper and not a mapping.
pub fn task_frame(message: &TaskBusMsg) -> FeedFrame {
    let kind = match message.kind {
        TaskBusMsgKind::Created => TaskKind::Created(Box::new(message.task.clone())),
        TaskBusMsgKind::State => TaskKind::State(Box::new(message.task.clone())),
    };
    FeedFrame::of(FirehoseFrame {
        frame: Some(Frame::TaskDelta(Box::new(TaskDeltaProto {
            kind: Some(kind),
            ..TaskDeltaProto::default()
        }))),
        ..FirehoseFrame::default()
    })
}

/// One MCP registry change or relay event as its frame.
pub fn mcp_frame(message: &McpStreamMessage) -> Result<FeedFrame, FeedRefusal> {
    let kind = match message {
        McpStreamMessage::Delta(delta) => match delta {
            McpRelayDelta::Created { relay } => McpKind::Created(Box::new(relay_to_proto(relay)?)),
            McpRelayDelta::Updated { relay } => McpKind::Updated(Box::new(relay_to_proto(relay)?)),
            McpRelayDelta::Deleted { id } => McpKind::DeletedId(id.as_str().to_owned()),
        },
        McpStreamMessage::Event(event) => McpKind::Event(Box::new(McpRelayEvent {
            relay_id: event.relay_id.as_str().to_owned(),
            payload_json: json_text("mcp_relay_event.payload", &event.payload)?,
            ts: as_u64(event.ts),
            ..McpRelayEvent::default()
        })),
    };
    Ok(FeedFrame::of(FirehoseFrame {
        frame: Some(Frame::McpMsg(Box::new(McpStreamMessageProto {
            kind: Some(kind),
            ..McpStreamMessageProto::default()
        }))),
        ..FirehoseFrame::default()
    }))
}

/// One `audit_log` insert as its frame. `caller_label` is absent on every value
/// this bus carries; see `events::bus_messages::AuditRow`.
pub fn audit_frame(row: &AuditRow) -> FeedFrame {
    FeedFrame::of(FirehoseFrame {
        frame: Some(Frame::AuditRow(Box::new(PbAuditRow {
            id: as_u64(row.id),
            ts: as_u64(row.ts),
            caller_fp: row.caller_fp.clone(),
            caller_label: row.caller_label.clone(),
            method: row.method.clone(),
            path: row.path.clone(),
            status: as_u32(row.status),
            trace_id: row.trace_id.clone(),
            ..PbAuditRow::default()
        }))),
        ..FirehoseFrame::default()
    })
}

/// One pair-request change as its frame.
///
/// `removed` and `completed` are the two arms a browser cannot confuse: a
/// removal drops by `ephemeral_id` and a completion is a notice that never
/// upserts, so a client that merged them would resurrect a denied request.
pub fn pair_frame(delta: &PairRequestDelta) -> FeedFrame {
    let kind = match delta {
        PairRequestDelta::Pending {
            ephemeral_id,
            label,
            created_at_ms,
            user_agent,
            client_browser,
            client_os,
            client_device_type,
            source_ip,
            country_code,
            region,
            city,
            edge_identity_provider,
            edge_identity,
            edge_identity_verified,
            expires_at_ms,
        } => PairKind::Pending(Box::new(PairRequest {
            ephemeral_id: ephemeral_id.clone(),
            label: label.clone(),
            created_at_ms: as_u64(*created_at_ms),
            user_agent: user_agent.clone(),
            client_browser: client_browser.clone(),
            client_os: client_os.clone(),
            client_device_type: client_device_type.clone(),
            source_ip: source_ip.clone(),
            country_code: country_code.clone(),
            region: region.clone(),
            city: city.clone(),
            edge_identity_provider: edge_identity_provider.clone(),
            edge_identity: edge_identity.clone(),
            edge_identity_verified: *edge_identity_verified,
            expires_at_ms: as_u64(*expires_at_ms),
            ..PairRequest::default()
        })),
        PairRequestDelta::Removed { ephemeral_id } => PairKind::RemovedId(ephemeral_id.clone()),
        PairRequestDelta::Completed {
            ephemeral_id,
            label,
            client_browser,
            client_os,
            client_device_type,
            country_code,
            region,
            city,
            paired_at_ms,
        } => PairKind::Completed(Box::new(PairCompleted {
            ephemeral_id: ephemeral_id.clone(),
            label: label.clone(),
            client_browser: client_browser.clone(),
            client_os: client_os.clone(),
            client_device_type: client_device_type.clone(),
            country_code: country_code.clone(),
            region: region.clone(),
            city: city.clone(),
            paired_at_ms: as_u64(*paired_at_ms),
            ..PairCompleted::default()
        })),
    };
    FeedFrame::of(FirehoseFrame {
        frame: Some(Frame::PairRequestDelta(Box::new(PairRequestDeltaProto {
            kind: Some(kind),
            ..PairRequestDeltaProto::default()
        }))),
        ..FirehoseFrame::default()
    })
}

/// One agent status update as its frame.
///
/// An inactive update is a DELETE for the browser's retained row, not a state,
/// and `active` is what says which -- so the flag travels rather than being
/// inferred from an empty agent id.
pub fn agent_status_frame(status: &AgentStatusUpdate) -> FeedFrame {
    let common = &status.common;
    FeedFrame::of(FirehoseFrame {
        frame: Some(Frame::AgentStatus(Box::new(AgentStatusFrame {
            session_id: common.session_id.as_str().to_owned(),
            agent_id: common.agent_id.as_str().to_owned(),
            state: common.state.as_str().to_owned(),
            message: common.message.clone(),
            revision: as_u64(common.revision),
            completed_revision: as_u64(common.completed_revision),
            updated_at: as_f64(common.updated_at),
            active: status.active,
            status_epoch: common
                .status_epoch
                .as_ref()
                .map(|epoch| epoch.as_str().to_owned()),
            occupant_id: common.occupant_id.as_ref().map(|id| id.as_str().to_owned()),
            source: common.source.map(|source| source.as_str().to_owned()),
            occupant_exited: common.occupant_exited,
            ..AgentStatusFrame::default()
        }))),
        ..FirehoseFrame::default()
    })
}

/// One OSC title observation as its frame. Browsers never parse PTY bytes, so
/// the coordinator is the only thing that reads a title out of the stream.
pub fn session_title_frame(title: &SessionTitleUpdate) -> FeedFrame {
    FeedFrame::of(FirehoseFrame {
        frame: Some(Frame::TerminalTitle(Box::new(TerminalTitleFrame {
            session_id: title.session_id.clone(),
            title: title.title.clone(),
            ..TerminalTitleFrame::default()
        }))),
        ..FirehoseFrame::default()
    })
}

/// A workspace row as the message a browser renders.
fn workspace_to_proto(workspace: &roost_protocol::wire::Workspace) -> PbWorkspace {
    PbWorkspace {
        id: workspace.id.as_str().to_owned(),
        worker_fp: workspace.worker_fp.as_str().to_owned(),
        name: workspace.name.clone(),
        folder_path: workspace.folder_path.clone(),
        color: workspace.color.clone(),
        position: as_u32(workspace.position),
        version: as_u64(workspace.version),
        created_at_ms: as_u64(workspace.created_at_ms),
        updated_at_ms: as_u64(workspace.updated_at_ms),
        session_ids: workspace
            .session_ids
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect(),
        ..PbWorkspace::default()
    }
}

/// A relay row as the message a browser renders, with its free-form config
/// carried as the JSON text the wire field is declared as.
fn relay_to_proto(
    relay: &roost_protocol::wire::McpRelay,
) -> Result<roost_proto::McpRelay, FeedRefusal> {
    Ok(roost_proto::McpRelay {
        id: relay.id.as_str().to_owned(),
        label: relay.label.clone(),
        kind: relay.kind.as_str().to_owned(),
        config_json: json_text("mcp_relay.config", &relay.config)?,
        created_at_ms: as_u64(relay.created_at_ms),
        ..roost_proto::McpRelay::default()
    })
}

fn json_text(field: &'static str, value: &impl serde::Serialize) -> Result<String, FeedRefusal> {
    serde_json::to_string(value)
        .map_err(|error| roost_protocol::ProtocolError::new(field, error.to_string()).into())
}
