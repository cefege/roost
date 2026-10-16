//! Chat sink that records conversation rows, events and removals for assertions.
//! Shared by every agent-loop test through `support::RecordingSink`.

use futures::future::BoxFuture;
use parking_lot::Mutex;
use roost_agent::ChatSink;
use roost_protocol::wire::agent_chat::{ChatEvent, ConversationSummary};

/// Records conversation rows and events.
#[derive(Default)]
pub struct RecordingSink {
    pub summaries: Mutex<Vec<ConversationSummary>>,
    pub events: Mutex<Vec<(String, ChatEvent)>>,
    pub removed: Mutex<Vec<String>>,
}

impl RecordingSink {
    pub fn events_for(&self, id: &str) -> Vec<ChatEvent> {
        self.events
            .lock()
            .iter()
            .filter(|(conversation, _)| conversation == id)
            .map(|(_, event)| event.clone())
            .collect()
    }
}

impl ChatSink for RecordingSink {
    fn conversation<'a>(&'a self, summary: &'a ConversationSummary) -> BoxFuture<'a, ()> {
        self.summaries.lock().push(summary.clone());
        Box::pin(async {})
    }

    fn events<'a>(&'a self, conversation_id: &'a str, events: Vec<ChatEvent>) -> BoxFuture<'a, ()> {
        let mut recorded = self.events.lock();
        for event in events {
            recorded.push((conversation_id.to_owned(), event));
        }
        Box::pin(async {})
    }

    fn removed<'a>(&'a self, conversation_id: &'a str) -> BoxFuture<'a, ()> {
        self.removed.lock().push(conversation_id.to_owned());
        Box::pin(async {})
    }
}
