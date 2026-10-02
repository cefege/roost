//! The document performance counters `perfProbe` and `resetPerfCounters`
//! answer from: the long-task window, the stall throttle, and the input round
//! trip ring. Pins `platform::browser::perf_counters` (v2
//! `apps/web/src/browser/leakWatch.ts`, `terminal-input-lanes.ts` RTT stamps).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_web::platform::browser::perf_counters::{INPUT_RTT_CAPACITY, PerfCounters};

#[test]
fn a_reset_opens_a_fresh_long_task_window_and_keeps_the_round_trips() {
    let mut counters = PerfCounters::default();
    counters.record_long_task(60.0, 0.0);
    counters.record_long_task(70.5, 1.0);
    counters.note_input_sent("s1", 100.0);
    counters.note_frame_painted("s1", 130.0);
    assert_eq!(counters.long_task_count, 2);
    assert_eq!(counters.long_task_ms, 130.5);
    counters.reset();
    assert_eq!(counters.long_task_count, 0);
    assert_eq!(counters.long_task_ms, 0.0);
    assert_eq!(counters.input_rtt_percentile(0.5), 30);
}

#[test]
fn a_stall_is_reported_once_per_throttle_window() {
    let mut counters = PerfCounters::default();
    assert!(
        !counters.record_long_task(199.0, 0.0),
        "below the stall bar"
    );
    assert!(counters.record_long_task(250.0, 1_000.0));
    assert!(
        !counters.record_long_task(400.0, 5_000.0),
        "inside the throttle"
    );
    assert!(counters.record_long_task(300.0, 11_000.0));
    assert_eq!(counters.long_task_count, 4, "every task still counts");
}

#[test]
fn a_send_stamp_becomes_one_round_trip_and_only_a_plausible_one() {
    let mut counters = PerfCounters::default();
    assert_eq!(counters.input_rtt_percentile(0.5), -1);
    counters.note_input_sent("s1", 1_000.0);
    counters.note_frame_painted("s1", 1_040.0);
    counters.note_frame_painted("s1", 1_900.0);
    assert_eq!(
        counters.input_rtt_percentile(0.95),
        40,
        "the stamp is consumed"
    );
    counters.note_input_sent("s2", 0.0);
    counters.note_frame_painted("s2", 9_000.0);
    counters.note_input_sent("s3", 10.0);
    counters.forget_session("s3");
    counters.note_frame_painted("s3", 20.0);
    assert_eq!(
        counters.input_rtt_percentile(0.95),
        40,
        "stale and forgotten stamps add none"
    );
}

#[test]
fn percentiles_rank_the_bounded_ring_like_v2() {
    let mut counters = PerfCounters::default();
    for sample in 1..=(INPUT_RTT_CAPACITY as u32 + 100) {
        counters.note_input_sent("s1", 0.0);
        counters.note_frame_painted("s1", f64::from(sample % 4_000 + 1));
    }
    // The ring keeps the newest 500: samples 101..=600 → values 102..=601.
    assert_eq!(counters.input_rtt_percentile(0.5), 352);
    assert_eq!(counters.input_rtt_percentile(0.95), 577);
}
