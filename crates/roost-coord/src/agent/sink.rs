//! The harness ChatSink: one writer task per conversation stores event batches
//! in order and publishes each to this replica's bus as soon as it is durable,
//! then tells the other replicas. Streamed deltas never wait on the database;
//! any other event returns once stored, as it always has. Snapshot projection
//! remains in the runtime journal; `chat_events::relay` carries batches
//! appended elsewhere.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use futures::future::BoxFuture;
use roost_agent::{AgentRuntime, ChatSink};
use roost_protocol::wire::agent_chat::{ChatEvent, ConversationSummary, TranscriptItem};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use super::chat_delivery::ChatDelivery;
use crate::cluster::notify::{CHAT_CHANNEL, ClusterNotifier};
use crate::db::CoordDb;
use crate::events::bus_domains::Buses;
use crate::events::bus_messages::AgentConversationUpdate;

/// How long streamed text gathers into one durable batch: one display frame.
/// The writer is the only party that waits; the harness hands events over
/// without blocking, so a slow append makes the next batch bigger rather
/// than stalling the stream.
const DELTA_WINDOW: Duration = Duration::from_millis(16);

enum WriterMessage {
    Events(Vec<ChatEvent>),
    /// Store everything queued so far, then answer.
    Flush(oneshot::Sender<()>),
}

/// What a writer task needs to store and announce a batch.
#[derive(Clone)]
struct BatchStore {
    db: CoordDb,
    notifier: Arc<ClusterNotifier>,
    delivery: Arc<ChatDelivery>,
}

/// Coordinator-side durable delivery of harness events.
pub struct CoordChatSink {
    store: BatchStore,
    writers: Mutex<HashMap<String, mpsc::UnboundedSender<WriterMessage>>>,
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
    pub fn new(db: CoordDb, notifier: Arc<ClusterNotifier>, buses: Arc<Buses>) -> Self {
        Self {
            store: BatchStore {
                db,
                notifier,
                delivery: Arc::new(ChatDelivery::new(buses)),
            },
            writers: Mutex::new(HashMap::new()),
            runtime: OnceLock::new(),
        }
    }

    pub fn bind(&self, runtime: AgentRuntime) {
        let _ = self.runtime.set(runtime);
    }

    pub async fn snapshot(
        &self,
        id: &str,
    ) -> Result<(u64, roost_protocol::wire::agent_chat::Transcript), roost_agent::AgentError> {
        let runtime = self
            .runtime
            .get()
            .ok_or_else(|| roost_agent::AgentError::Store("agent runtime is not bound".into()))?;
        let (cursor, rows) = super::chat_events::snapshot_rows(&self.store.db, id)
            .await
            .map_err(|error| roost_agent::AgentError::Store(error.to_string()))?;
        let mut transcript = runtime.transcript(id).await?;
        let queued_ids: HashSet<_> = transcript
            .queued
            .iter()
            .map(|message| message.id.clone())
            .collect();
        let user_ids: HashSet<_> = transcript
            .items
            .iter()
            .filter_map(|item| match item {
                TranscriptItem::User { id, .. } => Some(id.clone()),
                _ => None,
            })
            .chain(queued_ids.iter().cloned())
            .collect();
        let start = rows
            .first()
            .map(|row| row.cursor)
            .unwrap_or(cursor.saturating_add(1));
        for row in rows.into_iter().filter(|row| row.cursor >= start) {
            let events: Vec<ChatEvent> =
                serde_json::from_str(&row.events_json).map_err(|error| {
                    roost_agent::AgentError::Store(format!(
                        "invalid persisted chat event batch: {error}"
                    ))
                })?;
            for event in &events {
                // Journal membership is newer than these batches, including withdrawals
                // committed before their removal event could be persisted.
                match event {
                    ChatEvent::Reset {
                        transcript: replacement,
                    } => {
                        let mut replacement = replacement.clone();
                        replacement
                            .queued
                            .retain(|message| queued_ids.contains(&message.id));
                        replacement.items.retain(|item| {
                            !matches!(item,
                                TranscriptItem::User { id, .. } if !user_ids.contains(id)
                            )
                        });
                        transcript = replacement;
                    }
                    ChatEvent::Queued { message } if !queued_ids.contains(&message.id) => {}
                    ChatEvent::Item {
                        item: TranscriptItem::User { id, .. },
                    } if !user_ids.contains(id) => {}
                    _ => roost_protocol::wire::agent_chat::fold_chat_event(&mut transcript, event),
                }
            }
        }

        Ok((cursor, transcript))
    }

    pub(crate) fn database(&self) -> CoordDb {
        self.store.db.clone()
    }

    pub(crate) fn buses(&self) -> Arc<Buses> {
        self.store.delivery.buses()
    }

