//! The LIVE session scope of a registered Sync socket, read by the owner-mode
//! relay at the moment a command or an owner decision arrives.
//!
//! Ports the `allowsSession` closure `sync-ws-handler.ts:269` registers, which
//! reads the socket's current scope rather than the one it opened with: a
//! session spawned after the socket opened must be viewable on it. The
//! registry's `SocketRecord` holds the upgrade-time set; the relay paths here
//! consult this instead.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use roost_protocol::terminal_view::SocketRecord;

use super::TerminalViewHub;

/// A socket's current session scope.
pub trait SocketScope: Send + Sync {
    /// Whether the socket may observe `session_id` right now.
    fn allows_session(&self, session_id: &str) -> bool;
}

/// Every registered socket's live scope, by socket id.
#[derive(Default)]
pub struct SocketScopes {
    scopes: Mutex<HashMap<String, Arc<dyn SocketScope>>>,
}

impl std::fmt::Debug for SocketScopes {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SocketScopes")
            .field("sockets", &self.locked().len())
            .finish()
    }
}

impl SocketScopes {
    fn locked(&self) -> MutexGuard<'_, HashMap<String, Arc<dyn SocketScope>>> {
        self.scopes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn remove(&self, socket_id: &str) {
        self.locked().remove(socket_id);
    }

    /// The socket's record with its permission for `session_id` replaced by
    /// the live answer, so the relay's own check reads the current scope.
    pub(super) fn refreshed(&self, mut record: SocketRecord, session_id: &str) -> SocketRecord {
        let scope = self.locked().get(&record.id).cloned();
        if let Some(scope) = scope {
            if scope.allows_session(session_id) {
                record.allowed_sessions.insert(session_id.to_owned());
            } else {
                record.allowed_sessions.remove(session_id);
            }
        }
        record
    }
}

impl TerminalViewHub {
    /// Attach a registered socket's live scope. Called beside
    /// `register_socket`; `close_socket` detaches it.
    pub fn register_socket_scope(&self, socket_id: &str, scope: Arc<dyn SocketScope>) {
        self.scopes.locked().insert(socket_id.to_owned(), scope);
    }

    /// The registered socket, with its permission for `session_id` read live.
    pub(super) fn live_socket(&self, socket_id: &str, session_id: &str) -> Option<SocketRecord> {
        let record = self.locked().socket(socket_id).cloned()?;
        Some(self.scopes.refreshed(record, session_id))
    }
}
