//! The coordinator's agent harness: the Rust runtime from roost-agent wired to
//! the coordinator's database, worker links, Sync buses and provider accounts.
//! RPC handlers live in `rpc_*`; `tool_calls` carries tool frames to workers.

mod chat_delivery;
mod chat_events;
pub mod credentials;
mod executor;
mod job_ledger;
mod logins;
pub mod rpc_accounts;
pub mod rpc_auth;
pub mod rpc_chat;
mod rpc_errors;
pub mod rpc_inspect;
pub mod rpc_turns;
mod sink;
pub mod store;
pub mod tool_calls;
mod tool_calls_jobs;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use roost_agent::{AgentRuntime, ChatSink, RoostLlm, RuntimeConfig};
use roost_llm::{AccountPool, CredentialStore, Endpoints};

use crate::cluster::notify::{
    AGENT_CHANNEL, AGENT_SUMMARY_CHANNEL, AgentClusterSignal, ClusterNotifier,
};
use crate::db::CoordDb;
use crate::events::bus_domains::Buses;

pub use logins::{LoginError, LoginRegistry};
pub use sink::CoordChatSink;
use tool_calls::ToolCallRegistry;

/// How often a replica looks for runs whose owner stopped renewing.
const ORPHAN_SWEEP: Duration = Duration::from_secs(15);

/// This replica's identity among coordinators sharing one database.
#[derive(Debug, Clone)]
pub struct ClusterMembership {
    pub instance_id: String,
    pub notifier: Arc<ClusterNotifier>,
}

/// The harness and everything its RPCs need.
pub struct AgentService {
    pub runtime: AgentRuntime,
    pub llm: Arc<RoostLlm>,
    pub sink: Arc<CoordChatSink>,
    pub logins: LoginRegistry,
    pub http: reqwest::Client,
    pub endpoints: Endpoints,
    pub cluster: ClusterMembership,
    background: Mutex<Vec<tokio::task::AbortHandle>>,
}

impl std::fmt::Debug for AgentService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentService")
            .field("endpoints", &self.endpoints)
            .finish_non_exhaustive()
    }
}

impl AgentService {
    /// Builds the harness. `endpoint_overrides` replaces provider base URLs
    /// (`ROOST_AGENT_ENDPOINT_OVERRIDES`).
    pub fn new(
        db: CoordDb,
        buses: Arc<Buses>,
        tool_calls: Arc<ToolCallRegistry>,
        endpoint_overrides: BTreeMap<String, String>,
        cluster: ClusterMembership,
        credential_key: Option<[u8; 32]>,
    ) -> Arc<Self> {
        let registry = Arc::clone(&tool_calls);
        let tools = Arc::new(executor::WorkerToolExecutor::new(tool_calls));
        let config = RuntimeConfig {
            instance_id: cluster.instance_id.clone(),
            ..RuntimeConfig::default()
        };
        let service = Self::with_config(
            db,
            buses,
            tools,
            endpoint_overrides,
            cluster,
            config,
            credential_key,
        );
        registry.bind_runtime(service.runtime.clone());
        service
    }

    /// The harness over any tool executor; tests answer tool calls directly.
    pub fn with_tools(
        db: CoordDb,
        buses: Arc<Buses>,
        tools: Arc<dyn roost_agent::ToolExecutor>,
        endpoint_overrides: BTreeMap<String, String>,
        cluster: ClusterMembership,
    ) -> Arc<Self> {
        let config = RuntimeConfig {
            instance_id: cluster.instance_id.clone(),
            ..RuntimeConfig::default()
        };
        Self::with_config(db, buses, tools, endpoint_overrides, cluster, config, None)
    }

    /// [`Self::with_tools`] with the runtime's tunables; `config.instance_id`
    /// must be `cluster.instance_id`.
    pub fn with_config(
        db: CoordDb,
        buses: Arc<Buses>,
        tools: Arc<dyn roost_agent::ToolExecutor>,
        endpoint_overrides: BTreeMap<String, String>,
        cluster: ClusterMembership,
        config: RuntimeConfig,
        credential_key: Option<[u8; 32]>,
    ) -> Arc<Self> {
        let endpoints = if endpoint_overrides.is_empty() {
            Endpoints::production()
        } else {
            Endpoints::with_overrides(endpoint_overrides)
        };
        let http = reqwest::Client::new();
        let credentials = Arc::new(credentials::CoordCredentialStore::new(
            db.clone(),
            credential_key,
        )) as Arc<dyn CredentialStore>;
        let pool = Arc::new(AccountPool::new(
            credentials,
            http.clone(),
            endpoints.clone(),
        ));
        let logins = LoginRegistry::new(
            db.clone(),
            cluster.instance_id.clone(),
            http.clone(),
            endpoints.clone(),
            Arc::clone(&pool),
        );
        let llm = Arc::new(RoostLlm::new(
            http.clone(),
            Arc::clone(&pool),
            endpoints.clone(),
        ));
        let sink = Arc::new(CoordChatSink::new(
            db.clone(),
            Arc::clone(&cluster.notifier),
            Arc::clone(&buses),
        ));
        let runtime = AgentRuntime::clustered(
            Arc::new(store::CoordAgentStore::new(db)),
            tools,
            Arc::clone(&sink) as Arc<dyn roost_agent::ChatSink>,
            Arc::clone(&llm) as Arc<dyn roost_agent::Llm>,
            Arc::new(AgentClusterSignal(Arc::clone(&cluster.notifier))),
            config,
        );
        sink.bind(runtime.clone());
        Arc::new(Self {
            runtime,
            llm,
            sink,
            logins,
            http,
            endpoints,
            cluster,
            background: Mutex::new(Vec::new()),
        })
    }

