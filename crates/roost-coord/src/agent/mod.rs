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
mod sink;
pub mod store;
pub mod tool_calls;

use std::collections::BTreeMap;
use std::sync::Arc;

use roost_agent::{AgentRuntime, RoostLlm, RuntimeConfig};
use roost_llm::{AccountPool, CredentialStore, Endpoints};

use crate::db::CoordDb;
use crate::events::bus_domains::Buses;

pub use logins::{LoginError, LoginRegistry};
pub use sink::CoordChatSink;
use tool_calls::ToolCallRegistry;

/// The harness and everything its RPCs need.
pub struct AgentService {
    pub runtime: AgentRuntime,
    pub llm: Arc<RoostLlm>,
    pub sink: Arc<CoordChatSink>,
    pub logins: LoginRegistry,
    pub http: reqwest::Client,
    pub endpoints: Endpoints,
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
    ) -> Arc<Self> {
        let tools = Arc::new(executor::WorkerToolExecutor::new(tool_calls));
        Self::with_tools(db, buses, tools, endpoint_overrides)
    }

    /// The harness over any tool executor; tests answer tool calls directly.
    pub fn with_tools(
        db: CoordDb,
        buses: Arc<Buses>,
        tools: Arc<dyn roost_agent::ToolExecutor>,
        endpoint_overrides: BTreeMap<String, String>,
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
        let runtime = AgentRuntime::new(
            Arc::new(store::CoordAgentStore::new(db)),
            tools,
            Arc::clone(&sink) as Arc<dyn roost_agent::ChatSink>,
            Arc::clone(&llm) as Arc<dyn roost_agent::Llm>,
            RuntimeConfig::default(),
        );
        sink.bind(runtime.clone());
        Arc::new(Self {
            runtime,
            llm,
            sink,
            logins: LoginRegistry::default(),
            http,
            endpoints,
        })
    }

    pub fn pool(&self) -> &Arc<AccountPool> {
        self.llm.pool()
    }

    /// Settles runs a restart interrupted. Called once at boot.
    pub async fn recover(&self) {
        match self.runtime.recover_interrupted_runs().await {
            Ok(recovered) => tracing::info!(recovered, "agent harness ready"),
            Err(error) => tracing::error!(%error, "agent run recovery failed"),
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
