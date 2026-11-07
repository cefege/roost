//! Durable chat delivery: committed event batches are published locally and
//! across replicas, and conversation summaries fan out on the agent bus.
//! `chat_events::relay` carries remote batches; snapshots read the stored batches.

use std::sync::Arc;

use futures::future::BoxFuture;
use roost_protocol::wire::agent_chat::ConversationSummary;

use super::chat_delivery::ChatDelivery;
use crate::cluster::notify::CHAT_CHANNEL;
use crate::cluster::notify::ClusterNotifier;
use crate::db::CoordDb;
use crate::events::bus_domains::Buses;
use crate::events::bus_messages::AgentConversationUpdate;

/// The coordinator's sink for conversation summaries and removals.
pub trait ChatSink: Send + Sync {
    fn conversation<'a>(&'a self, summary: &'a ConversationSummary) -> BoxFuture<'a, ()>;
    fn removed<'a>(&'a self, conversation_id: &'a str) -> BoxFuture<'a, ()>;
}

/// What a committed batch needs to reach every replica.
struct BatchStore {
    db: CoordDb,
    notifier: Arc<ClusterNotifier>,
    delivery: Arc<ChatDelivery>,
}

/// Coordinator-side durable delivery of chat events.
pub struct CoordChatSink {
    store: BatchStore,
    buses: Arc<Buses>,
}

impl std::fmt::Debug for CoordChatSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CoordChatSink")
            .finish_non_exhaustive()
    }
}

impl CoordChatSink {
    pub fn new(db: CoordDb, notifier: Arc<ClusterNotifier>, buses: Arc<Buses>) -> Self {
        Self {
            store: BatchStore {
                db,
                notifier,
                delivery: Arc::new(ChatDelivery::new(Arc::clone(&buses))),
            },
            buses,
        }
    }

    pub(crate) fn database(&self) -> CoordDb {
        self.store.db.clone()
    }

    pub(crate) fn delivery(&self) -> Arc<ChatDelivery> {
        Arc::clone(&self.store.delivery)
    }

    pub(crate) async fn publish_committed(
        &self,
        conversation_id: &str,
        cursor: u64,
        events_json: String,
    ) {
        self.store
            .publish_committed(conversation_id, cursor, events_json)
            .await;
    }
}

impl BatchStore {
    async fn publish_committed(&self, conversation_id: &str, cursor: u64, events_json: String) {
        self.delivery.deliver(conversation_id, cursor, events_json);
        self.notifier
            .notify(CHAT_CHANNEL, &format!("{conversation_id}:{cursor}"))
            .await;
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

    fn removed<'a>(&'a self, conversation_id: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
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
