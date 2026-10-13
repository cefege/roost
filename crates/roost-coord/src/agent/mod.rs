//! The coordinator's agent harness: the Rust runtime from roost-agent wired to
//! the coordinator's database, worker links, Sync buses and provider accounts.
//! RPC handlers live in `rpc_*`; `tool_calls` carries tool frames to workers.

pub mod credentials;
mod executor;
mod logins;
pub mod rpc_accounts;
pub mod rpc_auth;
pub mod rpc_chat;
mod rpc_errors;
pub mod rpc_turns;
mod sink;
pub mod store;
pub mod tool_calls;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use roost_agent::{AgentRuntime, RoostLlm, RuntimeConfig};
use roost_llm::{AccountPool, CredentialStore, Endpoints};

use crate::cluster::notify::{AGENT_CHANNEL, AgentClusterSignal, ClusterNotifier};
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
    ) -> Arc<Self> {
        let tools = Arc::new(executor::WorkerToolExecutor::new(tool_calls));
        Self::with_tools(db, buses, tools, endpoint_overrides, cluster)
    }

    /// The harness over any tool executor; tests answer tool calls directly.
    pub fn with_tools(
        db: CoordDb,
        buses: Arc<Buses>,
        tools: Arc<dyn roost_agent::ToolExecutor>,
        endpoint_overrides: BTreeMap<String, String>,
        cluster: ClusterMembership,
    ) -> Arc<Self> {
        Self::with_config(
            db,
            buses,
            tools,
            endpoint_overrides,
            cluster.clone(),
            RuntimeConfig {
                instance_id: cluster.instance_id,
                ..RuntimeConfig::default()
            },
        )
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
    ) -> Arc<Self> {
        let endpoints = if endpoint_overrides.is_empty() {
            Endpoints::production()
        } else {
            Endpoints::with_overrides(endpoint_overrides)
        };
        let http = reqwest::Client::new();
        let credentials = Arc::new(credentials::CoordCredentialStore::new(db.clone()))
            as Arc<dyn CredentialStore>;
        let pool = Arc::new(AccountPool::new(
            credentials,
            http.clone(),
            endpoints.clone(),
        ));
        let llm = Arc::new(RoostLlm::new(http.clone(), pool, endpoints.clone()));
        let sink = Arc::new(CoordChatSink::new(buses));
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
            logins: LoginRegistry::default(),
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
        let notifier = Arc::clone(&self.cluster.notifier);
        self.spawn_background(notifier.listen());
        let mut notices = self.cluster.notifier.subscribe();
        let runtime = self.runtime.clone();
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
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                        tracing::warn!(missed, "agent cluster notices lagged; resynchronizing");
                        runtime.resync_local_runs().await;
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
