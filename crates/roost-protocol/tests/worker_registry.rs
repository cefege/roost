//! The fleet-side worker rows: the capacity report a heartbeat carries and the
//! host metrics a dashboard renders.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::json;

use roost_protocol::wire::worker::{HostMetrics, TerminalCoreCapacityReport, WorkerPresenceEvent};

fn capacity() -> serde_json::Value {
    json!({
        "used": 3,
        "pending": 1,
        "capacity": 4,
        "estimated_reserved_bytes": 1,
        "effective_memory_ceiling_bytes": 2,
        "boot_rss_bytes": 3,
        "overcommit_count": 0,
        "refusal_count": 0,
    })
}

fn metrics(cpu_pct: f64) -> HostMetrics {
    HostMetrics {
        cpu_pct,
        mem_used_bytes: 1,
        mem_total_bytes: 2,
        disk_used_bytes: 3,
        disk_total_bytes: 4,
        net_rx_bps: 5,
        net_tx_bps: 6,
        sampled_at_ms: 7,
    }
}

#[test]
fn a_capacity_report_within_its_advertised_capacity_is_accepted() {
    assert!(TerminalCoreCapacityReport::parse(capacity()).is_ok());
}

#[test]
fn a_replacement_reserve_covers_at_most_one_core() {
    let mut over_reserved = capacity();
    over_reserved["overcommit_count"] = json!(2);
    assert_eq!(
        TerminalCoreCapacityReport::parse(over_reserved)
            .unwrap_err()
            .field,
        "terminal_core_capacity.overcommit_count"
    );

    let mut one_reserved = capacity();
    one_reserved["overcommit_count"] = json!(1);
    // The fourth pending core is exactly the reserved slot.
    assert!(TerminalCoreCapacityReport::parse(one_reserved).is_ok());
}

#[test]
fn use_cannot_exceed_capacity_without_a_replacement_reserve() {
    let mut impossible = capacity();
    impossible["used"] = json!(9);
    impossible["pending"] = json!(0);
    assert_eq!(
        TerminalCoreCapacityReport::parse(impossible)
            .unwrap_err()
            .field,
        "terminal_core_capacity.used"
    );
}

#[test]
fn a_heartbeat_carries_pressure_and_nothing_else() {
    let heartbeat = WorkerPresenceEvent::parse(json!({
        "kind": "heartbeat",
        "fp": "a".repeat(64),
        "last_seen_ms": 900,
        "host_metrics": {
            "cpu_pct": 12.5,
            "mem_used_bytes": 1, "mem_total_bytes": 2,
            "disk_used_bytes": 3, "disk_total_bytes": 4,
            "net_rx_bps": 5, "net_tx_bps": 6, "sampled_at_ms": 7,
        },
        "terminal_core_capacity": capacity(),
    }))
    .expect("a valid heartbeat");
    let WorkerPresenceEvent::Heartbeat { host_metrics, .. } = &heartbeat else {
        panic!("a heartbeat is the heartbeat variant");
    };
    assert_eq!(host_metrics.as_ref().map(|row| row.cpu_pct), Some(12.5));
}

#[test]
fn an_impossible_cpu_share_is_refused() {
    assert!(metrics(150.0).check().is_err());
    assert!(metrics(-1.0).check().is_err());
    assert!(metrics(100.0).check().is_ok());
}

#[test]
fn a_negative_counter_is_refused() {
    let mut row = metrics(1.0);
    row.net_rx_bps = -1;
    assert_eq!(row.check().unwrap_err().field, "host_metrics.net_rx_bps");
    row.net_rx_bps = 0;
    row.sampled_at_ms = 0;
    assert_eq!(row.check().unwrap_err().field, "host_metrics.sampled_at_ms");
}
