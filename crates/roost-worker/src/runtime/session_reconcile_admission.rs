//! The admission half of a reconcile pass: every durable claim the pass's
//! possible outcomes need is reserved, per coordinator row, before the keeper
//! or the session table is touched. Owned by `runtime::session_reconcile`, which
//! runs the pass and gives unspent claims back; v2's
//! `apps/worker/src/boot/boot-session-reconcile.ts` `:111-165` is the authority.

use roost_protocol::agent_conversation_reference::AgentConversationReferenceV1;
use roost_protocol::wire::brand::{ChannelId, SessionId};

use super::reconcile::{OpenSession, OpenSessionSet};
use super::reconcile_claim::DurableClaim;
use super::session_reconcile::{
    BOOT_SESSION_ADMISSION_TIMEOUT, ReconcileFailure, SessionReconciler, release_admissions,
};
use crate::agents::conversation_recovery::RecoveryReferences;
use crate::event_store::DurableEventKind;
use crate::session::sinks::SessionEventError;
use crate::shell_spec::ShellSpec;

/// One row's reserved claims and the session identity they belong to. A claim
/// is given back on drop only if the pass never reached its outcome; the pass's
/// `finally` gives the rest back.
pub(crate) struct Admission {
    pub(crate) session_id: SessionId,
    pub(crate) channel_id: ChannelId,
    pub(crate) cwd: String,
    pub(crate) shell_spec: ShellSpec,
    pub(crate) agent_reference: Option<AgentConversationReferenceV1>,
    pub(crate) resume_close: DurableClaim,
    pub(crate) respawn_event: DurableClaim,
    pub(crate) future_close: DurableClaim,
}

impl SessionReconciler {
    /// The open-session rows, read with the admission deadline: a coordinator
    /// that does not answer in time is recoverable, as v2 treats it.
    pub(crate) async fn read_rows(&self) -> Result<OpenSessionSet, ReconcileFailure> {
        match tokio::time::timeout(BOOT_SESSION_ADMISSION_TIMEOUT, self.sessions.read()).await {
            Ok(Ok(set)) => Ok(set),
            Ok(Err(error)) => Err(ReconcileFailure::recoverable(error.to_string())),
            Err(_) => Err(ReconcileFailure::recoverable(format!(
                "sessionsList timed out after {}ms",
                BOOT_SESSION_ADMISSION_TIMEOUT.as_millis()
            ))),
        }
    }

    /// v2 `:111-165`: capacity for every durable path of the complete set, or
    /// nothing is touched and every claim taken so far is given back.
    pub(crate) async fn admit_all(
        &self,
        rows: &[OpenSession],
        references: &RecoveryReferences,
    ) -> Result<Vec<Admission>, ReconcileFailure> {
        let mut admissions: Vec<Admission> = Vec::with_capacity(rows.len());
        for row in rows {
            match self
                .admit(row, references.get(&row.id).cloned().flatten())
                .await
            {
                Ok(Some(admission)) => admissions.push(admission),
                Ok(None) => {}
                Err(refusal) => {
                    release_admissions(admissions).await;
                    return Err(refusal);
                }
            }
        }
        Ok(admissions)
    }

    async fn admit(
        &self,
        row: &OpenSession,
        agent_reference: Option<AgentConversationReferenceV1>,
    ) -> Result<Option<Admission>, ReconcileFailure> {
        let (Ok(session_id), Ok(channel_id)) = (
            SessionId::try_from(row.id.clone()),
            ChannelId::try_from(i64::from(row.channel)),
        ) else {
            tracing::warn!(session = %row.id, channel = row.channel, "reconcile: a coordinator row names an id this worker cannot address; it is skipped");
            return Ok(None);
        };
        let shell_spec = self
            .manager
            .resolve_shell_spec(&row.cwd, session_id.as_str())
            .map_err(ReconcileFailure::recoverable)?;
        let resume_close = self.claim(DurableEventKind::Closed).await?;
        let respawn_event = match self.claim(DurableEventKind::State).await {
            Ok(claim) => claim,
            Err(refusal) => {
                resume_close.release().await;
                return Err(refusal);
            }
        };
        let future_close = match self.claim(DurableEventKind::Closed).await {
            Ok(claim) => claim,
            Err(refusal) => {
                respawn_event.release().await;
                resume_close.release().await;
                return Err(refusal);
            }
        };
        Ok(Some(Admission {
            session_id,
            channel_id,
            cwd: row.cwd.clone(),
            shell_spec,
            agent_reference,
            resume_close,
            respawn_event,
            future_close,
        }))
    }

    /// A full outbox is recoverable (v2 `isSessionEventOutboxFullError`); a
    /// store that refused in its own right is the durability failure the
    /// worker must not survive (v2 `isSessionEventDurabilityError`).
    async fn claim(&self, kind: DurableEventKind) -> Result<DurableClaim, ReconcileFailure> {
        DurableClaim::take(&self.manager, kind)
            .await
            .map_err(|error| match error {
                SessionEventError::Reserve(_) => {
                    ReconcileFailure::recoverable(format!("session event outbox full: {error}"))
                }
                other => ReconcileFailure {
                    reason: other.to_string(),
                    fatal: true,
                },
            })
    }
}
