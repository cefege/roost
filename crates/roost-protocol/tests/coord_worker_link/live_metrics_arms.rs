//! Fixture vectors for the live-metrics arms: one host snapshot from the worker
//! and the subscription toggle from the coordinator. The shared codec fixture
//! list calls these to round-trip both directions.

use roost_protocol::wire::coord_worker::{CoordWorkerDownstream, CoordWorkerUpstream};
use roost_protocol::wire::live_metrics::{LiveHostSnapshot, LiveProcess};

pub fn upstream_arms() -> Vec<(&'static str, CoordWorkerUpstream)> {
    vec![(
        "live-metrics",
        CoordWorkerUpstream::LiveMetrics(live_snapshot()),
    )]
}

pub fn downstream_arms() -> Vec<(&'static str, CoordWorkerDownstream)> {
    vec![(
        "live-metrics-subscription",
        CoordWorkerDownstream::LiveMetricsSubscription(roost_proto::DLiveMetricsSubscription {
            active: true,
            ..Default::default()
        }),
    )]
}

fn live_snapshot() -> LiveHostSnapshot {
    LiveHostSnapshot {
        cpu_pct: 42.5,
        mem_used_bytes: 8_000_000_000,
        mem_total_bytes: 16_000_000_000,
        disk_used_bytes: 120_000_000_000,
        disk_total_bytes: 500_000_000_000,
        cpu_count: 8,
        uptime_secs: 86_400,
        process_count: 2,
        processes: vec![
            LiveProcess {
                pid: 1,
                ppid: 0,
                name: "launchd".to_owned(),
                cpu_pct: 0.5,
                rss_bytes: 10_000_000,
            },
            LiveProcess {
                pid: 412,
                ppid: 1,
                name: "roost-worker".to_owned(),
                cpu_pct: 3.25,
                rss_bytes: 64_000_000,
            },
        ],
        sampled_at_ms: 1_700_000_000_000,
    }
}
