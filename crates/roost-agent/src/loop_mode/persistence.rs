//! Journal snapshots for `/loop`: configuration, limits, and recovery markers.
//! The journal is the source of truth; the runtime map only accelerates a live loop.

use tokio_util::sync::CancellationToken;

use super::state::{LoopState, PersistedLoopState};
use crate::error::AgentError;
use crate::journal::effective_entries;
use crate::records::Entry;
use crate::runtime::AgentRuntime;

impl AgentRuntime {
    pub(crate) async fn restore_loop(&self, id: &str) -> Result<(), AgentError> {
        if self.loop_is_enabled(id) {
            return Ok(());
        }
        let entries = self.inner.store.entries(id).await?;
        let effective = effective_entries(&entries);
        let Some(state_json) = effective.iter().rev().find_map(|(_, entry)| match entry {
            Entry::LoopState { state_json } => Some(state_json.clone()),
            _ => None,
        }) else {
            return Ok(());
        };
        let Some(state_json) = state_json else {
            self.take_loop(id, None);
            tracing::info!(conversation_id = id, "loop recovery found a stopped loop");
            return Ok(());
        };
        let persisted: PersistedLoopState = serde_json::from_str(&state_json).map_err(|error| {
            AgentError::Store(format!("could not decode persisted loop state: {error}"))
        })?;
        let pending_repeat = persisted.pending_repeat.clone();
        let pending_capture = persisted.pending_capture.clone();
        let mut state = persisted.into_loop();
        let repeat_committed = pending_repeat.as_ref().and_then(|pending| {
            effective.iter().find_map(|(seq, entry)| {
                (*seq > pending.after_seq
                    && matches!(entry, Entry::User { text, loop_repeat: true, .. } if text == &pending.body))
                    .then_some(*seq)
            })
        });
        if let Some(seq) = repeat_committed {
            state.budget = state.budget.spend();
            state.condition_ready = false;
            state.pending_repeat = None;
            tracing::info!(
                conversation_id = id,
                user_seq = seq,
                "recovered committed loop repeat budget"
            );
        }
        let capture_committed = pending_capture.as_ref().and_then(|pending| {
            effective.iter().find_map(|(seq, entry)| {
                (*seq > pending.after_seq
                    && matches!(entry, Entry::User { text, loop_repeat: false, .. } if text == &pending.body))
                    .then_some((*seq, pending.body.clone()))
            })
        });
        if let Some((seq, body)) = capture_committed {
            state.body = Some(body);
            state.paused = false;
            state.budget = state.budget.spend();
            state.condition_ready = false;
            tracing::info!(
                conversation_id = id,
                user_seq = seq,
                "recovered committed loop prompt capture"
            );
        }
        let reconciled = pending_repeat.is_some() || pending_capture.is_some();
        state.pending_capture = None;
        self.take_loop(id, None);
        let ended = state.ended.clone();
        self.loops().insert(id.to_owned(), state);
        if reconciled {
            self.persist_loop_state(id, Some(&ended)).await?;
        }
        tracing::info!(
            conversation_id = id,
            repeat_committed = repeat_committed.is_some(),
            "loop state restored"
        );
        if let Some(message) = self
            .with_loop(id, |state| {
                state.budget.exhausted(std::time::Instant::now())
            })
            .flatten()
        {
            self.stop_loop(id, None, "info", message).await;
        }
        Ok(())
    }

    pub(super) async fn persist_loop_state(
        &self,
        id: &str,
        expected: Option<&CancellationToken>,
    ) -> Result<bool, AgentError> {
        let state_json = self
            .with_loop(id, |state| PersistedLoopState::from_loop(state))
            .map(|state| encode_state(&state))
            .transpose()?;
        self.append_loop_state(id, state_json, expected).await
    }

    pub(super) async fn update_loop_state(
        &self,
        id: &str,
        expected: Option<&CancellationToken>,
        change: impl FnOnce(&mut LoopState),
    ) -> Result<bool, AgentError> {
        let state_json = if let Some(expected) = expected {
            self.with_loop_for(id, expected, |state| {
                change(state);
                encode_state(&PersistedLoopState::from_loop(state))
            })
        } else {
            self.with_loop(id, |state| {
                change(state);
                encode_state(&PersistedLoopState::from_loop(state))
            })
        };
        let Some(state_json) = state_json else {
            return Ok(false);
        };
        self.append_loop_state(id, Some(state_json?), expected)
            .await
    }

    pub(super) async fn persist_loop_off(
        &self,
        id: &str,
        expected: Option<&CancellationToken>,
    ) -> Result<bool, AgentError> {
        self.append_loop_state(id, None, expected).await
    }

    pub(super) async fn reconcile_pending_loop_repeat(&self, id: &str) -> Result<(), AgentError> {
        let Some((pending, expected)) = self
            .with_loop(id, |state| {
                state
                    .pending_repeat
                    .clone()
                    .map(|pending| (pending, state.ended.clone()))
            })
            .flatten()
        else {
            return Ok(());
        };
        let committed = self.inner.store.entries(id).await?.iter().any(|(sequence, entry)| {
            *sequence > pending.after_seq
                && matches!(entry, Entry::User { text, loop_repeat: true, .. } if text == &pending.body)
        });
        self.update_loop_state(id, Some(&expected), |state| {
            if committed {
                state.budget = state.budget.spend();
                state.condition_ready = false;
            }
            state.pending_repeat = None;
        })
        .await?;
        tracing::info!(
            conversation_id = id,
            committed,
            "loop repeat admission reconciled after storage failure"
        );
        Ok(())
    }

    async fn append_loop_state(
        &self,
        id: &str,
        state_json: Option<String>,
        expected: Option<&CancellationToken>,
    ) -> Result<bool, AgentError> {
        let entry = Entry::LoopState { state_json };
        let result = if let Some(expected) = expected {
            self.append_guarded(id, entry, &[expected]).await
        } else {
            self.append(id, entry).await
        };
        match result {
            Ok(_) => Ok(true),
            Err(AgentError::Cancelled) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

fn encode_state(state: &PersistedLoopState) -> Result<String, AgentError> {
    serde_json::to_string(state)
        .map_err(|error| AgentError::Store(format!("could not encode loop state: {error}")))
}
