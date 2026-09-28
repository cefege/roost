//! THROWAWAY smoke (deleted after the run): the production heartbeat RPC and
//! the real Linux host sampler against a loopback Connect coordinator.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use connectrpc::client::{ClientConfig, HttpClient};
use connectrpc::{Response, Router, Server, handler::handler_fn};
use roost_host::{HostPlatform, ProcessEnv};
use roost_observability::clock::SystemClock;
use roost_proto::{
    COORDINATOR_SERVICE_SERVICE_NAME, CoordinatorServiceClient, WorkersHeartbeatRequest,
    WorkersHeartbeatResponse,
};
use roost_protocol::keeper_update::KeeperRuntimeObservationV1;
use roost_protocol::wire::HostMetrics;
use roost_worker::host::tailnet::{resolve_tailnet_dns_name, tailscale_binary_candidates};
use roost_worker::runtime::credential::{CredentialError, CredentialSource};
use roost_worker::runtime::heartbeat::{
    HeartbeatConfig, HeartbeatSources, KeeperReconciliation, spawn_heartbeat,
};
use roost_worker::runtime::heartbeat_metrics::HostMetricsCollector;
use roost_worker::runtime::heartbeat_sources::CoordinatorHeartbeatRpc;
use roost_worker::uplink::OwnerFuture;

struct Fixed;
impl CredentialSource for Fixed {
    fn mint(&self) -> Result<String, CredentialError> {
        Ok("smoke-token".to_string())
    }
}

struct Real(Arc<Mutex<HostMetricsCollector>>);
impl HeartbeatSources for Real {
    fn collect_host_metrics(&self) -> OwnerFuture<Result<HostMetrics, String>> {
        let collector = Arc::clone(&self.0);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || collector.lock().unwrap().collect())
                .await
                .map_err(|error| error.to_string())
        })
    }
    fn git_sha(&self) -> Option<String> {
        None
    }
    fn observe_keeper_runtime(
        &self,
        _: i64,
    ) -> OwnerFuture<Result<Option<KeeperRuntimeObservationV1>, String>> {
        Box::pin(std::future::ready(Ok(None)))
    }
    fn reachable_addr(&self) -> OwnerFuture<Option<String>> {
        let candidates = tailscale_binary_candidates(HostPlatform::Linux, &ProcessEnv::new());
        Box::pin(async move {
            let name = tokio::task::spawn_blocking(move || resolve_tailnet_dns_name(&candidates))
                .await
                .unwrap();
            (!name.is_empty()).then_some(name)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_real_beat_reaches_a_loopback_coordinator() {
    let seen: Arc<Mutex<Vec<(Option<String>, WorkersHeartbeatRequest)>>> = Arc::default();
    let record = Arc::clone(&seen);
    let router = Router::new().route(
        COORDINATOR_SERVICE_SERVICE_NAME,
        "WorkersHeartbeat",
        handler_fn(move |ctx, request: WorkersHeartbeatRequest| {
            let record = Arc::clone(&record);
            async move {
                let auth = ctx
                    .headers()
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                record.lock().unwrap().push((auth, request));
                Response::ok(WorkersHeartbeatResponse::default())
            }
        }),
    );
    let bound = Server::bind("127.0.0.1:0").await.unwrap();
    let address = bound.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = bound.serve(router).await;
    });
    let client = CoordinatorServiceClient::new(
        HttpClient::plaintext(),
        ClientConfig::new(format!("http://{address}").parse().unwrap()),
    );
    let collector = HostMetricsCollector::for_host(HostPlatform::Linux, Arc::new(SystemClock));
    let (handle, first) = spawn_heartbeat(HeartbeatConfig {
        rpc: Arc::new(CoordinatorHeartbeatRpc::new(client, Arc::new(Fixed))),
        reconciliation: KeeperReconciliation::default(),
        read_terminal_core_capacity: None,
        sources: Arc::new(Real(Arc::new(Mutex::new(collector)))),
        os: HostPlatform::Linux.as_str(),
        host_identity: roost_worker::host::identity::static_host_identity(),
    });
    tokio::time::timeout(Duration::from_secs(20), first).await.unwrap().unwrap();
    handle.stop();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    let (auth, request) = &seen[0];
    let metrics = request.host_metrics.as_option().unwrap();
    eprintln!(
        "SMOKE auth={auth:?} os={:?} reachable={:?} git_sha={:?} keeper_runtime={} metrics={metrics:?}",
        request.os,
        request.reachable_addr,
        request.git_sha,
        request.keeper_runtime.is_set(),
    );
    assert_eq!(auth.as_deref(), Some("Bearer smoke-token"));
    assert_eq!(request.os.as_deref(), Some("linux"));
    assert!(metrics.mem_total_bytes > 0 && metrics.disk_total_bytes > 0);
}
