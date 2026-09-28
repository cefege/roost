//! Ports v2 `apps/worker/tests/transport/heartbeat.test.ts`: the completion-
//! scheduled loop (first attempt awaited, exact RPC deadline, no overlap, next
//! attempt timed from settlement), last-good metrics, per-instance stall
//! counting, the capacity report, and the keeper runtime proof's gating on the
//! reconciliation it belongs to. Time is tokio's paused clock (v2 fake timers).

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "heartbeat_support/mod.rs"]
mod heartbeat_support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use heartbeat_support::{
    CAPACITY, FakeSources, METRICS, ScriptedRpc, config, held_until, observation, settle,
};
use roost_observability::fields::{LogFields, RecordSink};
use roost_protocol::proto_adapters::{
    keeper_runtime_observation_from_proto, terminal_core_capacity_report_from_proto,
};
use roost_worker::runtime::heartbeat::{
    HEARTBEAT_INTERVAL, HEARTBEAT_RPC_TIMEOUT, KeeperReconciliation, start_heartbeat,
};

fn reconciled(at_ms: Option<i64>) -> KeeperReconciliation {
    let reconciliation = KeeperReconciliation::default();
    if let Some(at_ms) = at_ms {
        reconciliation.reconciled(at_ms);
    }
    reconciliation
}

#[tokio::test(start_paused = true)]
async fn awaits_the_first_attempt_and_applies_the_exact_rpc_deadline() {
    let (release, held) = held_until();
    let rpc = ScriptedRpc::new(move |_| held.take());
    let starting = tokio::spawn(start_heartbeat(config(
        Arc::clone(&rpc),
        FakeSources::steady(),
        reconciled(Some(100)),
    )));
    settle().await;
    assert!(!starting.is_finished(), "start returned before its first attempt settled");
    assert_eq!(rpc.calls(), 1);
    assert_eq!(rpc.timeouts(), vec![HEARTBEAT_RPC_TIMEOUT]);
    release.send(()).unwrap();
    let handle = starting.await.unwrap();
    handle.stop();
    tokio::time::advance(HEARTBEAT_INTERVAL * 4).await;
    settle().await;
    assert_eq!(rpc.calls(), 1);
}

#[tokio::test(start_paused = true)]
async fn never_overlaps_calls_and_schedules_the_next_attempt_from_settlement() {
    let (release, held) = held_until();
    let rpc = ScriptedRpc::new(move |attempt| if attempt == 2 { held.take() } else { Ok(()).into() });
    let handle = start_heartbeat(config(Arc::clone(&rpc), FakeSources::steady(), reconciled(Some(100)))).await;
    tokio::time::advance(HEARTBEAT_INTERVAL).await;
    settle().await;
    assert_eq!(rpc.calls(), 2);
    tokio::time::advance(HEARTBEAT_INTERVAL * 10).await;
    settle().await;
    assert_eq!(rpc.calls(), 2, "a second RPC started while the first was in flight");
    release.send(()).unwrap();
    settle().await;
    tokio::time::advance(HEARTBEAT_INTERVAL - Duration::from_millis(1)).await;
    settle().await;
    assert_eq!(rpc.calls(), 2);
    tokio::time::advance(Duration::from_millis(1)).await;
    settle().await;
    assert_eq!(rpc.calls(), 3);
    handle.stop();
    tokio::time::advance(HEARTBEAT_INTERVAL * 4).await;
    settle().await;
    assert_eq!(rpc.calls(), 3);
}

#[tokio::test(start_paused = true)]
async fn retains_the_last_good_metrics_when_collection_is_unknown() {
    let rpc = ScriptedRpc::new(|_| Ok(()).into());
    let sources = FakeSources::sampling(|sample| {
        if sample == 2 {
            Err("sample unavailable".to_string())
        } else {
            Ok(METRICS)
        }
    });
    let handle = start_heartbeat(config(Arc::clone(&rpc), sources, reconciled(Some(100)))).await;
    tokio::time::advance(HEARTBEAT_INTERVAL).await;
    settle().await;
    let requests = rpc.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1] == requests[0], "the failed sample changed what was sent");
    assert!(requests[0].host_metrics.is_set());
    assert_eq!(rpc.timeouts(), vec![HEARTBEAT_RPC_TIMEOUT; 2]);
    handle.stop();
}

/// Records every Tier-1 signal this binary emits. Only this test fails RPCs.
struct SignalCapture(Mutex<Vec<LogFields>>);

impl RecordSink for SignalCapture {
    fn emit(&self, record: &LogFields) {
        self.0.lock().unwrap().push(record.clone());
    }
}

