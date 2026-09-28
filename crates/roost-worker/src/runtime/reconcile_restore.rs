//! Agent conversation restore as the reconcile pass drives it: one claim set
//! per pass, the adopted survivor's claim on its reference, and one restore
//! attempt after a durable respawn whose outcome never re-enters respawn or the
//! tombstone. Ports the `resumedReferenceKeys`/`restoreAgentConversation` edges
//! of v2 `apps/worker/src/boot/boot-session-reconcile.ts:191-196,278-296` and
//! the `restoreAgentConversation` wiring of `main.ts`. Called by
//! `runtime::session_reconcile`; built by `runtime::reconcile_gate`.

use std::collections::HashSet;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures_util::FutureExt as _;
use futures_util::future::BoxFuture;
use roost_host::HostPlatform;
use roost_protocol::agent_conversation_reference::AgentConversationReferenceV1;
use roost_protocol::wire::brand::SessionId;

use crate::agents::conversation_restore::{
    AgentConversationRestoreDeps, AgentConversationRestoreOutcome, conversation_restore_dedupe_key,
    restore_agent_conversation_after_respawn,
};
use crate::session::lifecycle::SessionManager;

/// One restore after a durable respawn. `claims` is the pass's shared set: the
/// restore claims its reference there before it writes and gives it back only
/// for a proven pre-write rejection.
pub trait ConversationRestorer: Send + Sync {
    fn restore<'a>(
        &'a self,
        session_id: &'a SessionId,
        reference: Option<&'a AgentConversationReferenceV1>,
        claims: &'a mut HashSet<String>,
    ) -> BoxFuture<'a, AgentConversationRestoreOutcome>;
}

/// The production restorer: `restore_agent_conversation_after_respawn` under
/// `ROOST_AGENT_CONVERSATION_RESTORE` (`WorkerBoot::agent_conversation_restore`).
pub struct WorkerConversationRestorer {
    enabled: bool,
    manager: Arc<SessionManager>,
    platform: HostPlatform,
}

impl WorkerConversationRestorer {
    pub fn new(enabled: bool, manager: Arc<SessionManager>, platform: HostPlatform) -> Self {
        Self {
            enabled,
            manager,
            platform,
        }
    }
}

impl std::fmt::Debug for WorkerConversationRestorer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkerConversationRestorer")
            .field("enabled", &self.enabled)
            .field("platform", &self.platform)
            .finish_non_exhaustive()
    }
}

impl ConversationRestorer for WorkerConversationRestorer {
    fn restore<'a>(
        &'a self,
        session_id: &'a SessionId,
        reference: Option<&'a AgentConversationReferenceV1>,
        claims: &'a mut HashSet<String>,
    ) -> BoxFuture<'a, AgentConversationRestoreOutcome> {
        let deps = AgentConversationRestoreDeps {
            enabled: self.enabled,
            manager: &self.manager,
            platform: self.platform,
            resumed_reference_keys: Some(claims),
        };
        Box::pin(restore_agent_conversation_after_respawn(
            deps, session_id, reference,
        ))
    }
}

/// v2 `resumedReferenceKeys`: the references this pass has claimed.
#[derive(Debug, Default)]
pub struct PassReferenceClaims {
    keys: HashSet<String>,
}

impl PassReferenceClaims {
    /// An adopted PTY still runs an agent on `reference`, so no session later
    /// in the pass may resume the same conversation.
    pub fn claim_adopted(&mut self, reference: Option<&AgentConversationReferenceV1>) {
        if let Some(reference) = reference {
            self.keys.insert(conversation_restore_dedupe_key(reference));
        }
    }

    /// One attempt, after the respawn's durable event returned. Every outcome
    /// is terminal; a restorer that panics is logged ambiguous (v2's `catch`)
    /// and the pass goes on, never back into respawn or the tombstone.
    pub async fn restore_after_respawn(
        &mut self,
        restorer: &dyn ConversationRestorer,
        session_id: &SessionId,
        reference: Option<&AgentConversationReferenceV1>,
    ) {
        let attempt = restorer.restore(session_id, reference, &mut self.keys);
        if AssertUnwindSafe(attempt).catch_unwind().await.is_err() {
            tracing::warn!(
                session_id = %session_id,
                outcome = "ambiguous",
                "agent_conversation_restore_transition"
            );
        }
    }

    /// The references claimed so far, for the pass's tests.
    pub fn claimed(&self) -> &HashSet<String> {
        &self.keys
    }
}
