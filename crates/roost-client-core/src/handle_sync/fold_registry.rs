//! The registry folds: workspaces, tasks, MCP relays, worker presence and the
//! routable worker set, each applied from an already-decoded wire value.
//!
//! Called by `apply_frame` only, downstream of the admission gate. Ported from
//! `apps/web/src/store/sync-handlers.ts` (`_handleWorkspacesDelta`,
//! `_handleTasksDelta`, `_handleMcpEvent`, `_handlePresenceEvent`) and
//! `apps/web/src/store/sync-inbound.ts:150-178` (`dispatchRoutableChunk`).

use std::collections::BTreeSet;

use roost_protocol::wire::{
    McpRelayDelta, McpStreamMessage, TaskDelta, WorkerPresenceEvent, WorkspaceDelta,
};

use crate::effect::Effect;
use crate::store::Store;
use crate::store::mutations::{delete_mcp_relay, delete_worker, upsert_mcp_relay};
use crate::store::sync_feeds::RoutableProgress;
use crate::sync::inbound::RoutableChunk;

use super::close_failed::close_failed_sync_link;

/// A workspace created, updated, deleted, or given a new session list.
pub(super) fn fold_workspace_delta(store: &mut Store, delta: &WorkspaceDelta) {
    let changed = match delta {
        WorkspaceDelta::Created { workspace } | WorkspaceDelta::Updated { workspace } => {
            let id = workspace.id.as_str();
            if store.workspaces.get(id) == Some(workspace) {
                false
            } else {
                store.workspaces.insert(id.to_owned(), workspace.clone());
                true
            }
        }
        WorkspaceDelta::Deleted { id } => store.workspaces.remove(id.as_str()).is_some(),
        // A session list for a workspace this store never received is dropped,
        // as v2's updater returns `prev` when there is none.
        WorkspaceDelta::SessionsSet {
            id,
            session_ids,
            version,
        } => match store.workspaces.get_mut(id.as_str()) {
            Some(workspace)
                if workspace.session_ids != *session_ids || workspace.version != *version =>
            {
                workspace.session_ids.clone_from(session_ids);
                workspace.version = *version;
                true
            }
            _ => false,
        },
    };
    if changed {
        store.note_change();
    }
    tracing::debug!(target: "sync", changed, count = store.workspaces.len(), "workspace delta folded");
}

/// A task created or moved to a new state: either way the row replaces the
/// held one.
pub(super) fn fold_task_delta(store: &mut Store, delta: &TaskDelta) {
    let (TaskDelta::Created { task } | TaskDelta::State { task }) = delta;
    let id = task.id.as_str();
    let changed = store.tasks.get(id) != Some(task);
    if changed {
        store.tasks.insert(id.to_owned(), task.clone());
        store.note_change();
    }
    tracing::debug!(target: "sync", changed, task = id, state = task.state.as_str(), "task delta folded");
}

/// A relay created, updated or deleted. A relay EVENT is traffic for whoever
/// is watching the relay, not registry state, and v2's handler ignores it.
pub(super) fn fold_mcp_message(store: &mut Store, message: &McpStreamMessage) {
    let changed = match message {
        McpStreamMessage::Delta(
            McpRelayDelta::Created { relay } | McpRelayDelta::Updated { relay },
        ) => upsert_mcp_relay(store, relay.clone()),
        McpStreamMessage::Delta(McpRelayDelta::Deleted { id }) => {
            delete_mcp_relay(store, id.as_str())
        }
        McpStreamMessage::Event(_) => false,
    };
    tracing::debug!(target: "sync", changed, "mcp message folded");
}

/// A worker registered, heartbeat, or was removed.
///
/// A removal is v2 `applyWorkerRemoval`: the worker's direct routes retire
/// before the machine record disappears, so no route outlives its worker.
pub(super) fn fold_worker_presence(
    store: &mut Store,
    event: &WorkerPresenceEvent,
    out: &mut Vec<Effect>,
) {
    match event {
        WorkerPresenceEvent::Registered { worker } => {
            let fp = worker.fp.as_str();
            let changed = store.workers.get(fp) != Some(worker);
            if changed {
                store.workers.insert(fp.to_owned(), worker.clone());
                store.note_change();
            }
            tracing::info!(target: "sync", worker_fp = fp, changed, "worker registered");
        }
        WorkerPresenceEvent::Heartbeat {
            fp,
            last_seen_ms,
            host_metrics,
            terminal_core_capacity,
        } => {
            // A heartbeat for a worker this store has no record of changes
            // nothing: v2's updater returns `prev` when there is none.
            let Some(worker) = store.workers.get_mut(fp.as_str()) else {
                tracing::debug!(
                    target: "sync",
                    worker_fp = fp.as_str(),
                    "heartbeat for an unknown worker"
                );
                return;
            };
            let changed = worker.last_seen_ms != *last_seen_ms
                || worker.host_metrics != *host_metrics
                || worker.terminal_core_capacity != *terminal_core_capacity;
            if changed {
                worker.last_seen_ms = *last_seen_ms;
                worker.host_metrics.clone_from(host_metrics);
                worker
                    .terminal_core_capacity
                    .clone_from(terminal_core_capacity);
                store.note_change();
            }
            tracing::trace!(target: "sync", worker_fp = fp.as_str(), changed, "worker heartbeat");
        }
        WorkerPresenceEvent::Removed { fp } => {
            crate::handle_terminal::handle_worker_retired(store, fp.as_str(), out);
            delete_worker(store, fp.as_str());
        }
    }
}

/// The routable set: replaced wholesale by a live frame, or by a retained
/// seed once its last chunk arrives.
pub(super) fn fold_worker_routable(
    store: &mut Store,
    generation: u64,
    fps: &[String],
    chunk: Option<&RoutableChunk>,
    out: &mut Vec<Effect>,
) {
    let Some(chunk) = chunk else {
        replace_routable_set(store, fps.iter().cloned().collect());
        return;
    };
    match store.routable_assembly.accept(chunk, fps) {
        RoutableProgress::Pending => tracing::debug!(
            target: "sync",
            snapshot_id = %chunk.snapshot_id,
            chunk_index = chunk.chunk_index,
            chunk_count = chunk.chunk_count,
            "routable seed chunk held"
        ),
        RoutableProgress::Complete(routable) => replace_routable_set(store, routable),
        RoutableProgress::CountMismatch => close_failed_sync_link(
            store,
            generation,
            format!(
                "worker_routable chunk count {} disagrees with its seed",
                chunk.chunk_count
            ),
            out,
        ),
    }
}

/// Replace the routable set, moving the revision only when membership moved:
/// v2's signal compares by content, so an unchanged set notifies nobody.
fn replace_routable_set(store: &mut Store, routable: BTreeSet<String>) {
    if store.routable_worker_fps.as_ref() == Some(&routable) {
        return;
    }
    tracing::info!(target: "sync", count = routable.len(), "routable worker set replaced");
    store.routable_worker_fps = Some(routable);
    store.note_change();
}
