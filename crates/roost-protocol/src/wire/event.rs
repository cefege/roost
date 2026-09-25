//! The canonical fold of the append-only session event log. There is exactly
//! one fold in Roost and it is here.
//!
//! Public session state is the deterministic fold of an ordered event log,
//! shared by the coordinator's projector, the browser's store, and the worker's
//! own reconciliation. Replay is deterministic for a stable input order, which
//! is what makes a projection rebuildable from the log at all. The union being
//! folded is in `variants`.
//!
//! `fold_event` takes its input by reference and returns a new map, so a caller
//! may hold a projection across a fold and know it was not touched. A no-op
//! variant, and any variant naming a session the projection does not hold, hands
//! back the same contents.
mod variants;

use std::collections::BTreeMap;

pub use self::variants::SessionEvent;
use crate::wire::brand::SessionId;
use crate::wire::session::{Session, SessionStatus};

/// The projection the fold produces, keyed by session id.
pub type SessionMap = BTreeMap<SessionId, Session>;

/// Fold one event into a session map. The input is never mutated: a no-op
/// variant and a variant naming a session the projection does not hold both
/// hand back the same contents.
pub fn fold_event(prev: &SessionMap, event: &SessionEvent) -> SessionMap {
    match event {
        // Private and viewer-local variants are explicit no-ops: they are
        // durable and ordered, but they are not public session state.
        SessionEvent::Attached { .. }
        | SessionEvent::Detached { .. }
        | SessionEvent::AgentReference { .. } => prev.clone(),

        // The only arm that ignores the previous state. It captures the spawn
        // folder once, and leaves the resolved git, pull-request, and port
        // fields absent rather than null: nothing has looked yet.
        SessionEvent::Opened {
            session_id,
            worker_fp,
            channel,
            session_kind,
            cwd,
            ts,
            ..
        } => {
            let mut next = prev.clone();
            next.insert(
                session_id.clone(),
                Session {
                    id: session_id.clone(),
                    worker_fp: worker_fp.clone(),
                    channel: *channel,
                    kind: *session_kind,
                    cwd: cwd.clone(),
                    spawn_cwd: Some(cwd.clone()),
                    workspace_id: None,
                    status: SessionStatus::Open,
                    created_at: *ts,
                    closed_at: None,
                    custom_title: None,
                    git_branch: None,
                    git_remote: None,
                    pr_number: None,
                    pr_state: None,
                    pr_checks: None,
                    pr_url: None,
                    ports: None,
                },
            );
            next
        }

        // The terminal's process actually exited, or the coordinator confirmed
        // the session is gone from the worker's authoritative live snapshot.
        // This is the only deletion trigger, and it is a no-op for a session the
        // projection does not hold.
        SessionEvent::Closed { session_id, .. } => {
            let mut next = prev.clone();
            next.remove(session_id);
            next
        }

        SessionEvent::Cwd {
            session_id, cwd, ..
        } => {
            let mut next = prev.clone();
            if let Some(session) = next.get_mut(session_id) {
                // Only the live folder drifts. `spawn_cwd` is the creation
                // fact behind the stable `/t/` URL and never moves.
                session.cwd = cwd.clone();
            }
            next
        }

        SessionEvent::WorkspaceAssigned {
            session_id,
            workspace_id,
            ..
        } => {
            let mut next = prev.clone();
            if let Some(session) = next.get_mut(session_id) {
                session.workspace_id = workspace_id.clone();
            }
            next
        }

        // Sync this worker's announced sessions into the projection. A session
        // of this worker that is absent from the snapshot is NOT deleted: it
        // persists as an offline breadcrumb, because a worker restart kills the
        // PTY but the row is where the user was working. Other workers' rows
        // are untouched. Exactly four fields are carried over from the previous
        // row — the coordinator- or user-owned ones the worker does not track.
        SessionEvent::Snapshot { sessions, .. } => {
            let mut next = prev.clone();
            for announced in sessions {
                let mut row = announced.clone();
                if let Some(before) = prev.get(&row.id) {
                    row.created_at = before.created_at;
                    row.workspace_id = before.workspace_id.clone();
                    row.custom_title = before.custom_title.clone();
                    row.spawn_cwd = before.spawn_cwd.clone();
                }
                next.insert(announced.id.clone(), row);
            }
            next
        }

        SessionEvent::Respawned {
            session_id,
            new_channel,
            ..
        } => {
            let mut next = prev.clone();
            if let Some(session) = next.get_mut(session_id) {
                session.channel = *new_channel;
                // Forced open, in case a `closed` was projected before the
                // respawn landed.
                session.status = SessionStatus::Open;
                session.closed_at = None;
            }
            next
        }

        SessionEvent::Renamed {
            session_id,
            custom_title,
            ..
        } => {
            let mut next = prev.clone();
            if let Some(session) = next.get_mut(session_id) {
                session.custom_title = if custom_title.is_empty() {
                    None
                } else {
                    Some(custom_title.clone())
                };
            }
            next
        }

        SessionEvent::Git {
            session_id,
            branch,
            remote,
            ..
        } => {
            let mut next = prev.clone();
            if let Some(session) = next.get_mut(session_id) {
                session.git_branch = branch.clone();
                if let Some(remote) = remote {
                    session.git_remote = Some(Some(remote.clone()));
                }
            }
            next
        }

        SessionEvent::Pr {
            session_id,
            number,
            state,
            checks,
            url,
            ..
        } => {
            let mut next = prev.clone();
            if let Some(session) = next.get_mut(session_id) {
                session.pr_number = *number;
                session.pr_state = *state;
                session.pr_checks = *checks;
                session.pr_url = url.clone();
            }
            next
        }

        SessionEvent::Ports {
            session_id, ports, ..
        } => {
            let mut next = prev.clone();
            if let Some(session) = next.get_mut(session_id) {
                session.ports = Some(ports.clone());
            }
            next
        }
    }
}

/// Fold an ordered log from nothing. Replay is deterministic, so rebuilding a
/// projection from the log reproduces it exactly.
pub fn fold_all(events: &[SessionEvent]) -> SessionMap {
    let mut projection = SessionMap::new();
    for event in events {
        projection = fold_event(&projection, event);
    }
    projection
}
