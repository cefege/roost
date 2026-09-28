//! The registry arms: audit rows, workspace/task/MCP deltas, worker presence,
//! the routable set, and pair requests, each converted into the shared wire
//! shape the folds take.
//!
//! Called by `decode::map_arm` after the meta rule has passed. The row
//! conversions are `client::rpc::codec::wire_rows`, shared with the hydration
//! lists. Ported from `apps/web/src/client/sync/sync-proto-adapters.ts` and
//! `apps/web/src/store/sync-frame.ts:83-176,285-350`.
//!
//! An adapter failure is FATAL for the link, not for one frame. v2's
//! `_foldDelta` comment (`sync-frame.ts:77-81`) says one malformed row must
//! cost one frame, but its code does not: it returns `false`, `_dispatchSyncFrame`
//! reports the frame unconsumed, `dispatchSyncFrameCausally` answers
//! "unapplied" (`sync-flow.ts:52-55`), and `_consumeSyncFrame` throws into
//! `_closeFailedSyncLink` (`sync-inbound.ts:74-83`). The code is what ships.

use roost_proto::__buffa::oneof::mcp_stream_message_proto::Kind as McpKind;
use roost_proto::__buffa::oneof::pair_request_delta_proto::Kind as PairKind;
use roost_proto::__buffa::oneof::task_delta_proto::Kind as TaskKind;
use roost_proto::__buffa::oneof::worker_presence_proto::Kind as PresenceKind;
use roost_proto::__buffa::oneof::workspace_delta_proto::Kind as WorkspaceKind;
use roost_proto::{
    AuditRow, McpStreamMessageProto, PairRequestDeltaProto, TaskDeltaProto, WorkerPresenceProto,
    WorkerRoutableFrame, WorkspaceDeltaProto,
};
use roost_protocol::proto_adapters::terminal_core_capacity_report_from_proto;
use roost_protocol::wire::{
    McpRelayDelta, McpRelayEvent, McpRelayId, McpStreamMessage, SessionId, TaskDelta, WorkerFp,
    WorkerPresenceEvent, WorkspaceDelta, WorkspaceId,
};

use crate::client::rpc::codec::wire_rows::{
    host_metrics_from_proto, mcp_relay_from_proto, pair_request_from_proto, task_from_proto,
    worker_from_proto, workspace_from_proto,
};
use crate::store::sync_feeds::ROUTABLE_SEED_CHUNKS_MAX;
use crate::sync::inbound::{
    AuditEntry, PairRequestChange, PairedBrowser, RoutableChunk, SyncFrame,
};

/// One audit row, in the legacy shape the audit pane renders.
pub(super) fn audit_row(value: AuditRow) -> SyncFrame {
    SyncFrame::AuditRow {
        row: AuditEntry {
            id: value.id,
            ts: value.ts,
            caller_fp: value.caller_fp,
            caller_label: value.caller_label,
            method: value.method,
            path: value.path,
            status: value.status,
            trace_id: value.trace_id,
        },
    }
}

/// A workspace change. An empty oneof is v2's adapter returning null.
pub(super) fn workspace_delta(value: WorkspaceDeltaProto) -> Result<SyncFrame, String> {
    let delta = match value.kind.ok_or("workspace_delta carries no change")? {
        WorkspaceKind::Created(workspace) => WorkspaceDelta::Created {
            workspace: workspace_from_proto(&workspace).map_err(|error| error.to_string())?,
        },
        WorkspaceKind::Updated(workspace) => WorkspaceDelta::Updated {
            workspace: workspace_from_proto(&workspace).map_err(|error| error.to_string())?,
        },
        WorkspaceKind::DeletedId(id) => WorkspaceDelta::Deleted {
            id: WorkspaceId::try_from(id.as_str()).map_err(|error| error.to_string())?,
        },
        WorkspaceKind::SessionsSet(set) => WorkspaceDelta::SessionsSet {
            id: WorkspaceId::try_from(set.workspace_id.as_str())
                .map_err(|error| error.to_string())?,
            session_ids: set
                .session_ids
                .iter()
                .map(|id| SessionId::try_from(id.as_str()).map_err(|error| error.to_string()))
                .collect::<Result<_, _>>()?,
            version: signed("workspace_delta.sessions_set.version", set.version)?,
        },
    };
    Ok(SyncFrame::WorkspaceDelta { delta })
}

/// A task created or moved to a new state.
pub(super) fn task_delta(value: TaskDeltaProto) -> Result<SyncFrame, String> {
    let delta = match value.kind.ok_or("task_delta carries no change")? {
        TaskKind::Created(task) => TaskDelta::Created {
            task: task_from_proto(&task).map_err(|error| error.to_string())?,
        },
        TaskKind::State(task) => TaskDelta::State {
            task: task_from_proto(&task).map_err(|error| error.to_string())?,
        },
    };
    Ok(SyncFrame::TaskDelta { delta })
}

