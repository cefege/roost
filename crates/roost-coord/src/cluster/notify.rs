//! Cross-replica wake-ups. On Postgres a notice is `pg_notify(channel,
//! payload)` and a dedicated `LISTEN` connection fans it into this process; on
//! SQLite, where the coordinator is the only replica, notices stay in process.
//! A notice only says "re-read this row"; the database stays the truth, so a
//! lost notice delays work until the next sweep and never corrupts it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::future::BoxFuture;
use sqlx::postgres::PgListener;
use tokio::sync::broadcast;

use crate::db::{CoordDb, DbBackend};

/// The conversation an agent command, abort or deletion touched.
pub const AGENT_CHANNEL: &str = "roost_agent";
/// Durable chat batches persisted by the agent runtime.
pub const CHAT_CHANNEL: &str = "roost_chat";
/// Cross-replica control-plane state changes.
pub const CLUSTER_CHANNEL: &str = "roost_cluster";

/// Every channel this process listens on.
pub const CHANNELS: &[&str] = &[AGENT_CHANNEL, CHAT_CHANNEL, CLUSTER_CHANNEL];

/// Notices buffered per subscriber before a slow one starts missing them.
const NOTICE_BUFFER: usize = 1024;

/// One wake-up: which channel, and the id it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub channel: String,
    pub payload: String,
    /// True for the synthetic notice sent after the listener reconnected:
    /// anything sent while it was down was lost, so receivers resynchronize.
    pub resync: bool,
}

/// This process's sender and listener of cluster notices.
#[derive(Debug)]
pub struct ClusterNotifier {
    db: CoordDb,
    local: broadcast::Sender<Notice>,
    listening: AtomicBool,
}

impl ClusterNotifier {
    #[must_use]
    pub fn new(db: CoordDb) -> Arc<Self> {
        let (local, _) = broadcast::channel(NOTICE_BUFFER);
        Arc::new(Self {
            db,
            local,
            listening: AtomicBool::new(false),
        })
    }

    /// Receives every notice this process hears, its own included.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Notice> {
        self.local.subscribe()
    }

    /// Whether notices from other replicas can arrive: always on SQLite,
    /// and on Postgres while the `LISTEN` connection is up.
    #[must_use]
    pub fn connected(&self) -> bool {
        self.db.backend() == DbBackend::Sqlite || self.listening.load(Ordering::SeqCst)
    }

    /// Sends a notice to every replica, this one included.
    pub async fn notify(&self, channel: &str, payload: &str) {
        match self.db.backend() {
            DbBackend::Sqlite => self.deliver_locally(channel, payload, false),
            DbBackend::Postgres => {
                let sent = sqlx::query("SELECT pg_notify($1, $2)")
                    .bind(channel)
                    .bind(payload)
                    .execute(self.db.pool())
                    .await;
                if let Err(error) = sent {
                    tracing::warn!(channel, %error, "cluster notice was not sent; delivering locally only");
                    self.deliver_locally(channel, payload, false);
                }
            }
        }
    }

    fn deliver_locally(&self, channel: &str, payload: &str, resync: bool) {
        let _ = self.local.send(Notice {
            channel: channel.to_owned(),
            payload: payload.to_owned(),
            resync,
        });
    }

    /// Runs the Postgres `LISTEN` loop until the process exits; returns at
    /// once on SQLite. Reconnects with backoff and announces each reconnect
    /// as a resync notice on every channel.
    pub async fn listen(self: Arc<Self>) {
        if self.db.backend() != DbBackend::Postgres {
            return;
        }
        let url = self.db.pool().connect_options().database_url.to_string();
        let mut delay = Duration::from_millis(500);
        loop {
            match self.listen_once(&url).await {
                Ok(()) => delay = Duration::from_millis(500),
                Err(error) => {
                    tracing::warn!(%error, "cluster notice listener failed; reconnecting");
                    delay = (delay * 2).min(Duration::from_secs(10));
                }
            }
            self.listening.store(false, Ordering::SeqCst);
            tokio::time::sleep(delay).await;
        }
    }

    async fn listen_once(&self, url: &str) -> Result<(), sqlx::Error> {
        let mut listener = PgListener::connect(url).await?;
        listener.listen_all(CHANNELS.iter().copied()).await?;
        self.listening.store(true, Ordering::SeqCst);
        tracing::info!(channels = ?CHANNELS, "cluster notice listener connected");
        for channel in CHANNELS {
            self.deliver_locally(channel, "", true);
        }
        loop {
            match listener.try_recv().await? {
                Some(notification) => {
                    self.deliver_locally(notification.channel(), notification.payload(), false);
                }
                None => {
                    tracing::warn!("cluster notice listener connection lost");
                    return Ok(());
                }
            }
        }
    }
}

/// The agent runtime's view of the notifier: wake the owner of a conversation.
#[derive(Debug, Clone)]
pub struct AgentClusterSignal(pub Arc<ClusterNotifier>);

impl roost_agent::ClusterSignal for AgentClusterSignal {
    fn conversation_changed<'a>(&'a self, conversation_id: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(self.0.notify(AGENT_CHANNEL, conversation_id))
    }
}