    pub fn pool(&self) -> &Arc<AccountPool> {
        self.llm.pool()
    }

    /// Resumes durable unfinished work at boot; a storage failure retries
    /// without preventing the coordinator's worker listener from opening.
    pub async fn recover(&self) {
        match self.runtime.recover_interrupted_runs().await {
            Ok(recovered) => tracing::info!(recovered, "agent harness ready"),
            Err(error) => {
                tracing::error!(%error, "agent run recovery failed; retry scheduled");
                let runtime = self.runtime.clone();
                self.spawn_background(async move {
                    let mut delay = Duration::from_secs(2);
                    loop {
                        tokio::time::sleep(delay).await;
                        match runtime.recover_interrupted_runs().await {
                            Ok(recovered) => {
                                tracing::info!(recovered, "agent harness recovery completed");
                                break;
                            }
                            Err(error) => {
                                tracing::error!(%error, "agent run recovery failed; retry scheduled");
                                delay = (delay * 2).min(Duration::from_secs(60));
                            }
                        }
                    }
                });
            }
        }
    }

    /// Joins the replica set: listens for other replicas' notices, applies
    /// agent commands they admitted, and adopts runs whose owner vanished.
    pub fn join_cluster(&self) {
        let mut notices = self.cluster.notifier.subscribe();
        let notifier = Arc::clone(&self.cluster.notifier);
        let db = self.sink.database();
        self.spawn_background(chat_events::relay(
            notifier,
            db.clone(),
            self.sink.delivery(),
        ));
        self.spawn_background(async move {
            let mut prune = tokio::time::interval(Duration::from_secs(10 * 60));
            loop {
                prune.tick().await;
                match chat_events::prune(&db).await {
                    Ok(deleted) if deleted > 0 => {
                        tracing::info!(deleted, "expired agent chat event batches pruned")
                    }
                    Ok(_) => {}
                    Err(error) => tracing::warn!(%error, "agent chat event pruning failed"),
                }
            }
        });
        let runtime = self.runtime.clone();
        let sink = Arc::clone(&self.sink);
        self.spawn_background(async move {
            loop {
                match notices.recv().await {
                    Ok(notice) if notice.channel == AGENT_CHANNEL && notice.resync => {
                        runtime.resync_local_runs().await;
                    }
                    Ok(notice) if notice.channel == AGENT_CHANNEL => {
                        let runtime = runtime.clone();
                        tokio::spawn(async move {
                            runtime.apply_remote_change(&notice.payload).await;
                        });
                    }
                    Ok(notice) if notice.channel == AGENT_SUMMARY_CHANNEL => {
                        let runtime = runtime.clone();
                        let sink = Arc::clone(&sink);
                        let conversation_id = (!notice.resync).then_some(notice.payload);
                        tokio::spawn(async move {
                            refresh_summary_bus(&runtime, &sink, conversation_id.as_deref()).await;
                        });
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                        tracing::warn!(missed, "agent cluster notices lagged; resynchronizing");
                        runtime.resync_local_runs().await;
                        let runtime = runtime.clone();
                        let sink = Arc::clone(&sink);
                        tokio::spawn(async move {
                            refresh_summary_bus(&runtime, &sink, None).await;
                        });
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        let runtime = self.runtime.clone();
        self.spawn_background(async move {
            let mut sweep = tokio::time::interval(ORPHAN_SWEEP);
            loop {
                sweep.tick().await;
                if let Err(error) = runtime.recover_orphaned_runs().await {
                    tracing::warn!(%error, "orphaned agent run sweep failed");
                }
            }
        });
        tracing::info!(instance_id = %self.cluster.instance_id, "agent harness joined the coordinator cluster");
    }

    /// Stops this replica's background tasks and hands its runs to the other
    /// replicas: local runs stop without settling and their leases are freed.
    pub async fn leave_cluster(&self) {
        for task in self
            .background
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain(..)
        {
            task.abort();
        }
        let released = self.runtime.release_all_leases().await;
        tracing::info!(released, instance_id = %self.cluster.instance_id, "agent harness left the coordinator cluster");
    }

    fn spawn_background(&self, task: impl Future<Output = ()> + Send + 'static) {
        let handle = tokio::spawn(task).abort_handle();
        self.background
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(handle);
    }
}

async fn refresh_summary_bus(
    runtime: &AgentRuntime,
    sink: &CoordChatSink,
    conversation_id: Option<&str>,
) {
    let summaries = match conversation_id {
        Some(id) => runtime.conversation(id).await.map(|summary| vec![summary]),
        None => runtime.conversations().await,
    };
    match summaries {
        Ok(summaries) => {
            for summary in &summaries {
                sink.conversation(summary).await;
            }
        }
        Err(error) => {
            tracing::warn!(?conversation_id, %error, "conversation summaries could not be refreshed")
        }
    }
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
