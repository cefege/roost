//! The process-local transcript and conversation projection received from the agent host.
//!
//! The follower is the sole writer. RPC reads clone bounded current state under
//! one synchronous mutex and never hold the lock across network work.

use std::collections::{BTreeMap, HashMap};

use roost_protocol::wire::agent_chat::{
    ConversationSummary, HostStreamLine, Transcript, fold_chat_event,
};

/// Updates the Sync firehose can publish from one host line.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentChatUpdate {
    Conversations {
        conversations: Vec<ConversationSummary>,
    },
    Conversation {
        conversation: ConversationSummary,
    },
    ConversationRemoved {
        id: String,
    },
    HostConnected {
        connected: bool,
    },
    Events {
        conversation_id: String,
        seq: u64,
        events_json: String,
    },
}

/// One cached view of the durable host's conversations and transcripts.
#[derive(Debug, Default)]
pub struct ChatCache {
    connected: bool,
    conversations: BTreeMap<String, ConversationSummary>,
    transcripts: HashMap<String, (u64, Transcript)>,
}

impl ChatCache {
    /// Apply one host stream line and return the Sync updates it represents.
    pub fn apply_line(&mut self, line: &HostStreamLine) -> Vec<AgentChatUpdate> {
        match line {
            HostStreamLine::Hello { .. } | HostStreamLine::Ping => Vec::new(),
            HostStreamLine::Conversations { conversations } => {
                let incoming: BTreeMap<_, _> = conversations
                    .iter()
                    .map(|c| (c.id.clone(), c.clone()))
                    .collect();
                let removed: Vec<_> = self
                    .conversations
                    .keys()
                    .filter(|id| !incoming.contains_key(*id))
                    .cloned()
                    .collect();
                self.transcripts.retain(|id, _| incoming.contains_key(id));
                self.conversations = incoming;
                let mut updates: Vec<_> = removed
                    .into_iter()
                    .map(|id| AgentChatUpdate::ConversationRemoved { id })
                    .collect();
                updates.push(AgentChatUpdate::Conversations {
                    conversations: conversations.clone(),
                });
                updates
            }
            HostStreamLine::Conversation { conversation } => {
                self.conversations
                    .insert(conversation.id.clone(), conversation.clone());
                vec![AgentChatUpdate::Conversation {
                    conversation: conversation.clone(),
                }]
            }
            HostStreamLine::ConversationRemoved { id } => {
                self.conversations.remove(id);
                self.transcripts.remove(id);
                vec![AgentChatUpdate::ConversationRemoved { id: id.clone() }]
            }
            HostStreamLine::Chat {
                conversation_id,
                events,
            } => {
                let state = self
                    .transcripts
                    .entry(conversation_id.clone())
                    .or_insert_with(|| (0, empty_transcript()));
                for event in events {
                    fold_chat_event(&mut state.1, event);
                }
                state.0 = state.0.saturating_add(1);
                vec![AgentChatUpdate::Events {
                    conversation_id: conversation_id.clone(),
                    seq: state.0,
                    events_json: serde_json::to_string(events).unwrap_or_else(|_| "[]".to_owned()),
                }]
            }
        }
    }

    /// Set host connectivity and report whether it changed.
    pub fn set_connected(&mut self, connected: bool) -> Option<AgentChatUpdate> {
        if self.connected == connected {
            return None;
        }
        self.connected = connected;
        Some(AgentChatUpdate::HostConnected { connected })
    }

    /// Read the current host connection state and conversation list.
    pub fn conversations(&self) -> (bool, Vec<ConversationSummary>) {
        (
            self.connected,
            self.conversations.values().cloned().collect(),
        )
    }

    /// Read one folded transcript and its host-line sequence.
    pub fn snapshot(&self, id: &str) -> Option<(u64, String)> {
        let (seq, transcript) = self.transcripts.get(id)?;
        serde_json::to_string(transcript)
            .ok()
            .map(|json| (*seq, json))
    }
}

fn empty_transcript() -> Transcript {
    Transcript {
        items: Vec::new(),
        run_state: roost_protocol::wire::agent_chat::AgentRunState::Idle,
        error: None,
        model: None,
        thinking_level: None,
        usage: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversations_replace_rows_and_drop_vanished_transcripts() {
        let mut cache = ChatCache::default();
        let first = summary("a");
        cache.conversations.insert("gone".into(), summary("gone"));
        cache
            .transcripts
            .insert("gone".into(), (1, empty_transcript()));
        cache.apply_line(&HostStreamLine::Conversations {
            conversations: vec![first.clone()],
        });
        assert_eq!(cache.conversations().1, vec![first]);
        assert!(cache.transcripts.is_empty());
    }

    #[test]
    fn chat_line_folds_and_increments_sequence_once() {
        use roost_protocol::wire::agent_chat::{ChatEvent, TranscriptItem};
        let mut cache = ChatCache::default();
        let events = vec![ChatEvent::Item {
            item: TranscriptItem::User {
                id: "u".into(),
                text: "hello".into(),
            },
        }];
        let update = cache.apply_line(&HostStreamLine::Chat {
            conversation_id: "c".into(),
            events,
        });
        assert!(matches!(&update[0], AgentChatUpdate::Events { seq: 1, .. }));
        assert_eq!(cache.snapshot("c").map(|(seq, _)| seq), Some(1));
    }

    #[test]
    fn unknown_snapshot_is_absent() {
        assert!(ChatCache::default().snapshot("missing").is_none());
    }

    fn summary(id: &str) -> ConversationSummary {
        ConversationSummary {
            id: id.into(),
            title: String::new(),
            worker_fp: String::new(),
            worker_label: String::new(),
            cwd: String::new(),
            model: None,
            thinking_level: None,
            run_state: roost_protocol::wire::agent_chat::AgentRunState::Idle,
            error: None,
            created_ms: 0,
            updated_ms: 0,
        }
    }
}
