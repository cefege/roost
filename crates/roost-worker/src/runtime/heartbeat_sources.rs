//! The production heartbeat sources and RPC: the cached host metrics, the
//! build's git sha, the keeper runtime proof over the pool's own connection,
//! the reachable address (`reachable_addr`), and the `WorkersHeartbeat`
//! Connect call under a fresh worker credential. Ports v2
//! `apps/worker/src/transport/heartbeat.ts` (`DEFAULT_HEARTBEAT_SOURCES`) and
//! the heartbeat half of `transport/coord-client.ts`.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use connectrpc::client::{CallOptions, HttpClient};
use roost_proto::{CoordinatorServiceClient, WorkersHeartbeatRequest};
use roost_protocol::keeper_update::KeeperRuntimeObservationV1;
use roost_protocol::wire::HostMetrics;

use super::bootstrap_redeem::boot_call::AUTHORIZATION;
use super::credential::CredentialSource;
use super::heartbeat::{HeartbeatRpc, HeartbeatSources};
use super::heartbeat_metrics::HostMetricsCollector;
use super::reachable_addr::ReachableAddr;
use crate::keeper_pool::KeeperPool;
use crate::uplink::OwnerFuture;

/// The sources a worker's heartbeat reads.
pub struct WorkerHeartbeatSources {
    metrics: Arc<Mutex<HostMetricsCollector>>,
    git_sha: Option<String>,
    reachable: Arc<Mutex<ReachableAddr>>,
    pool: Arc<KeeperPool>,
}

impl std::fmt::Debug for WorkerHeartbeatSources {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkerHeartbeatSources")
            .field("git_sha", &self.git_sha)
            .finish_non_exhaustive()
    }
}

impl WorkerHeartbeatSources {
    pub fn new(
        metrics: HostMetricsCollector,
        git_sha: Option<String>,
        reachable: ReachableAddr,
        pool: Arc<KeeperPool>,
    ) -> Self {
        Self {
            metrics: Arc::new(Mutex::new(metrics)),
            git_sha,
            reachable: Arc::new(Mutex::new(reachable)),
            pool,
        }
    }
}

impl HeartbeatSources for WorkerHeartbeatSources {
    fn collect_host_metrics(&self) -> OwnerFuture<Result<HostMetrics, String>> {
        let metrics = Arc::clone(&self.metrics);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                metrics
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .collect()
            })
            .await
            .map_err(|error| error.to_string())
        })
    }

    fn git_sha(&self) -> Option<String> {
        self.git_sha.clone()
    }

    /// v2 `observeKeeperRuntime`: an unreachable, unauthenticated or
    /// incomplete keeper is no proof (`None`), never an error.
    fn observe_keeper_runtime(
        &self,
        reconciled_at_ms: i64,
    ) -> OwnerFuture<Result<Option<KeeperRuntimeObservationV1>, String>> {
        let probe = self.pool.probe_runtime();
        Box::pin(async move {
            Ok(probe
                .await
                .ok()
                .and_then(|probe| probe.observation(reconciled_at_ms)))
        })
    }

    fn reachable_addr(&self) -> OwnerFuture<Option<String>> {
        let reachable = Arc::clone(&self.reachable);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                reachable
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .current()
            })
            .await
            .ok()
            .flatten()
        })
    }
}

/// The `WorkersHeartbeat` call over Connect (v2 `createCoordClient`).
pub struct CoordinatorHeartbeatRpc {
    client: Arc<CoordinatorServiceClient<HttpClient>>,
    credential: Arc<dyn CredentialSource>,
}

impl std::fmt::Debug for CoordinatorHeartbeatRpc {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CoordinatorHeartbeatRpc")
            .finish_non_exhaustive()
    }
}

impl CoordinatorHeartbeatRpc {
    pub fn new(
        client: CoordinatorServiceClient<HttpClient>,
        credential: Arc<dyn CredentialSource>,
    ) -> Self {
        Self {
            client: Arc::new(client),
            credential,
        }
    }
}

impl HeartbeatRpc for CoordinatorHeartbeatRpc {
    fn workers_heartbeat(
        &self,
        request: WorkersHeartbeatRequest,
        timeout: Duration,
    ) -> OwnerFuture<Result<(), String>> {
        let client = Arc::clone(&self.client);
        let options = authorized_options(self.credential.as_ref(), timeout);
        Box::pin(async move {
            client
                .workers_heartbeat_with_options(request, options)
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    }
}

/// v2's interceptor: a fresh credential per call; one that cannot be minted is
/// logged and the call goes out without it, for the coordinator to refuse.
fn authorized_options(credential: &dyn CredentialSource, timeout: Duration) -> CallOptions {
    let token = match credential.mint() {
        Ok(token) => token,
        Err(error) => {
            tracing::warn!(%error, "jwt mint failed");
            return CallOptions::default().with_timeout(timeout);
        }
    };
    match CallOptions::default()
        .with_timeout(timeout)
        .try_with_header(AUTHORIZATION, format!("Bearer {token}"))
    {
        Ok(options) => options,
        Err(error) => {
            tracing::warn!(%error, "the worker credential is not a header value");
            CallOptions::default().with_timeout(timeout)
        }
    }
}
