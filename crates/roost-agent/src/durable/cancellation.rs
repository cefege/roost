//! Combines core request cancellation with the native Roost run signal.
//! The child token reaches providers and nested tools without cancelling either parent.

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::runtime::AgentRuntime;

pub(crate) struct InvocationCancellation {
    token: CancellationToken,
    core_watcher: JoinHandle<()>,
}

impl InvocationCancellation {
    pub(crate) fn new(
        runtime: &AgentRuntime,
        conversation_id: &str,
        core: CancellationToken,
    ) -> Self {
        let token = runtime
            .with_run(conversation_id, |run| run.cancel.child_token())
            .unwrap_or_default();
        if core.is_cancelled() {
            token.cancel();
        }
        let watcher_token = token.clone();
        let core_watcher = tokio::spawn(async move {
            tokio::select! {
                _ = core.cancelled() => watcher_token.cancel(),
                _ = watcher_token.cancelled() => {}
            }
        });
        Self {
            token,
            core_watcher,
        }
    }

    pub(crate) fn token(&self) -> &CancellationToken {
        &self.token
    }
}

impl Drop for InvocationCancellation {
    fn drop(&mut self) {
        self.token.cancel();
        self.core_watcher.abort();
    }
}