#[tokio::test(start_paused = true)]
async fn counts_one_miss_per_settlement_resets_on_success_and_isolates_instances() {
    let signals = Arc::new(SignalCapture(Mutex::new(Vec::new())));
    roost_observability::set_signal_sink(Some(Arc::clone(&signals) as Arc<dyn RecordSink>));
    let rejected = || -> Result<(), String> { Err("deadline_exceeded".to_string()) };
    let first = ScriptedRpc::new(move |_| rejected().into());
    let second = ScriptedRpc::new(move |_| rejected().into());
    let stop_first = start_heartbeat(config(first, FakeSources::steady(), reconciled(Some(100)))).await;
    let stop_second = start_heartbeat(config(second, FakeSources::steady(), reconciled(Some(100)))).await;
    tokio::time::advance(HEARTBEAT_INTERVAL).await;
    settle().await;
    assert!(signals.0.lock().unwrap().is_empty(), "two misses in each of two loops stalled");
    stop_first.stop();
    stop_second.stop();

    let reset = ScriptedRpc::new(move |attempt| if attempt == 3 { Ok(()).into() } else { rejected().into() });
    let stop_reset = start_heartbeat(config(Arc::clone(&reset), FakeSources::steady(), reconciled(Some(100)))).await;
    for _ in 0..5 {
        tokio::time::advance(HEARTBEAT_INTERVAL).await;
        settle().await;
    }
    roost_observability::set_signal_sink(None);
    let fired = signals.0.lock().unwrap().clone();
    assert_eq!(fired.len(), 1, "the stall fires once, on the third consecutive miss");
    assert_eq!(fired[0].get("evt").and_then(|value| value.as_str()), Some("heartbeat.stalled"));
    assert_eq!(fired[0].get("misses").and_then(serde_json::Value::as_u64), Some(3));
    assert!(reset.timeouts().iter().all(|timeout| *timeout == HEARTBEAT_RPC_TIMEOUT));
    stop_reset.stop();
}

#[tokio::test(start_paused = true)]
async fn ships_the_current_worker_owned_capacity_snapshot() {
    let rpc = ScriptedRpc::new(|_| Ok(()).into());
    let reads = Arc::new(Mutex::new(0_u32));
    let counted = Arc::clone(&reads);
    let mut beat = config(Arc::clone(&rpc), FakeSources::steady(), reconciled(None));
    beat.read_terminal_core_capacity = Some(Arc::new(move || {
        *counted.lock().unwrap() += 1;
        CAPACITY
    }));
    let handle = start_heartbeat(beat).await;
    assert_eq!(*reads.lock().unwrap(), 1);
    let requests = rpc.requests();
    let shipped = requests[0].terminal_core_capacity.as_option().expect("capacity is sent");
    assert_eq!(terminal_core_capacity_report_from_proto(shipped).unwrap(), CAPACITY);
    handle.stop();
}

#[tokio::test(start_paused = true)]
async fn withholds_the_observation_until_boot_reconciliation_has_succeeded() {
    let rpc = ScriptedRpc::new(|_| Ok(()).into());
    let sources = FakeSources::observing(|_, _| Ok(Some(observation(1))));
    let handle = start_heartbeat(config(Arc::clone(&rpc), Arc::clone(&sources), reconciled(None))).await;
    assert!(sources.observed().is_empty(), "the keeper was probed before reconciliation");
    assert!(!rpc.requests()[0].keeper_runtime.is_set());
    handle.stop();
}

#[tokio::test(start_paused = true)]
async fn ships_the_proved_observation_stamped_with_the_reconciliation_it_belongs_to() {
    let rpc = ScriptedRpc::new(|_| Ok(()).into());
    let sources = FakeSources::observing(|at_ms, _| Ok(Some(observation(at_ms))));
    let handle = start_heartbeat(config(
        Arc::clone(&rpc),
        Arc::clone(&sources),
        reconciled(Some(1_700_000_000_777)),
    ))
    .await;
    assert_eq!(sources.observed(), vec![1_700_000_000_777]);
    let requests = rpc.requests();
    let shipped = requests[0].keeper_runtime.as_option().expect("the proof is sent");
    assert_eq!(
        keeper_runtime_observation_from_proto(shipped).unwrap(),
        observation(1_700_000_000_777)
    );
    handle.stop();
}

#[tokio::test(start_paused = true)]
async fn drops_an_observation_whose_reconciliation_was_superseded_mid_beat() {
    let rpc = ScriptedRpc::new(|_| Ok(()).into());
    let sources = FakeSources::observing(|at_ms, reconciliation| {
        reconciliation.started();
        Ok(Some(observation(at_ms)))
    });
    let reconciliation = reconciled(Some(1_700_000_000_111));
    sources.watch(reconciliation.clone());
    let handle = start_heartbeat(config(Arc::clone(&rpc), sources, reconciliation)).await;
    assert!(!rpc.requests()[0].keeper_runtime.is_set());
    handle.stop();
}

#[tokio::test(start_paused = true)]
async fn still_beats_when_the_keeper_probe_fails() {
    let rpc = ScriptedRpc::new(|_| Ok(()).into());
    let sources = FakeSources::observing(|_, _| Err("keeper unreachable".to_string()));
    let handle = start_heartbeat(config(
        Arc::clone(&rpc),
        sources,
        reconciled(Some(1_700_000_000_222)),
    ))
    .await;
    assert_eq!(rpc.calls(), 1);
    assert!(!rpc.requests()[0].keeper_runtime.is_set());
    handle.stop();
}
