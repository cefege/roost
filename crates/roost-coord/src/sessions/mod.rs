//! The session domain: the lifecycle RPCs, the pending-spawn table, the list
//! projection, and the workspaces, tasks and MCP relays that hang off a session.
//!
//! One field on `CoordServices`, reached as `core.services.sessions`. The list
//! projection, the spawn reservation and the input lane all read the same
//! session state, so two instances would be two answers to "does this session
//! exist".
//!
//! `new()` takes nothing and must keep taking nothing: anything the domain
//! needs from configuration is read at call time from `core.services.boot`.

use std::sync::Arc;

use roost_protocol::wire::{SessionEvent, WorkerFp};

use crate::sessions::pending_spawns::PendingSpawns;

pub mod assign_workspace;
pub mod cursor_pos;
pub mod list_projection;
pub mod mcp;
pub mod mcp_store;
pub mod pending_spawns;
pub mod rpc_sessions;
pub mod rpc_workspaces;
pub mod spawn;
pub mod tasks;
pub mod workspace_delete;
pub mod workspaces;

/// The session state one coordinator process holds.
#[derive(Debug, Default)]
pub struct SessionsRuntime {
    /// In-flight spawns by session UUID. An `Arc` because the worker lifecycle
    /// registry holds it too, to reject a revoked worker's spawns.
    pending_spawns: Arc<PendingSpawns>,
}

impl SessionsRuntime {
    /// A coordinator with no sessions and nothing reserved.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The pending-spawn reservation table.
    #[must_use]
    pub fn pending_spawns(&self) -> &Arc<PendingSpawns> {
        &self.pending_spawns
    }

    /// Reconcile a committed worker event with the spawn it may complete: a
    /// durable `opened` resolves a spawn whose worker reply was lost (v2
    /// `worker-frame-dispatch.ts` `resolvePendingSpawnOpened`). Any other event
    /// is not a spawn's, and `false` says so.
    pub fn resolve_spawn_on_opened(&self, worker_fp: &WorkerFp, event: &SessionEvent) -> bool {
        let SessionEvent::Opened {
            session_id,
            channel,
            ..
        } = event
        else {
            return false;
        };
        self.pending_spawns.resolve_opened(
            worker_fp.as_str(),
            session_id.as_str(),
            channel.as_u32(),
        )
    }
}
