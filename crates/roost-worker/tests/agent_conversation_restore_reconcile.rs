//! The reconcile pass's reference claims: an adopted survivor claims its
//! reference before any respawned session restores, every session of a pass
//! shares one claim set, a proven rejection frees the reference for the next
//! session, and a restore that fails unexpectedly never escapes the pass.
//! Ports the claim-set cases of v2
//! `apps/worker/tests/agents/agent-conversation-restore-reconcile.test.ts`
//! against `runtime::reconcile_restore`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod session_support;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use roost_platform::HostPlatform;
use roost_protocol::agent_conversation_reference::{
    AgentConversationReferenceKind, AgentConversationReferenceV1,
};
use roost_protocol::wire::brand::SessionId;
use roost_worker::agents::conversation_restore::{
    AgentConversationRestoreOutcome, RestoreSkip, conversation_restore_dedupe_key,
};
use roost_worker::runtime::reconcile_restore::{
    ConversationRestorer, PassReferenceClaims, WorkerConversationRestorer,
};
use roost_worker::session::input_write::WorkerInputResult;
use session_support::Harness;

const FIRST: &str = "00000000-0000-4000-8000-000000000051";
const SECOND: &str = "00000000-0000-4000-8000-000000000052";

fn reference() -> AgentConversationReferenceV1 {
    AgentConversationReferenceV1 {
        schema_version: 1,
        agent_id: "omp".to_owned(),
        kind: AgentConversationReferenceKind::Path,
        value: "/private/opaque-secret.jsonl".to_owned(),
    }
}

fn session(id: &str) -> SessionId {
    SessionId::try_from(id).unwrap()
}

/// Records the claim set each call saw, optionally claims `claim`, then
/// answers a skip, or panics when `panics`.
struct Scripted {
    seen: Mutex<Vec<Vec<String>>>,
    claim: Option<&'static str>,
    panics: bool,
}

impl Scripted {
    fn new(claim: Option<&'static str>, panics: bool) -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            claim,
            panics,
        }
    }

    fn seen(&self) -> Vec<Vec<String>> {
        self.seen.lock().unwrap().clone()
    }
}

impl ConversationRestorer for Scripted {
    fn restore<'a>(
        &'a self,
        _session_id: &'a SessionId,
        _reference: Option<&'a AgentConversationReferenceV1>,
        claims: &'a mut HashSet<String>,
    ) -> BoxFuture<'a, AgentConversationRestoreOutcome> {
        let mut keys: Vec<String> = claims.iter().cloned().collect();
        keys.sort();
        self.seen.lock().unwrap().push(keys);
        if let Some(key) = self.claim {
            claims.insert(key.to_owned());
        }
        let panics = self.panics;
        Box::pin(async move {
            assert!(!panics, "restore callback failed");
            AgentConversationRestoreOutcome::Skipped(RestoreSkip::Duplicate)
        })
    }
}

/// The production restorer, with every outcome it returned recorded.
struct Recording {
    inner: WorkerConversationRestorer,
    outcomes: Arc<Mutex<Vec<AgentConversationRestoreOutcome>>>,
}

impl ConversationRestorer for Recording {
    fn restore<'a>(
        &'a self,
        session_id: &'a SessionId,
        reference: Option<&'a AgentConversationReferenceV1>,
        claims: &'a mut HashSet<String>,
    ) -> BoxFuture<'a, AgentConversationRestoreOutcome> {
        Box::pin(async move {
            let outcome = self.inner.restore(session_id, reference, claims).await;
            self.outcomes.lock().unwrap().push(outcome.clone());
            outcome
        })
    }
}

#[tokio::test]
async fn an_adopted_sessions_reference_is_claimed_before_any_respawn_restore() {
    let restorer = Scripted::new(None, false);
    let mut claims = PassReferenceClaims::default();
    claims.claim_adopted(Some(&reference()));
    claims.claim_adopted(None);
    claims
        .restore_after_respawn(&restorer, &session(SECOND), Some(&reference()))
        .await;
    assert_eq!(
        restorer.seen(),
        [vec![conversation_restore_dedupe_key(&reference())]]
    );
}

#[tokio::test]
async fn every_session_in_one_pass_shares_the_reference_claim_set() {
    let restorer = Scripted::new(Some("claimed"), false);
    let mut claims = PassReferenceClaims::default();
    for id in [FIRST, SECOND] {
        claims
            .restore_after_respawn(&restorer, &session(id), Some(&reference()))
            .await;
    }
    assert_eq!(restorer.seen(), [vec![], vec!["claimed".to_owned()]]);
}

/// Both sessions carry the same reference and neither has a live channel, so
/// each write is a provable zero-byte rejection: the second still attempts.
#[tokio::test]
async fn a_rejected_write_frees_the_reference_for_the_next_session_in_the_pass() {
    let harness = Harness::new();
    let outcomes = Arc::new(Mutex::new(Vec::new()));
    let restorer = Recording {
        inner: WorkerConversationRestorer::new(
            true,
            Arc::clone(&harness.manager),
            HostPlatform::Linux,
        ),
        outcomes: Arc::clone(&outcomes),
    };
    let mut claims = PassReferenceClaims::default();
    for id in [FIRST, SECOND] {
        claims
            .restore_after_respawn(&restorer, &session(id), Some(&reference()))
            .await;
    }
    let outcomes = outcomes.lock().unwrap().clone();
    assert_eq!(outcomes.len(), 2, "{outcomes:?}");
    for outcome in outcomes {
        assert!(
            matches!(
                outcome,
                AgentConversationRestoreOutcome::Written(WorkerInputResult::Rejected { .. })
            ),
            "{outcome:?}"
        );
    }
    assert!(claims.claimed().is_empty());
}

#[tokio::test]
async fn an_unexpected_restore_failure_never_escapes_the_pass() {
    let restorer = Scripted::new(None, true);
    let mut claims = PassReferenceClaims::default();
    for id in [FIRST, SECOND] {
        claims
            .restore_after_respawn(&restorer, &session(id), Some(&reference()))
            .await;
    }
    assert_eq!(restorer.seen().len(), 2, "the pass stopped at the failure");
}
