//! Where a live channel's exit and its record-less output go: the session
//! close (`closed_by_keeper`) and the
//! keeper-health counter behind `emit_no_session`.
//! `RecordBinding::closing` captures one at construction; the keeper's dispatch
//! thread reaches it through the binding.
//!
//! The close is ASYNC and the dispatch thread is not, so it is spawned on the
//! runtime the binding was built on, never blocked on: one slow durable write
//! must not stall every other channel's output.

use std::sync::{Arc, Weak};

use super::lifecycle::SessionManager;

/// A binding's route to its session manager.
#[derive(Clone)]
pub struct SessionCloser {
    manager: Weak<SessionManager>,
    runtime: Option<tokio::runtime::Handle>,
}

impl SessionCloser {
    /// The route for bindings `manager` builds, on the current runtime.
    pub(super) fn of(manager: &SessionManager) -> Self {
        Self {
            manager: manager
                .owned()
                .as_ref()
                .map(Arc::downgrade)
                .unwrap_or_default(),
            runtime: tokio::runtime::Handle::try_current().ok(),
        }
    }

    /// The channel's keeper exited: close its session.
    pub(super) fn close(&self, channel_id: u16, exit_code: Option<i32>) {
        let Some(manager) = self.manager.upgrade() else {
            tracing::warn!(
                channel_id,
                ?exit_code,
                "a channel ended after its session manager was dropped"
            );
            return;
        };
        let Some(runtime) = &self.runtime else {
            tracing::error!(
                channel_id,
                ?exit_code,
                "a channel ended with no runtime to record its close on"
            );
            return;
        };
        runtime.spawn(async move {
            match manager.close_channel(channel_id, exit_code).await {
                Ok(outcome) => tracing::debug!(channel_id, ?outcome, "a keeper exit closed its session"),
                Err(refusal) => tracing::error!(channel_id, reason = %refusal.message(), "a keeper exit could not close its session"),
            }
        });
    }

    /// Output for a channel with no record.
    pub(super) fn orphan_output(&self, channel_id: u16, len: usize, now_ms: i64) {
        match self.manager.upgrade() {
            Some(manager) => {
                manager
                    .keeper_health()
                    .note_orphan_output(channel_id, len, now_ms);
            }
            None => tracing::debug!(
                channel_id,
                len,
                "output arrived after the session manager was dropped"
            ),
        }
    }
}

impl std::fmt::Debug for SessionCloser {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionCloser")
            .field("runtime", &self.runtime.is_some())
            .finish_non_exhaustive()
    }
}
