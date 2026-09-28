//! The coordinator's private recovery metadata as boot reconciliation reads it:
//! exactly one row per open session, each carrying the agent reference that
//! session's respawn may restore. Called by the boot reconcile's open-session
//! read (`runtime::reconcile`). Ports `_assertExactRecoveryMetadata` of
//! `apps/worker/src/boot/boot-session-reconcile.ts`.

use std::collections::{HashMap, HashSet};

use roost_proto::SessionRecoveryMetadata;
use roost_protocol::agent_conversation_reference::AgentConversationReferenceV1;
use roost_protocol::proto_adapters::session_recovery_metadata_from_proto;

/// Each open session's reference, `None` where it has none or its stored one
/// no longer satisfies the bounded contract.
pub type RecoveryReferences = HashMap<String, Option<AgentConversationReferenceV1>>;

/// Why the coordinator's recovery metadata cannot be admitted against its own
/// open-session set. Either one fails the reconcile pass, as v2's throw does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RecoveryMetadataMismatch {
    #[error("coordinator recovery metadata set is incomplete")]
    Incomplete,
    #[error("coordinator recovery metadata set does not match sessions")]
    Mismatched,
}

/// Pair every open session with its one recovery row. A row whose reference
/// no longer parses restores an ordinary shell rather than failing admission:
/// failing here would exit the worker and crash-loop it for every session on
/// the machine.
pub fn assert_exact_recovery_metadata(
    session_ids: &[String],
    rows: &[SessionRecoveryMetadata],
) -> Result<RecoveryReferences, RecoveryMetadataMismatch> {
    if rows.len() != session_ids.len() {
        return Err(RecoveryMetadataMismatch::Incomplete);
    }
    let expected: HashSet<&str> = session_ids.iter().map(String::as_str).collect();
    let mut references = RecoveryReferences::with_capacity(rows.len());
    for row in rows {
        let session_id = row.session_id.as_str();
        let reference = match session_recovery_metadata_from_proto(row) {
            Ok(metadata) => metadata.agent_reference,
            Err(_) => {
                tracing::warn!(session = %session_id, "recovery_reference_unusable");
                None
            }
        };
        if !expected.contains(session_id) || references.contains_key(session_id) {
            return Err(RecoveryMetadataMismatch::Mismatched);
        }
        references.insert(session_id.to_owned(), reference);
    }
    if references.len() != expected.len() {
        return Err(RecoveryMetadataMismatch::Incomplete);
    }
    Ok(references)
}