    pub(crate) fn delivery(&self) -> Arc<ChatDelivery> {
        Arc::clone(&self.store.delivery)
    }

    /// Hands `message` to the conversation's writer, starting one if it has
    /// none or the last one ended.
    fn send(&self, conversation_id: &str, message: WriterMessage) {
        let mut writers = self.writers.lock().unwrap_or_else(PoisonError::into_inner);
        let message = match writers.get(conversation_id) {
            Some(writer) => match writer.send(message) {
                Ok(()) => return,
                Err(mpsc::error::SendError(message)) => message,
            },
            None => message,
        };
        let (writer, inbox) = mpsc::unbounded_channel();
        // A fresh channel's receiver is alive until the task below ends.
        let _ = writer.send(message);
        tokio::spawn(write_conversation(
            conversation_id.to_owned(),
            inbox,
            self.store.clone(),
        ));
        writers.insert(conversation_id.to_owned(), writer);
    }
}

/// Stores one conversation's events in arrival order. Text and thinking
/// deltas wait up to `DELTA_WINDOW` to share a batch; any other event first
/// stores the deltas before it, so the durable order is the harness's order.
async fn write_conversation(
    conversation_id: String,
    mut inbox: mpsc::UnboundedReceiver<WriterMessage>,
    store: BatchStore,
) {
    let mut deltas: Vec<ChatEvent> = Vec::new();
    let mut deadline: Option<Instant> = None;
    loop {
        let message = match deadline {
            Some(at) => match tokio::time::timeout_at(at, inbox.recv()).await {
                Ok(message) => message,
                Err(_) => {
                    store
                        .persist(&conversation_id, std::mem::take(&mut deltas))
                        .await;
                    deadline = None;
                    continue;
                }
            },
            None => inbox.recv().await,
        };
        let Some(message) = message else {
            store.persist(&conversation_id, deltas).await;
            return;
        };
        match message {
            WriterMessage::Events(events) => {
                for event in events {
                    if is_delta(&event) {
                        deltas.push(event);
                        deadline.get_or_insert_with(|| Instant::now() + DELTA_WINDOW);
                    } else {
                        store
                            .persist(&conversation_id, std::mem::take(&mut deltas))
                            .await;
                        deadline = None;
                        store.persist(&conversation_id, vec![event]).await;
                    }
                }
            }
            WriterMessage::Flush(done) => {
                store
                    .persist(&conversation_id, std::mem::take(&mut deltas))
                    .await;
                deadline = None;
                let _ = done.send(());
            }
        }
    }
}

impl BatchStore {
    /// Appends one batch, publishes it here at once, then notifies every
    /// replica; the local relay finds it already delivered.
    async fn persist(&self, conversation_id: &str, events: Vec<ChatEvent>) {
        if events.is_empty() {
            return;
        }
        let events_json = match serde_json::to_string(&events) {
            Ok(json) => json,
            Err(error) => {
                tracing::warn!(conversation_id, %error, "agent chat events did not serialize");
                return;
            }
        };
        let run_start = events.iter().any(|event| {
            matches!(
                event,
                ChatEvent::RunState {
                    run_state: roost_protocol::wire::agent_chat::AgentRunState::Running,
                    ..
                }
            )
        });
        match super::chat_events::append(&self.db, conversation_id, &events_json, run_start).await {
            Ok(cursor) => {
                self.delivery.deliver(conversation_id, cursor, events_json);
                self.notifier
                    .notify(CHAT_CHANNEL, &format!("{conversation_id}:{cursor}"))
                    .await;
            }
            Err(error) => {
                tracing::error!(conversation_id, %error, "agent chat event batch was not persisted")
            }
        }
    }
}

fn is_delta(event: &ChatEvent) -> bool {
    matches!(
        event,
        ChatEvent::TextDelta { .. } | ChatEvent::ThinkingDelta { .. }
    )
}

impl ChatSink for CoordChatSink {
    fn conversation<'a>(&'a self, summary: &'a ConversationSummary) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.buses()
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
            if events.is_empty() {
                return;
            }
            let settles = events.iter().any(|event| !is_delta(event));
            self.send(conversation_id, WriterMessage::Events(events));
            // A snapshot folds stored batches over the runtime journal, so an
            // item boundary must be stored before the harness moves past it.
            if settles {
                let (done, stored) = oneshot::channel();
                self.send(conversation_id, WriterMessage::Flush(done));
                let _ = stored.await;
            }
        })
    }

    fn removed<'a>(&'a self, conversation_id: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let (done, stored) = oneshot::channel();
            self.send(conversation_id, WriterMessage::Flush(done));
            let _ = stored.await;
            // Dropping the sender ends the writer once its queue is empty.
            self.writers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(conversation_id);
            self.buses()
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
