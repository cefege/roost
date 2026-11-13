//! The browser's replica of agent conversations and transcripts.

use std::collections::{BTreeMap, HashMap};

use super::OmpPresentationClocks;
use roost_protocol::wire::agent_chat::{
    ChatEvent, ConversationSummary, Transcript, fold_chat_event,
};

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedTranscript {
    pub seq: u64,
    /// Advances on Agent events even when the selected values are unchanged:
    /// OMP may have changed its available choices without switching models.
    pub settings_seq: u64,
    pub transcript: Transcript,
    pub presentation_clocks: OmpPresentationClocks,
    pub stale: bool,
}

impl LoadedTranscript {
    fn fold_events(&mut self, seq: u64, events: &[ChatEvent], now_ms: u64) {
        for event in events {
            fold_chat_event(&mut self.transcript, event);
            if matches!(event, ChatEvent::Agent { .. }) {
                self.settings_seq = seq;
            }
            if matches!(
                event,
                ChatEvent::OmpUi { .. } | ChatEvent::OmpUiReset { .. }
            ) && self.transcript.omp_ui_error.is_none()
            {
                self.presentation_clocks.reconcile(&self.transcript, now_ms);
            }
        }
        self.seq = seq;
    }
}

#[derive(Debug, Clone, Default)]
pub struct AgentChatState {
    pub host_connected: bool,
    pub conversations: BTreeMap<String, ConversationSummary>,
    pub transcripts: HashMap<String, LoadedTranscript>,
    snapshot_generations: HashMap<String, u64>,
}

impl AgentChatState {
    pub fn hydrate(&mut self, conversations: Vec<ConversationSummary>, host_connected: bool) {
        self.conversations = conversations
            .into_iter()
            .map(|row| (row.id.clone(), row))
            .collect();
        self.host_connected = host_connected;
        for loaded in self.transcripts.values_mut() {
            loaded.stale = true;
        }
    }

    /// The canonical chat sequence this replica has folded, or `None` while
    /// no snapshot has loaded. A receipt whose `after_seq` is above this is
    /// fenced until the missing events arrive.
    pub fn loaded_seq(&self, id: &str) -> Option<u64> {
        self.transcripts.get(id).map(|loaded| loaded.seq)
    }

    pub fn apply_conversation_frame(
        &mut self,
        id: String,
        removed: bool,
        conversation: Option<ConversationSummary>,
        host_connected: bool,
    ) {
        // Only the frame without a conversation speaks for the host; on a
        // conversation upsert the flag is unset, not "offline" (spec C3).
        if id.is_empty() {
            self.host_connected = host_connected;
            return;
        }
        if removed {
            self.conversations.remove(&id);
            self.transcripts.remove(&id);
            self.snapshot_generations.remove(&id);
        } else if let Some(conversation) = conversation {
            self.conversations.insert(id, conversation);
        }
    }

    pub fn apply_events(&mut self, id: &str, seq: u64, events: &[ChatEvent], now_ms: u64) {
        let Some(loaded) = self.transcripts.get_mut(id) else {
            return;
        };
        if seq <= loaded.seq {
            return;
        }
        if loaded.seq.checked_add(1) != Some(seq) {
            loaded.stale = true;
            return;
        }
        loaded.fold_events(seq, events, now_ms);
    }

    pub fn request_snapshot(&mut self, id: &str, generation: u64) {
        self.snapshot_generations.insert(id.to_owned(), generation);
    }

    pub fn load_snapshot(
        &mut self,
        id: String,
        generation: u64,
        seq: u64,
        transcript: Transcript,
        now_ms: u64,
    ) {
        if self
            .snapshot_generations
            .get(&id)
            .is_some_and(|latest| generation < *latest)
        {
            return;
        }
        if self
            .transcripts
            .get(&id)
            .is_some_and(|loaded| !loaded.stale && seq < loaded.seq)
        {
            return;
        }
        let mut presentation_clocks = self
            .transcripts
            .get_mut(&id)
            .map(|loaded| std::mem::take(&mut loaded.presentation_clocks))
            .unwrap_or_default();
        presentation_clocks.reconcile(&transcript, now_ms);
        self.transcripts.insert(
            id,
            LoadedTranscript {
                seq,
                settings_seq: seq,
                transcript,
                presentation_clocks,
                stale: false,
            },
        );
    }

    pub fn apply_event_batches(
        &mut self,
        id: &str,
        batches: &[(u64, Vec<ChatEvent>)],
        truncated: bool,
        now_ms: u64,
    ) {
        let Some(loaded) = self.transcripts.get_mut(id) else {
            return;
        };
        if truncated {
            loaded.stale = true;
            return;
        }
        let was_stale = loaded.stale;
        let mut applied = false;
        for (seq, events) in batches {
            if *seq <= loaded.seq {
                continue;
            }
            if loaded.seq.checked_add(1) != Some(*seq) {
                loaded.stale = true;
                return;
            }
            loaded.fold_events(*seq, events, now_ms);
            applied = true;
        }
        loaded.stale = was_stale && !applied;
    }
    pub fn conversations_in_folder(
        &self,
        paths: &dyn crate::store::paths::WorkerPaths,
        store: &crate::store::Store,
        folder_key: &str,
    ) -> Vec<String> {
        let mut conversations: Vec<_> = self
            .conversations
            .values()
            .filter(|conversation| {
                !conversation.is_native() && {
                    let worker_os = store
                        .workers
                        .get(&conversation.worker_fp)
                        .map(|worker| worker.os.as_str());
                    crate::store::paths::folder_key_of(
                        paths,
                        worker_os,
                        &conversation.worker_fp,
                        &conversation.cwd,
                    ) == folder_key
                }
            })
            .collect();
        conversations.sort_by_key(|conversation| std::cmp::Reverse(conversation.updated_ms));
        conversations
            .into_iter()
            .map(|conversation| conversation.id.clone())
            .collect()
    }

    pub fn forget(&mut self, id: &str) {
        self.transcripts.remove(id);
        self.snapshot_generations.remove(id);
    }
}
