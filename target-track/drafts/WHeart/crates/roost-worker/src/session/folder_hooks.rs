//! The owners told when a live session's shell is (re)established in a folder:
//! after a spawn, a respawn, a survivor adoption, and an OSC 7 folder change —
//! the four places v2 calls `_startGitBranch` + `_startPorts`
//! (`apps/worker/src/session/session-{spawn,respawn,resume,scrollback}.ts`).
//! `session::git_ports` registers the folder watcher; `session::respawn`,
//! `session::resume` and `session::cwd_events` notify, with no lock held.

use std::sync::{Arc, Mutex};

use roost_protocol::wire::brand::SessionId;

use super::lifecycle::SessionManager;

/// One owner's reaction to a session whose folder is now current.
pub type SessionFolderHook = Arc<dyn Fn(&SessionId, u16) + Send + Sync>;

/// Every registered hook, in registration order.
#[derive(Default)]
pub struct SessionFolderHooks {
    hooks: Mutex<Vec<SessionFolderHook>>,
}

impl std::fmt::Debug for SessionFolderHooks {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionFolderHooks")
            .field("hooks", &self.snapshot().len())
            .finish()
    }
}

impl SessionFolderHooks {
    pub fn register(&self, hook: SessionFolderHook) {
        let mut hooks = self.hooks.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        hooks.push(hook);
        tracing::info!(hooks = hooks.len(), "a session-folder hook was registered");
    }

    /// Call every hook. The list is copied out first, so a hook runs with no
    /// lock of this type held and may itself reach the manager.
    pub fn notify(&self, session_id: &SessionId, channel_id: u16) {
        let hooks = self.snapshot();
        tracing::debug!(%session_id, channel_id, hooks = hooks.len(), "session-folder hooks notified");
        for hook in hooks {
            hook(session_id, channel_id);
        }
    }

    fn snapshot(&self) -> Vec<SessionFolderHook> {
        self.hooks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl SessionManager {
    /// Register an owner told whenever a live session's folder is (re)established.
    pub fn on_session_folder(&self, hook: SessionFolderHook) {
        self.folder_hooks.register(hook);
    }

    /// Tell every registered owner that `session_id` on `channel_id` is live in
    /// its current folder. Called with no record or table lock held.
    pub fn notify_session_folder(&self, session_id: &SessionId, channel_id: u16) {
        self.folder_hooks.notify(session_id, channel_id);
    }
}
