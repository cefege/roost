//! Owns the single sequential advisor queue for each conversation and keeps
//! runtime state private to that background task.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use tokio::sync::{Mutex as AsyncMutex, mpsc};

use crate::records::ConversationRecord;
use crate::runtime::AgentRuntime;

use super::delta::render_delta;
use super::review::{ReviewState, review_with_retries};

#[derive(Debug, Default)]
pub(crate) struct AdvisorHub {
    conversations: Mutex<HashMap<String, AdvisorQueue>>,
}

#[derive(Debug, Clone)]
pub(crate) struct AdvisorStatus {
    pub state: &'static str,
    pub model: String,
    pub reviews: u64,
    pub accepted: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

struct AdvisorQueue {
    sender: mpsc::UnboundedSender<AdvisorJob>,
    state: std::sync::Arc<AsyncMutex<ReviewState>>,
}

#[derive(Debug, Clone, Copy)]
enum AdvisorJob {
    Turn { ended_with_tool_calls: bool },
    Reset,
}

impl std::fmt::Debug for AdvisorQueue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AdvisorQueue")
            .finish_non_exhaustive()
    }
}

impl AdvisorHub {
    pub(crate) fn enqueue(runtime: &AgentRuntime, id: &str, ended_with_tool_calls: bool) {
        let sender = ensure_queue(runtime, id);
        if sender
            .send(AdvisorJob::Turn {
                ended_with_tool_calls,
            })
            .is_err()
        {
            lock(&runtime.inner.advisors.conversations).remove(id);
        }
    }

    pub(crate) async fn status(
        runtime: &AgentRuntime,
        record: &ConversationRecord,
    ) -> AdvisorStatus {
        let queue = lock(&runtime.inner.advisors.conversations)
            .get(&record.id)
            .map(|queue| queue.state.clone());
        let settings = runtime.inner.store.settings().await.unwrap_or_default();
        let enabled =
            record.parent_id.is_none() && record.advisor.unwrap_or(settings.advisor_enabled);
        let model = crate::roles::resolve_role(
            runtime.inner.llm.as_ref(),
            &settings,
            crate::records::Role::Advisor,
        )
        .await
        .map(|resolved| format!("{}/{}", resolved.info.provider, resolved.info.id))
        .unwrap_or_else(|| "unresolved".into());
        let Some(queue) = queue else {
            let state = if !enabled {
                "inactive"
            } else if model == "unresolved" {
                "no_model"
            } else {
                "active"
            };
            return AdvisorStatus {
                state,
                model,
                reviews: 0,
                accepted: 0,
                input_tokens: 0,
                output_tokens: 0,
            };
        };
        let state = queue.lock().await;
        let state_name = if state.halted {
            "halted"
        } else if enabled && model != "unresolved" {
            "active"
        } else if enabled {
            "no_model"
        } else {
            "inactive"
        };
        AdvisorStatus {
            state: state_name,
            model,
            reviews: state.reviews,
            accepted: state.accepted,
            input_tokens: state.input_tokens,
            output_tokens: state.output_tokens,
        }
    }

    pub(crate) fn reset(runtime: &AgentRuntime, id: &str) {
        let sender = ensure_queue(runtime, id);
        let _ = sender.send(AdvisorJob::Reset);
    }

    pub(crate) fn forget(runtime: &AgentRuntime, id: &str) {
        lock(&runtime.inner.advisors.conversations).remove(id);
    }
}

async fn queue_loop(
    runtime: AgentRuntime,
    id: String,
    mut receiver: mpsc::UnboundedReceiver<AdvisorJob>,
    state: std::sync::Arc<AsyncMutex<ReviewState>>,
) {
    while let Some(job) = receiver.recv().await {
        let ended_with_tool_calls = match job {
            AdvisorJob::Reset => {
                let cursor = runtime
                    .inner
                    .store
                    .entries(&id)
                    .await
                    .ok()
                    .and_then(|entries| entries.last().map(|(seq, _)| *seq))
                    .unwrap_or_default();
                let mut state = state.lock().await;
                *state = ReviewState {
                    cursor,
                    seeded: true,
                    ..ReviewState::default()
                };
                continue;
            }
            AdvisorJob::Turn {
                ended_with_tool_calls,
            } => ended_with_tool_calls,
        };
        let record = match runtime.record(&id).await {
            Ok(record) => record,
            Err(error) => {
                tracing::debug!(conversation_id = id, %error, "advisor skipped missing conversation");
                continue;
            }
        };
        let settings = match runtime.inner.store.settings().await {
            Ok(settings) => settings,
            Err(error) => {
                tracing::warn!(conversation_id = id, %error, "advisor settings unavailable");
                continue;
            }
        };
        if record.parent_id.is_some() || !record.advisor.unwrap_or(settings.advisor_enabled) {
            continue;
        }
        let entries = match runtime.inner.store.entries(&id).await {
            Ok(entries) => entries,
            Err(error) => {
                tracing::warn!(conversation_id = id, %error, "advisor transcript unavailable");
                continue;
            }
        };
        let mut review_state = state.lock().await;
        if !review_state.seeded {
            review_state.seeded = true;
            review_state.cursor = entries
                .iter()
                .rev()
                .find_map(|(seq, entry)| {
                    matches!(entry, crate::records::Entry::User { .. })
                        .then_some(seq.saturating_sub(1))
                })
                .or_else(|| entries.last().map(|(seq, _)| *seq))
                .unwrap_or_default();
        }
        if review_state.immune_turns_left > 0 {
            review_state.immune_turns_left -= 1;
        }
        let delta = render_delta(&entries, review_state.cursor);
        if let Some((seq, _)) = entries.last() {
            review_state.cursor = *seq;
        }
        review_with_retries(
            &runtime,
            &record,
            &delta,
            &mut review_state,
            ended_with_tool_calls,
        )
        .await;
    }
}

fn ensure_queue(runtime: &AgentRuntime, id: &str) -> mpsc::UnboundedSender<AdvisorJob> {
    let (sender, receiver, state) = {
        let mut conversations = lock(&runtime.inner.advisors.conversations);
        if let Some(queue) = conversations.get(id) {
            (queue.sender.clone(), None, queue.state.clone())
        } else {
            let (sender, receiver) = mpsc::unbounded_channel();
            let state = std::sync::Arc::new(AsyncMutex::new(ReviewState::default()));
            conversations.insert(
                id.to_owned(),
                AdvisorQueue {
                    sender: sender.clone(),
                    state: state.clone(),
                },
            );
            (sender, Some(receiver), state)
        }
    };
    if let Some(receiver) = receiver {
        let runtime = runtime.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            queue_loop(runtime, id, receiver, state).await;
        });
    }
    sender
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
