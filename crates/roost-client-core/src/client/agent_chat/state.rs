//! The browser's replica of agent conversations and transcripts.

use std::collections::{BTreeMap, HashMap};

use roost_protocol::wire::agent_chat::{
    ChatEvent, ConversationSummary, Transcript, fold_chat_event,
};

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedTranscript {
    pub seq: u64,
    pub transcript: Transcript,
    pub stale: bool,
}

#[derive(Debug, Clone, Default)]
pub struct AgentChatState {
    pub host_connected: bool,
    pub conversations: BTreeMap<String, ConversationSummary>,
    pub transcripts: HashMap<String, LoadedTranscript>,
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
        } else if let Some(conversation) = conversation {
            self.conversations.insert(id, conversation);
        }
    }

    pub fn apply_events(&mut self, id: &str, seq: u64, events: &[ChatEvent]) {
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
        for event in events {
            fold_chat_event(&mut loaded.transcript, event);
        }
        loaded.seq = seq;
    }

    pub fn load_snapshot(&mut self, id: String, seq: u64, transcript: Transcript) {
        self.transcripts.insert(
            id,
            LoadedTranscript {
                seq,
                transcript,
                stale: false,
            },
        );
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
    }
}
