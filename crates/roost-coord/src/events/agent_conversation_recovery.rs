//! The coordinator's private agent-conversation recovery projection: the two
//! `sessions` columns a browser must never read, written by exactly one path.
//!
//! Ported from `apps/coord/src/agents/agent-conversation-recovery.ts`, which
//! lives in the agents folder and is called from inside the event transaction.
//! It is here rather than behind a trait because it is thirty lines and it owns
//! three columns; the alternative was an indirection in front of the append path's
//! most order-sensitive statement.
//!
//! WHAT IT IS FOR. A coding agent's conversation lives outside the coordinator,
//! and this reference is how the owning worker finds it again after a restart.
//! It is durable and ordered by the worker's outbox sequence, and it is
//! **private**: `visibility`'s predicate keeps it out of the durable reads, out
//! of the bus, and out of every Sync frame
//! (`docs/phase3-coord-contract.md` §3.7).
//!
//! WHY THE WRITE IS MONOTONIC. The statement only fires when the stored sequence
//! is null or lower than the incoming one
//! (`agent-conversation-recovery.ts:23-25`). The worker's outbox is ordered, so a
//! reference always arrives after the one it replaces; without the guard a
//! redelivery of an *older* reference -- which the at-least-once delivery
//! guarantee makes possible after a reconnect -- would silently roll the
//! conversation back to a state the agent has already left.
//!
//! WHY THE WORKER FINGERPRINT IS IN THE WHERE CLAUSE. The reference is the owning
//! worker's private state, and the row it writes must be a row that worker owns.
//! Admission has already refused a foreign session, so this is the second fence
//! on the same fact, and it is the one that survives an admission rule changing.

use roost_protocol::agent_conversation_reference::AgentConversationReferenceV1;
use roost_protocol::wire::{SessionId, WorkerFp};
use sqlx::sqlite::SqliteConnection;

/// Why a recovery reference was not written.
#[derive(Debug, thiserror::Error)]
pub enum AgentConversationRecoveryError {
    /// The statement failed.
    #[error("sqlite: {0}")]
    Sqlite(#[from] sqlx::Error),
    /// The caller passed a sequence no worker outbox can have produced.
    #[error("agent conversation reference requires a valid worker sequence")]
    InvalidWorkerSequence,
    /// The reference itself broke its own contract. The event boundary has
    /// already checked it once; checking again here is because this value is
    /// about to become a stored column that a later reader will trust.
    #[error("agent conversation reference is invalid: {0}")]
    InvalidReference(String),
    /// The reference could not be encoded.
    #[error("agent conversation reference could not be encoded: {0}")]
    Encode(serde_json::Error),
}

/// Write one agent-conversation recovery reference onto its session row.
///
/// A `None` reference clears the stored one while keeping the sequence, which is
/// how an agent that dropped its conversation says so without making the next
/// reference look stale.
pub async fn project_agent_conversation_reference(
    connection: &mut SqliteConnection,
    session_id: &SessionId,
    reference: Option<&AgentConversationReferenceV1>,
    client_seq: u64,
    worker_fp: &WorkerFp,
) -> Result<(), AgentConversationRecoveryError> {
    if client_seq == 0 {
        return Err(AgentConversationRecoveryError::InvalidWorkerSequence);
    }
    let reference_json = match reference {
        Some(reference) => {
            reference
                .check()
                .map_err(|error| AgentConversationRecoveryError::InvalidReference(error.reason))?;
            Some(serde_json::to_string(reference).map_err(AgentConversationRecoveryError::Encode)?)
        }
        None => None,
    };
    let sequence = i64::try_from(client_seq)
        .map_err(|_| AgentConversationRecoveryError::InvalidWorkerSequence)?;
    sqlx::query(
        "UPDATE sessions \
            SET agent_reference_json = ?, agent_reference_client_seq = ? \
          WHERE id = ? AND worker_fp = ? \
            AND (agent_reference_client_seq IS NULL OR agent_reference_client_seq < ?)",
    )
    .bind(reference_json)
    .bind(sequence)
    .bind(session_id.as_str())
    .bind(worker_fp.as_str())
    .bind(sequence)
    .execute(&mut *connection)
    .await?;
    Ok(())
}
