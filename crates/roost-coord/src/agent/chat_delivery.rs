//! This replica's delivery of durable chat batches to its Sync bus, at most
//! once per cursor and never out of order. The writer that appended a batch
//! publishes it at once (the direct path); the notice relay publishes what
//! another replica appended, or anything the direct path did not reach.
//! Shared by `sink` and `chat_events::relay`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use crate::events::bus_domains::Buses;
use crate::events::bus_messages::AgentChatEventsUpdate;

pub struct ChatDelivery {
    buses: Arc<Buses>,
    /// Conversation → the last cursor this replica published.
    delivered: Mutex<HashMap<String, u64>>,
}

impl std::fmt::Debug for ChatDelivery {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChatDelivery")
            .finish_non_exhaustive()
    }
}

impl ChatDelivery {
    pub fn new(buses: Arc<Buses>) -> Self {
        Self {
            buses,
            delivered: Mutex::new(HashMap::new()),
        }
    }

    pub fn buses(&self) -> Arc<Buses> {
        Arc::clone(&self.buses)
    }

    /// Publishes the batch at `cursor` unless this replica already published
    /// it or a later one. The check and the publish share one lock, so the
    /// direct path and the relay can race without either duplicating or
    /// reordering a batch. Returns whether it published.
    pub fn deliver(&self, conversation_id: &str, cursor: u64, events_json: String) -> bool {
        let mut delivered = self
            .delivered
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if delivered
            .get(conversation_id)
            .is_some_and(|last| *last >= cursor)
        {
            return false;
        }
        self.buses.agent_chat_bus.publish(AgentChatEventsUpdate {
            conversation_id: conversation_id.to_owned(),
            seq: cursor,
            events_json,
        });
        delivered.insert(conversation_id.to_owned(), cursor);
        true
    }

    /// The last cursor published for a conversation, if any.
    pub fn last_delivered(&self, conversation_id: &str) -> Option<u64> {
        self.delivered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(conversation_id)
            .copied()
    }

    /// Starts relaying a conversation this replica has not published yet from
    /// `first`; history before it reaches a client through its snapshot.
    pub fn start_at(&self, conversation_id: &str, first: u64) {
        self.delivered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(conversation_id.to_owned())
            .or_insert_with(|| first.saturating_sub(1));
    }

    /// Every conversation this replica delivers, for a resync after lost notices.
    pub fn conversations(&self) -> Vec<String> {
        self.delivered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::ChatDelivery;
    use crate::events::bus_domains::Buses;

    #[test]
    fn each_cursor_reaches_the_bus_once_in_order_whichever_path_delivers_it() {
        let buses = Arc::new(Buses::new());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        let _subscription = buses.agent_chat_bus.subscribe(move |update| {
            if let Ok(mut seen) = record.lock() {
                seen.push(update.seq);
            }
        });
        let delivery = ChatDelivery::new(Arc::clone(&buses));
        // The relay learns of the conversation from a notice naming cursor 3.
        delivery.start_at("c", 3);
        // The writer publishes 3 directly; the relay's later read finds 3 and 4.
        assert!(delivery.deliver("c", 3, "[]".into()));
        assert!(!delivery.deliver("c", 3, "[]".into()));
        assert!(delivery.deliver("c", 4, "[]".into()));
        // A batch older than one already published is never sent after it.
        assert!(!delivery.deliver("c", 2, "[]".into()));
        let published = seen.lock().map(|seen| seen.clone()).unwrap_or_default();
        assert_eq!(published, [3, 4]);
    }
}
