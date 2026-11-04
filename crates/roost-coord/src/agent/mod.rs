//! Agent lifecycle and archive boundary; worker-hosted OMP lives in `omp`.
//! Production owns durable chat delivery but no native runtime or provider clients.
//! Explicit native fixtures bind worker tools and run recovery/schedule tasks.

mod chat_delivery;
mod chat_events;
pub mod credentials;
mod executor;
mod job_ledger;
mod logins;
pub mod omp;
pub mod rpc_accounts;
pub mod rpc_auth;
pub mod rpc_chat;
mod rpc_chat_attachments;
pub mod rpc_chat_transcript;
mod rpc_errors;
pub mod rpc_inspect;
pub mod rpc_turns;
mod sink;
pub mod store;
pub mod tool_calls;
mod tool_calls_jobs;

use std::collections::BTreeMap;
use std::future::Future;
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
/// How often a replica looks for scheduled messages whose `due_ms` has passed.
const SCHEDULE_SWEEP: Duration = Duration::from_secs(5);

/// This replica's identity among coordinators sharing one database.
#[derive(Debug, Clone)]
pub struct ClusterMembership {
    pub instance_id: String,
    pub notifier: Arc<ClusterNotifier>,
}

/// Production keeps only durable chat delivery; native execution is test-only.
pub struct AgentService {
    pub native: Option<NativeAgent>,
    pub sink: Arc<CoordChatSink>,
    pub cluster: ClusterMembership,
    background: Mutex<Vec<tokio::task::AbortHandle>>,
}

/// Runtime and provider services used by explicit native fixtures.
pub struct NativeAgent {
    pub runtime: AgentRuntime,
    pub llm: Arc<RoostLlm>,
    pub logins: LoginRegistry,
}

impl std::fmt::Debug for NativeAgent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeAgent")
            .finish_non_exhaustive()
    }
}

impl AgentService {
    pub fn native_runtime(&self) -> Result<&AgentRuntime, connectrpc::ConnectError> {
        self.native_service().map(|native| &native.runtime)
    }

    pub fn native_service(&self) -> Result<&NativeAgent, connectrpc::ConnectError> {
        self.native.as_ref().ok_or_else(|| {
            connectrpc::ConnectError::new(
                connectrpc::ErrorCode::FailedPrecondition,
                "native agent is archived",
            )
        })
    }
    pub fn pool(&self) -> Result<&Arc<AccountPool>, connectrpc::ConnectError> {
        self.native_service().map(|native| native.llm.pool())
    }
}

impl std::fmt::Debug for AgentService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentService")
            .field("native_enabled", &self.native.is_some())
            .finish_non_exhaustive()
    }
}

impl AgentService {
    /// Production construction retains event delivery without creating an
    /// engine, provider client, login registry or worker callback binding.
    pub fn new(db: CoordDb, buses: Arc<Buses>, cluster: ClusterMembership) -> Arc<Self> {
        let sink = Arc::new(CoordChatSink::new(db, Arc::clone(&cluster.notifier), buses));
        tracing::info!(
            backend = "native",
            state = "archived",
            "native agent runtime disabled"
        );
        Arc::new(Self {
            native: None,
            sink,
            cluster,
            background: Mutex::new(Vec::new()),
        })
    }

    /// Explicit native harness construction for tests.
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
    /// Native fixture construction with the coordinator's worker tool router.
    pub fn with_worker_tools(
        db: CoordDb,
        buses: Arc<Buses>,
        tool_calls: Arc<ToolCallRegistry>,
        endpoint_overrides: BTreeMap<String, String>,
        cluster: ClusterMembership,
    ) -> Arc<Self> {
        let service = Self::with_tools(
            db,
            buses,
            Arc::new(executor::WorkerToolExecutor::new(Arc::clone(&tool_calls))),
            endpoint_overrides,
            cluster,
        );
        if let Ok(native) = service.native_runtime() {
            tool_calls.bind_runtime(native.clone());
        }
        service
    }

    /// Explicit native harness construction with runtime tunables.
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
            native: Some(NativeAgent {
                runtime,
                llm,
                logins,
            }),
            sink,
            cluster,
            background: Mutex::new(Vec::new()),
        })
    }

    /// Resumes durable unfinished native work only for explicit native fixtures.
    pub async fn recover(&self) {
        let Some(native) = &self.native else {
            return;
        };
        match native.runtime.recover_interrupted_runs().await {
            Ok(recovered) => tracing::info!(recovered, "agent harness ready"),
            Err(error) => {
                tracing::error!(%error, "agent run recovery failed; retry scheduled");
                let runtime = native.runtime.clone();
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

    pub fn join_cluster(&self) {
        let db = self.sink.database();
        self.spawn_background(chat_events::relay(
            Arc::clone(&self.cluster.notifier),
            db.clone(),
            self.sink.delivery(),
        ));
        self.spawn_background(async move {
            let mut prune = tokio::time::interval(Duration::from_secs(10 * 60));
            loop {
                prune.tick().await;
                if let Err(error) = chat_events::prune(&db).await {
                    tracing::warn!(%error, "agent chat event pruning failed");
                }
            }
        });
        if let Some(native) = &self.native {
            let mut notices = self.cluster.notifier.subscribe();
            let runtime = native.runtime.clone();
            let sink = Arc::clone(&self.sink);
            self.spawn_background(async move {
                loop {
                    match notices.recv().await {
                        Ok(notice) if notice.channel == AGENT_CHANNEL && notice.resync => {
                            runtime.resync_local_runs().await;
                        }
                        Ok(notice) if notice.channel == AGENT_CHANNEL => {
                            runtime.apply_remote_change(&notice.payload).await;
                        }
                        Ok(notice) if notice.channel == AGENT_SUMMARY_CHANNEL => {
                            let conversation_id = (!notice.resync).then_some(notice.payload);
                            refresh_summary_bus(&runtime, &sink, conversation_id.as_deref()).await;
                        }
                        Ok(_) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            runtime.resync_local_runs().await;
                            refresh_summary_bus(&runtime, &sink, None).await;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
            let runtime = native.runtime.clone();
            self.spawn_background(async move {
                let mut sweep = tokio::time::interval(ORPHAN_SWEEP);
                loop {
                    sweep.tick().await;
                    if let Err(error) = runtime.recover_orphaned_runs().await {
                        tracing::warn!(%error, "orphaned agent run sweep failed");
                    }
                }
            });
            let runtime = native.runtime.clone();
            self.spawn_background(async move {
                let mut sweep = tokio::time::interval(SCHEDULE_SWEEP);
                loop {
                    sweep.tick().await;
                    if let Err(error) = roost_agent::schedules::deliver_due(&runtime).await {
                        tracing::warn!(%error, "scheduled message sweep failed");
                    }
                }
            });
        }
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
        if let Some(native) = &self.native {
            let released = native.runtime.release_all_leases().await;
            tracing::info!(released, instance_id = %self.cluster.instance_id, "agent harness left the coordinator cluster");
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
