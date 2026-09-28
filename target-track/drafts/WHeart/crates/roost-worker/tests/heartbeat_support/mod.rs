//! The heartbeat's collaborators as v2's test drove them: a scripted RPC that
//! records each request and its deadline, sources whose sampling and keeper
//! probe are closures, and v2's metric, capacity and observation fixtures.
//! Compiled into `tests/heartbeat.rs` only.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use roost_proto::WorkersHeartbeatRequest;
use roost_protocol::keeper_update::KeeperRuntimeObservationV1;
use roost_protocol::wire::{HostMetrics, TerminalCoreCapacityReport};
use roost_worker::runtime::heartbeat::{
    HeartbeatConfig, HeartbeatRpc, HeartbeatSources, KeeperReconciliation,
};
use roost_worker::uplink::OwnerFuture;
use serde_json::json;
use tokio::sync::oneshot;

pub const METRICS: HostMetrics = HostMetrics {
    cpu_pct: 12.5,
    mem_used_bytes: 100,
    mem_total_bytes: 200,
    disk_used_bytes: 300,
    disk_total_bytes: 400,
    net_rx_bps: 500,
    net_tx_bps: 600,
    sampled_at_ms: 700,
};

pub const CAPACITY: TerminalCoreCapacityReport = TerminalCoreCapacityReport {
    used: 2,
    pending: 1,
    capacity: 2,
    estimated_reserved_bytes: 120 * 1024 * 1024,
    effective_memory_ceiling_bytes: 2 * 1024 * 1024 * 1024,
    boot_rss_bytes: 256 * 1024 * 1024,
    overcommit_count: 1,
    refusal_count: 4,
};

/// v2's `OBSERVATION`, stamped with the reconciliation it belongs to.
pub fn observation(reconciled_at_ms: i64) -> KeeperRuntimeObservationV1 {
    KeeperRuntimeObservationV1::parse(&json!({
        "schema_version": 1,
        "running_contract": {
            "protocol_version": 3,
            "supported_features": ["history-records", "spawn-epoch"],
            "required_features": ["history-records"],
            "implementation_digest": "b".repeat(64),
            "platform": "linux",
            "arch": "x64",
            "build_sha": "c".repeat(40),
        },
        "keeper_pid": 4242,
        "keeper_epoch": "3f2504e0-4f89-41d3-9a0c-0305e82c3301",
        "channel_count": 2,
        "binding_digest": "a".repeat(64),
        "reconciled_at_ms": reconciled_at_ms,
    }))
    .expect("v2's fixture is a valid observation")
}

/// Let every task the paused runtime can run make progress.
pub async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}

/// One RPC answer: settled now, or held until its release fires.
pub struct Answer(OwnerFuture<Result<(), String>>);

impl From<Result<(), String>> for Answer {
    fn from(result: Result<(), String>) -> Self {
        Self(Box::pin(std::future::ready(result)))
    }
}

/// An RPC answer held until the paired sender fires (v2 `withResolvers`).
pub struct Held(Mutex<Option<oneshot::Receiver<()>>>);

impl Held {
    pub fn take(&self) -> Answer {
        let held = self.0.lock().unwrap().take();
        Answer(Box::pin(async move {
            if let Some(held) = held {
                let _ = held.await;
            }
            Ok(())
        }))
    }
}

pub fn held_until() -> (oneshot::Sender<()>, Held) {
    let (release, held) = oneshot::channel();
    (release, Held(Mutex::new(Some(held))))
}

type Script = Box<dyn Fn(usize) -> Answer + Send + Sync>;

/// v2 `clientWith(vi.fn(workersHeartbeat))`.
pub struct ScriptedRpc {
    script: Script,
    seen: Mutex<Vec<(WorkersHeartbeatRequest, Duration)>>,
}

