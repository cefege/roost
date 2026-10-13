//! The harness ChatSink durably stores event batches and publishes conversation rows.
//! Snapshot projection remains in the runtime journal; buses are fed by the cluster relay.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures::future::BoxFuture;
use roost_agent::{AgentRuntime, ChatSink};
use roost_protocol::wire::agent_chat::{ChatEvent, ConversationSummary};

use crate::cluster::notify::{CHAT_CHANNEL, ClusterNotifier};
use crate::db::CoordDb;
use crate::events::bus_domains::Buses;
use crate::events::bus_messages::AgentConversationUpdate;

const DELTA_WINDOW: Duration = Duration::from_millis(50);

type Pending = Arc<tokio::sync::Mutex<HashMap<String, Vec<ChatEvent>>>>;

/// Coordinator-side durable delivery of harness events.
pub struct CoordChatSink {
    db: CoordDb,
    notifier: Arc<ClusterNotifier>,
    buses: Arc<Buses>,
    pending: Pending,
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
            db,
            notifier,
            buses,
            pending: Arc::default(),
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
        let (cursor, rows) = super::chat_events::snapshot_rows(&self.db, id)
            .await
            .map_err(|error| roost_agent::AgentError::Store(error.to_string()))?;
        let mut transcript = runtime.transcript(id).await?;
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
                roost_protocol::wire::agent_chat::fold_chat_event(&mut transcript, event);
            }
        }

        Ok((cursor, transcript))
    }

    pub(crate) fn database(&self) -> CoordDb {
        self.db.clone()
    }

    pub(crate) fn buses(&self) -> Arc<Buses> {
        Arc::clone(&self.buses)
    }

    async fn flush_pending(&self, conversation_id: &str) {
        let mut pending = self.pending.lock().await;
        if let Some(events) = pending.remove(conversation_id) {
            self.persist(conversation_id, events).await;
        }
    }

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
                self.notifier
                    .notify(CHAT_CHANNEL, &format!("{conversation_id}:{cursor}"))
                    .await
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
            let mut pending = self.pending.lock().await;
            let mut schedule_flush = false;
            for event in events {
                if is_delta(&event) {
                    pending
                        .entry(conversation_id.to_owned())
                        .or_default()
                        .push(event);
                    schedule_flush = true;
                } else {
                    if let Some(deltas) = pending.remove(conversation_id) {
                        self.persist(conversation_id, deltas).await;
                    }
                    self.persist(conversation_id, vec![event]).await;
                }
            }
            drop(pending);
            if schedule_flush {
                let pending = Arc::clone(&self.pending);
                let db = self.db.clone();
                let notifier = Arc::clone(&self.notifier);
                let id = conversation_id.to_owned();
                tokio::spawn(async move {
                    tokio::time::sleep(DELTA_WINDOW).await;
                    let mut pending = pending.lock().await;
                    if let Some(events) = pending.remove(&id) {
                        let json = match serde_json::to_string(&events) {
                            Ok(json) => json,
                            Err(error) => {
                                tracing::warn!(conversation_id = %id, %error, "agent chat deltas did not serialize");
                                return;
                            }
                        };
                        match super::chat_events::append(&db, &id, &json, false).await {
                            Ok(cursor) => {
                                notifier
                                    .notify(CHAT_CHANNEL, &format!("{id}:{cursor}"))
                                    .await
                            }
                            Err(error) => {
                                tracing::error!(conversation_id = %id, %error, "agent chat delta batch was not persisted")
                            }
                        }
                    }
                });
            }
        })
    }

    fn removed<'a>(&'a self, conversation_id: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.flush_pending(conversation_id).await;
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
