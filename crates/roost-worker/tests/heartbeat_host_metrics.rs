//! v2 `apps/worker/src/transport/heartbeat.ts` `collectHostMetrics`: one real
//! sample per minute (beats in between reuse it and leave the bandwidth
//! baseline alone), bandwidth as the rate between the last two real samples,
//! zero before a baseline exists and for a counter that went backwards.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

use roost_observability::clock::EventClock;
use roost_worker::host::{HostSample, NetCounters};
use roost_worker::runtime::heartbeat_metrics::{HOST_METRICS_INTERVAL_MS, HostMetricsCollector};

#[derive(Debug, Default)]
struct StepClock(AtomicI64);

impl EventClock for StepClock {
    fn now_epoch_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn mono_ns(&self) -> u64 {
        0
    }
}

/// A collector over scripted interface counters, and how often it sampled.
fn collector(
    counters: Vec<Option<NetCounters>>,
) -> (HostMetricsCollector, Arc<StepClock>, Arc<AtomicUsize>) {
    let clock = Arc::new(StepClock(AtomicI64::new(1_000_000)));
    let sampled = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&sampled);
    let mut script = counters.into_iter();
    let collector = HostMetricsCollector::new(
        Box::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            HostSample {
                cpu_pct: 7.0,
                net: script.next().flatten(),
                ..HostSample::default()
            }
        }),
        Box::new(|| None),
        Arc::clone(&clock) as Arc<dyn EventClock>,
    );
    (collector, clock, sampled)
}

fn net(rx_bytes: u64, tx_bytes: u64) -> Option<NetCounters> {
    Some(NetCounters { rx_bytes, tx_bytes })
}

#[test]
fn a_sample_is_reused_for_a_minute_and_then_replaced() {
    let (mut collector, clock, sampled) = collector(vec![net(0, 0), net(0, 0)]);
    let first = collector.collect();
    clock
        .0
        .fetch_add(HOST_METRICS_INTERVAL_MS - 1, Ordering::SeqCst);
    assert_eq!(
        collector.collect(),
        first,
        "a beat inside the minute re-sampled"
    );
    assert_eq!(sampled.load(Ordering::SeqCst), 1);
    clock.0.fetch_add(1, Ordering::SeqCst);
    let second = collector.collect();
    assert_eq!(sampled.load(Ordering::SeqCst), 2);
    assert_eq!(
        second.sampled_at_ms,
        first.sampled_at_ms + HOST_METRICS_INTERVAL_MS
    );
}

#[test]
fn bandwidth_is_the_rate_between_the_last_two_real_samples() {
    let (mut collector, clock, _) = collector(vec![net(1_000, 500), net(61_000, 30_500)]);
    let first = collector.collect();
    assert_eq!(
        (first.net_rx_bps, first.net_tx_bps),
        (0, 0),
        "no baseline yet"
    );
    clock.0.fetch_add(30_000, Ordering::SeqCst);
    collector.collect();
    clock.0.fetch_add(30_000, Ordering::SeqCst);
    let second = collector.collect();
    assert_eq!(
        (second.net_rx_bps, second.net_tx_bps),
        (1_000, 500),
        "the cached beat moved the baseline"
    );
}

#[test]
fn a_counter_that_went_backwards_reads_as_zero_for_that_direction() {
    let (mut collector, clock, _) = collector(vec![net(90_000, 1_000), net(30_000, 61_000)]);
    collector.collect();
    clock
        .0
        .fetch_add(HOST_METRICS_INTERVAL_MS, Ordering::SeqCst);
    let after_reset = collector.collect();
    assert_eq!(after_reset.net_rx_bps, 0);
    assert_eq!(after_reset.net_tx_bps, 1_000);
}
