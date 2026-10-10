//! The harness's `ChatSink`: folds each conversation's events into a cached
//! transcript, numbers them per conversation, and publishes them and the
//! conversation rows on the Sync buses. `AgentChatSnapshot` reads the cache.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use futures::future::BoxFuture;
use roost_agent::{AgentRuntime, ChatSink};
use roost_protocol::wire::agent_chat::{
    ChatEvent, ConversationSummary, Transcript, fold_chat_event,
};

use crate::events::bus_domains::Buses;
use crate::events::bus_messages::{AgentChatEventsUpdate, AgentConversationUpdate};

/// Transcripts kept folded in memory; older ones reload from the store.
const CACHED_TRANSCRIPTS: usize = 64;

#[derive(Default)]
struct CacheState {
    transcripts: HashMap<String, Transcript>,
    recency: VecDeque<String>,
    /// Never evicted: a client's snapshot seq must keep meaning the same thing.
    seqs: HashMap<String, u64>,
}

/// Coordinator-side delivery of harness events.
pub struct CoordChatSink {
    buses: Arc<Buses>,
    state: Mutex<CacheState>,
    runtime: OnceLock<AgentRuntime>,
}

impl std::fmt::Debug for CoordChatSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CoordChatSink")
            .finish_non_exhaustive()
    }
}

impl CoordChatSink {
    pub fn new(buses: Arc<Buses>) -> Self {
        Self {
            buses,
            state: Mutex::new(CacheState::default()),
            runtime: OnceLock::new(),
        }
    }

    /// Binds the runtime whose store reloads evicted transcripts.
    pub fn bind(&self, runtime: AgentRuntime) {
        let _ = self.runtime.set(runtime);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CacheState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The folded transcript and the seq of the last event folded into it.
    pub async fn snapshot(&self, id: &str) -> Result<(u64, Transcript), roost_agent::AgentError> {
        if let Some(found) = self.cached(id) {
            return Ok(found);
        }
        let transcript = self.load(id).await?;
        let mut state = self.lock();
        let seq = state.seqs.get(id).copied().unwrap_or(0);
        if !state.transcripts.contains_key(id) {
            remember(&mut state, id, transcript.clone());
            return Ok((seq, transcript));
        }
        let cached = state.transcripts.get(id).cloned().unwrap_or(transcript);
        Ok((seq, cached))
    }

    fn cached(&self, id: &str) -> Option<(u64, Transcript)> {
        let state = self.lock();
        let transcript = state.transcripts.get(id)?.clone();
        Some((state.seqs.get(id).copied().unwrap_or(0), transcript))
    }

    async fn load(&self, id: &str) -> Result<Transcript, roost_agent::AgentError> {
        let runtime = self
            .runtime
            .get()
            .ok_or_else(|| roost_agent::AgentError::Store("agent runtime is not bound".into()))?;
        runtime.transcript(id).await
    }
}

fn remember(state: &mut CacheState, id: &str, transcript: Transcript) {
    state.transcripts.insert(id.to_owned(), transcript);
    state.recency.retain(|known| known != id);
    state.recency.push_back(id.to_owned());
    while state.recency.len() > CACHED_TRANSCRIPTS {
        if let Some(oldest) = state.recency.pop_front() {
            state.transcripts.remove(&oldest);
        }
    }
}

impl ChatSink for CoordChatSink {
    fn conversation<'a>(&'a self, summary: &'a ConversationSummary) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.buses
                .agent_conversation_bus
                .publish(AgentConversationUpdate {
                    conversation_id: summary.id.clone(),
                    removed: false,
                    conversation: Some(summary.clone()),
                    host_connected: None,
                });
        })
    }

    fn events<'a>(&'a self, conversation_id: &'a str, events: Vec<ChatEvent>) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            // The first event of an uncached conversation folds onto its stored
            // transcript, so the cache never starts from an empty one.
            let loaded = if self.cached(conversation_id).is_none() {
                self.load(conversation_id).await.ok()
            } else {
                None
            };
            let seq = {
                let mut state = self.lock();
                if let Some(transcript) = loaded
                    && !state.transcripts.contains_key(conversation_id)
                {
                    remember(&mut state, conversation_id, transcript);
                }
                if let Some(transcript) = state.transcripts.get_mut(conversation_id) {
                    for event in &events {
                        fold_chat_event(transcript, event);
                    }
                }
                let seq = state.seqs.entry(conversation_id.to_owned()).or_insert(0);
                *seq = seq.saturating_add(1);
                *seq
            };
            let events_json = match serde_json::to_string(&events) {
                Ok(json) => json,
                Err(error) => {
                    tracing::warn!(conversation_id, %error, "agent chat events did not serialize");
                    return;
                }
            };
            self.buses.agent_chat_bus.publish(AgentChatEventsUpdate {
                conversation_id: conversation_id.to_owned(),
                seq,
                events_json,
            });
        })
    }

    fn removed<'a>(&'a self, conversation_id: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            {
                let mut state = self.lock();
                state.transcripts.remove(conversation_id);
                state.recency.retain(|known| known != conversation_id);
            }
            self.buses
                .agent_conversation_bus
                .publish(AgentConversationUpdate {
                    conversation_id: conversation_id.to_owned(),
                    removed: true,
                    conversation: None,
                    host_connected: None,
                });
        })
    }
}