impl ScriptedRpc {
    pub fn new(script: impl Fn(usize) -> Answer + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            script: Box::new(script),
            seen: Mutex::new(Vec::new()),
        })
    }

    pub fn calls(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    pub fn timeouts(&self) -> Vec<Duration> {
        self.seen.lock().unwrap().iter().map(|(_, timeout)| *timeout).collect()
    }

    pub fn requests(&self) -> Vec<WorkersHeartbeatRequest> {
        self.seen.lock().unwrap().iter().map(|(request, _)| request.clone()).collect()
    }
}

impl HeartbeatRpc for ScriptedRpc {
    fn workers_heartbeat(
        &self,
        request: WorkersHeartbeatRequest,
        timeout: Duration,
    ) -> OwnerFuture<Result<(), String>> {
        let attempt = {
            let mut seen = self.seen.lock().unwrap();
            seen.push((request, timeout));
            seen.len()
        };
        (self.script)(attempt).0
    }
}

type Sample = Box<dyn Fn(usize) -> Result<HostMetrics, String> + Send + Sync>;
type Observe = Box<
    dyn Fn(i64, &KeeperReconciliation) -> Result<Option<KeeperRuntimeObservationV1>, String>
        + Send
        + Sync,
>;

/// v2 `sources(..)`: git sha `test-sha`, reachable `worker.test`.
pub struct FakeSources {
    sample: Sample,
    samples: Mutex<usize>,
    observe: Observe,
    observed: Mutex<Vec<i64>>,
    watched: Mutex<KeeperReconciliation>,
}

impl FakeSources {
    fn build(sample: Sample, observe: Observe) -> Arc<Self> {
        Arc::new(Self {
            sample,
            samples: Mutex::new(0),
            observe,
            observed: Mutex::new(Vec::new()),
            watched: Mutex::new(KeeperReconciliation::default()),
        })
    }

    pub fn steady() -> Arc<Self> {
        Self::build(Box::new(|_| Ok(METRICS)), Box::new(|_, _| Ok(None)))
    }

    pub fn sampling(
        sample: impl Fn(usize) -> Result<HostMetrics, String> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Self::build(Box::new(sample), Box::new(|_, _| Ok(None)))
    }

    pub fn observing(
        observe: impl Fn(i64, &KeeperReconciliation) -> Result<Option<KeeperRuntimeObservationV1>, String>
        + Send
        + Sync
        + 'static,
    ) -> Arc<Self> {
        Self::build(Box::new(|_| Ok(METRICS)), Box::new(observe))
    }

    /// The reconciliation the observe closure is handed (it may supersede it).
    pub fn watch(&self, reconciliation: KeeperReconciliation) {
        *self.watched.lock().unwrap() = reconciliation;
    }

    pub fn observed(&self) -> Vec<i64> {
        self.observed.lock().unwrap().clone()
    }
}

impl HeartbeatSources for FakeSources {
    fn collect_host_metrics(&self) -> OwnerFuture<Result<HostMetrics, String>> {
        let sample = {
            let mut samples = self.samples.lock().unwrap();
            *samples += 1;
            *samples
        };
        Box::pin(std::future::ready((self.sample)(sample)))
    }

    fn git_sha(&self) -> Option<String> {
        Some("test-sha".to_string())
    }

    fn observe_keeper_runtime(
        &self,
        reconciled_at_ms: i64,
    ) -> OwnerFuture<Result<Option<KeeperRuntimeObservationV1>, String>> {
        self.observed.lock().unwrap().push(reconciled_at_ms);
        let watched = self.watched.lock().unwrap().clone();
        Box::pin(std::future::ready((self.observe)(reconciled_at_ms, &watched)))
    }

    fn reachable_addr(&self) -> OwnerFuture<Option<String>> {
        Box::pin(std::future::ready(Some("worker.test".to_string())))
    }
}

/// One loop over `rpc` and `sources`, with no capacity reader.
pub fn config(
    rpc: Arc<ScriptedRpc>,
    sources: Arc<FakeSources>,
    reconciliation: KeeperReconciliation,
) -> HeartbeatConfig {
    HeartbeatConfig {
        rpc,
        reconciliation,
        read_terminal_core_capacity: None,
        sources,
        os: "linux",
        host_identity: None,
    }
}
