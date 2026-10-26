//! The live host snapshot on the worker link: a valid sample survives the round
//! trip byte for byte, and a sample the rules reject is refused on both the
//! encode and the decode side, with the field named in the error.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_proto::CoordWorkerUp;
use roost_proto::buffa::Message;
use roost_proto::coord_worker_up::Frame;
use roost_protocol::proto_adapters::coord_worker_proto::{decode_upstream, encode_upstream};
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;
use roost_protocol::wire::live_metrics::{LIVE_PROCESS_LIST_MAX, LiveHostSnapshot, LiveProcess};

fn process(pid: u32, cpu_pct: f64) -> LiveProcess {
    LiveProcess {
        pid,
        ppid: 1,
        name: format!("proc-{pid}"),
        cpu_pct,
        rss_bytes: 1_000_000,
    }
}

fn snapshot() -> LiveHostSnapshot {
    LiveHostSnapshot {
        cpu_pct: 42.5,
        mem_used_bytes: 8_000_000_000,
        mem_total_bytes: 16_000_000_000,
        disk_used_bytes: 120_000_000_000,
        disk_total_bytes: 500_000_000_000,
        cpu_count: 8,
        uptime_secs: 86_400,
        process_count: 2,
        processes: vec![process(10, 0.5), process(11, 3.25)],
        sampled_at_ms: 1_700_000_000_000,
    }
}

fn refused_field(snapshot: LiveHostSnapshot) -> String {
    encode_upstream(&CoordWorkerUpstream::LiveMetrics(snapshot))
        .unwrap_err()
        .field
}

#[test]
fn live_snapshot_round_trips_to_the_same_bytes() {
    let frame = CoordWorkerUpstream::LiveMetrics(snapshot());
    let first = encode_upstream(&frame).unwrap();
    let decoded = decode_upstream(&first).unwrap();
    assert_eq!(decoded, frame);
    assert_eq!(encode_upstream(&decoded).unwrap(), first);
}

#[test]
fn process_cpu_above_one_hundred_is_a_per_core_share() {
    let mut sample = snapshot();
    sample.processes = vec![process(20, 399.5)];
    sample.process_count = 1;
    let frame = CoordWorkerUpstream::LiveMetrics(sample);
    let decoded = decode_upstream(&encode_upstream(&frame).unwrap()).unwrap();
    assert_eq!(decoded, frame);
}

#[test]
fn process_list_at_the_limit_is_accepted() {
    let mut sample = snapshot();
    sample.processes = (0..LIVE_PROCESS_LIST_MAX)
        .map(|index| process(u32::try_from(index).unwrap(), 1.0))
        .collect();
    sample.process_count = u32::try_from(LIVE_PROCESS_LIST_MAX).unwrap();
    assert!(encode_upstream(&CoordWorkerUpstream::LiveMetrics(sample)).is_ok());
}

#[test]
fn process_list_over_the_limit_is_refused() {
    let mut sample = snapshot();
    sample.processes = (0..=LIVE_PROCESS_LIST_MAX)
        .map(|index| process(u32::try_from(index).unwrap(), 1.0))
        .collect();
    sample.process_count = u32::try_from(LIVE_PROCESS_LIST_MAX + 1).unwrap();
    assert_eq!(refused_field(sample), "live_metrics.processes");
}

#[test]
fn host_cpu_outside_zero_to_one_hundred_is_refused() {
    let mut above = snapshot();
    above.cpu_pct = 100.5;
    assert_eq!(refused_field(above), "live_metrics.cpu_pct");

    let mut not_a_number = snapshot();
    not_a_number.cpu_pct = f64::NAN;
    assert_eq!(refused_field(not_a_number), "live_metrics.cpu_pct");
}

#[test]
fn negative_or_non_finite_process_cpu_is_refused() {
    for cpu_pct in [-1.0, f64::INFINITY] {
        let mut sample = snapshot();
        sample.processes[0].cpu_pct = cpu_pct;
        assert_eq!(refused_field(sample), "live_metrics.processes[0].cpu_pct");
    }
}

#[test]
fn process_count_below_the_listed_count_is_refused() {
    let mut sample = snapshot();
    sample.process_count = 1;
    assert_eq!(refused_field(sample), "live_metrics.process_count");
}

#[test]
fn zero_cpu_count_and_sample_time_are_refused() {
    let mut no_cpus = snapshot();
    no_cpus.cpu_count = 0;
    assert_eq!(refused_field(no_cpus), "live_metrics.cpu_count");

    let mut no_time = snapshot();
    no_time.sampled_at_ms = 0;
    assert_eq!(refused_field(no_time), "live_metrics.sampled_at_ms");
}

#[test]
fn live_metrics_frame_without_a_snapshot_is_refused() {
    let bytes = CoordWorkerUp {
        frame: Some(Frame::LiveMetrics(Box::default())),
        ..Default::default()
    }
    .encode_to_vec();
    let error = decode_upstream(&bytes).unwrap_err();
    assert_eq!(error.field, "live_metrics.snapshot");
}
