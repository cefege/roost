//! The owners told when a session this worker held has ended: v2
//! `apps/worker/src/session/session-manager-state.ts` `setAgentStatusHooks`
//! (`sessionClosed`) as `session-lifecycle.ts` `_dropChannelState` calls it.
//! `runtime::owners` registers the route owner's `retire_session` and the view
//! owner's `close_session` (v2 `main.ts:228-236`); `SessionManager::close_channel`
//! notifies, after the record left the table and with no lock held.

use std::sync::{Arc, Mutex};

use roost_protocol::wire::brand::SessionId;

/// One owner's reaction to a closed session.
pub type SessionClosedHook = Arc<dyn Fn(&SessionId) + Send + Sync>;

/// Every registered hook, in registration order.
#[derive(Default)]
pub struct SessionClosedHooks {
    hooks: Mutex<Vec<SessionClosedHook>>,
}

impl std::fmt::Debug for SessionClosedHooks {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionClosedHooks")
            .field("hooks", &self.snapshot().len())
            .finish()
    }
}

impl SessionClosedHooks {
    pub fn register(&self, hook: SessionClosedHook) {
        let mut hooks = self
            .hooks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        hooks.push(hook);
        tracing::info!(hooks = hooks.len(), "a session-closed hook was registered");
    }

    /// Call every hook for `session_id`. The list is copied out first, so a hook
    /// runs with no lock of this type held and may itself reach the manager.
    pub fn notify(&self, session_id: &SessionId) {
        let hooks = self.snapshot();
        tracing::debug!(%session_id, hooks = hooks.len(), "session-closed hooks notified");
        for hook in hooks {
            hook(session_id);
        }
    }

    fn snapshot(&self) -> Vec<SessionClosedHook> {
        self.hooks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}
