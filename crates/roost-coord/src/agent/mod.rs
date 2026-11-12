//! Agent lifecycle: durable chat delivery and the worker-hosted OMP backend.
//! The Rust-native harness is gone; `rpi-agent` is the runtime a second
//! backend will build on.

mod chat_delivery;
pub mod chat_events;
pub mod omp;
pub mod rpc_chat;
pub mod rpc_chat_transcript;
pub(crate) mod sink;

use std::sync::{Arc, Mutex, PoisonError};

use crate::cluster::notify::ClusterNotifier;
use crate::db::CoordDb;
use crate::events::bus_domains::Buses;

pub use sink::CoordChatSink;

/// This replica's identity among coordinators sharing one database.
#[derive(Debug, Clone)]
pub struct ClusterMembership {
    pub instance_id: String,
    pub notifier: Arc<ClusterNotifier>,
}

pub struct AgentService {
    pub sink: Arc<CoordChatSink>,
    pub cluster: ClusterMembership,
    background: Mutex<Vec<tokio::task::AbortHandle>>,
}

impl std::fmt::Debug for AgentService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentService")
            .finish_non_exhaustive()
    }
}

impl AgentService {
    /// Durable chat delivery and the cluster membership; the only agent
    /// runtime is the worker-hosted OMP one.
    pub fn new(db: CoordDb, buses: Arc<Buses>, cluster: ClusterMembership) -> Arc<Self> {
        let sink = Arc::new(CoordChatSink::new(db, Arc::clone(&cluster.notifier), buses));
        tracing::info!(
            instance_id = %cluster.instance_id,
            "agent service ready"
        );
        Arc::new(Self {
            sink,
            cluster,
            background: Mutex::new(Vec::new()),
        })
    }

    pub fn join_cluster(&self) {
        let db = self.sink.database();
        self.spawn_background(chat_events::relay(
            Arc::clone(&self.cluster.notifier),
            db.clone(),
            self.sink.delivery(),
        ));
        self.spawn_background(async move {
            let mut prune = tokio::time::interval(std::time::Duration::from_secs(10 * 60));
            loop {
                prune.tick().await;
                if let Err(error) = chat_events::prune(&db).await {
                    tracing::warn!(%error, "agent chat event pruning failed");
                }
            }
        });
    }

    pub async fn leave_cluster(&self) {
        for task in self
            .background
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain(..)
        {
            task.abort();
        }
    }

    fn spawn_background(&self, task: impl Future<Output = ()> + Send + 'static) {
        let handle = tokio::spawn(task).abort_handle();
        self.background
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(handle);
    }
}

/// Milliseconds since the Unix epoch, the timestamp every agent row stores.
pub(crate) fn now_ms() -> i64 {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
}

/// A random lowercase-hex identifier with a readable prefix.
pub(crate) fn random_hex(prefix: &str) -> String {
    let mut bytes = [0_u8; 12];
    if getrandom::fill(&mut bytes).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        bytes.copy_from_slice(&nanos.to_le_bytes()[..12]);
    }
    format!("{prefix}{}", hex::encode(bytes))
}