/// A relay change, or a relay event whose payload must parse
/// (`decodeWireJson` returning undefined makes the adapter return null).
pub(super) fn mcp_message(value: McpStreamMessageProto) -> Result<SyncFrame, String> {
    let message = match value.kind.ok_or("mcp_msg carries no change")? {
        McpKind::Created(relay) => McpStreamMessage::Delta(McpRelayDelta::Created {
            relay: mcp_relay_from_proto(&relay).map_err(|error| error.to_string())?,
        }),
        McpKind::Updated(relay) => McpStreamMessage::Delta(McpRelayDelta::Updated {
            relay: mcp_relay_from_proto(&relay).map_err(|error| error.to_string())?,
        }),
        McpKind::DeletedId(id) => McpStreamMessage::Delta(McpRelayDelta::Deleted {
            id: McpRelayId::try_from(id.as_str()).map_err(|error| error.to_string())?,
        }),
        McpKind::Event(event) => McpStreamMessage::Event(McpRelayEvent {
            relay_id: McpRelayId::try_from(event.relay_id.as_str())
                .map_err(|error| error.to_string())?,
            payload: serde_json::from_str(&event.payload_json)
                .map_err(|error| format!("mcp relay event payload is not JSON: {error}"))?,
            ts: signed("mcp_msg.event.ts", event.ts)?,
        }),
    };
    Ok(SyncFrame::McpMessage { message })
}

/// A worker registered, heartbeat, or was removed. An invalid capacity report
/// on a heartbeat becomes `None`, as v2's `terminalCoreCapacityProtoToWire`.
pub(super) fn worker_presence(value: WorkerPresenceProto) -> Result<SyncFrame, String> {
    let event = match value.kind.ok_or("worker_presence carries no change")? {
        PresenceKind::Registered(worker) => WorkerPresenceEvent::Registered {
            worker: worker_from_proto(&worker).map_err(|error| error.to_string())?,
        },
        PresenceKind::Heartbeat(heartbeat) => WorkerPresenceEvent::Heartbeat {
            fp: WorkerFp::try_from(heartbeat.worker_fp.as_str())
                .map_err(|error| error.to_string())?,
            last_seen_ms: signed(
                "worker_presence.heartbeat.last_seen_ms",
                heartbeat.last_seen_ms,
            )?,
            host_metrics: heartbeat
                .host_metrics
                .as_option()
                .map(host_metrics_from_proto)
                .transpose()
                .map_err(|error| error.to_string())?,
            terminal_core_capacity: heartbeat
                .terminal_core_capacity
                .as_option()
                .and_then(|report| terminal_core_capacity_report_from_proto(report).ok()),
        },
        PresenceKind::RemovedFp(fp) => WorkerPresenceEvent::Removed {
            fp: WorkerFp::try_from(fp.as_str()).map_err(|error| error.to_string())?,
        },
    };
    Ok(SyncFrame::WorkerPresence { event })
}

/// The routable set, or one bounded chunk of a retained seed. Out-of-bounds
/// chunk numbering is v2's `dispatchRoutableChunk` returning false
/// (`sync-inbound.ts:155-160`), which closes the link.
pub(super) fn worker_routable(value: WorkerRoutableFrame) -> Result<SyncFrame, String> {
    if value.snapshot_id.is_empty() {
        return Ok(SyncFrame::WorkerRoutable {
            fps: value.fps,
            chunk: None,
        });
    }
    if value.chunk_count == 0
        || value.chunk_count > ROUTABLE_SEED_CHUNKS_MAX
        || value.chunk_index >= value.chunk_count
    {
        return Err(format!(
            "worker_routable chunk {} of {} is out of bounds",
            value.chunk_index, value.chunk_count
        ));
    }
    Ok(SyncFrame::WorkerRoutable {
        fps: value.fps,
        chunk: Some(RoutableChunk {
            snapshot_id: value.snapshot_id,
            chunk_index: value.chunk_index,
            chunk_count: value.chunk_count,
        }),
    })
}

/// A pair-request change. An empty oneof is v2's `consumed = false`.
pub(super) fn pair_request_delta(value: PairRequestDeltaProto) -> Result<SyncFrame, String> {
    let change = match value.kind.ok_or("pair_request_delta carries no change")? {
        PairKind::Pending(request) => PairRequestChange::Pending(
            pair_request_from_proto(&request).map_err(|error| error.to_string())?,
        ),
        PairKind::RemovedId(ephemeral_id) => PairRequestChange::Removed { ephemeral_id },
        PairKind::Snapshot(snapshot) => PairRequestChange::Snapshot(
            snapshot
                .pending
                .iter()
                .map(|request| pair_request_from_proto(request).map_err(|error| error.to_string()))
                .collect::<Result<_, _>>()?,
        ),
        PairKind::Completed(completed) => PairRequestChange::Completed(PairedBrowser {
            ephemeral_id: completed.ephemeral_id,
            label: completed.label,
            client_browser: completed.client_browser,
            client_os: completed.client_os,
            city: completed.city,
            region: completed.region,
            country_code: completed.country_code,
        }),
    };
    Ok(SyncFrame::PairRequestDelta { change })
}

/// A wire `uint64` as the wire shape's signed integer.
fn signed(field: &str, value: u64) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("{field} {value} exceeds the signed range"))
}
