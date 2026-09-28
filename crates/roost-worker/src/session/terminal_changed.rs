//! The owner told that a channel's terminal received output: v2
//! `apps/worker/src/session/session-manager-state.ts` `setAgentStatusHooks`
//! (`terminalChanged`) as `session-emit.ts` `emitUpstreamChunk` calls it for
//! every chunk that reached a live record, on every lane. `agents::status_stack`
//! installs the detector's `schedule`; `runtime::channel_delivery` notifies,
//! on the keeper's dispatch thread with the record locked.

use std::sync::{Arc, OnceLock};

/// The reaction to one channel's output. Called with the record lock held, so
/// it must not reach the session table.
pub type TerminalChangedHook = Arc<dyn Fn(u16) + Send + Sync>;

/// v2 held exactly one hook, set once at boot; so does this.
#[derive(Default)]
pub struct TerminalChangedHooks {
    hook: OnceLock<TerminalChangedHook>,
}

impl std::fmt::Debug for TerminalChangedHooks {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TerminalChangedHooks")
            .field("installed", &self.hook.get().is_some())
            .finish()
    }
}

impl TerminalChangedHooks {
    /// Install the hook. A second install is refused and logged: two owners
    /// racing for one hook is a composition bug, not a policy.
    pub fn install(&self, hook: TerminalChangedHook) {
        if self.hook.set(hook).is_err() {
            tracing::error!(
                "a second terminal-changed hook was refused; the first stays installed"
            );
        } else {
            tracing::info!("the terminal-changed hook was installed");
        }
    }

    pub fn notify(&self, channel_id: u16) {
        if let Some(hook) = self.hook.get() {
            hook(channel_id);
        }
    }
}
