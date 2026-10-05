//! The document phase ring the oracle's navigation probe reads: oldest-first
//! order across eviction, the once-per-key rule, and the timeline's clock and
//! driver fields. Pins `platform::browser::phase_marks` (v2
//! `apps/web/src/browser/diag.ts` `markPhase`, `markPhaseOnce`, `phaseTimeline`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_web::platform::browser::phase_marks::{
    PHASE_MARK_CAPACITY, PhaseClock, PhaseName, PhaseRing,
};
use serde_json::{Value, json};

fn clock(monotonic_ms: f64) -> PhaseClock {
    PhaseClock {
        monotonic_ms,
        time_origin_epoch_ms: 1_000_000.0,
        navigation_start_epoch_ms: 1_000_010.0,
    }
}

fn indexes(timeline: &Value) -> Vec<u64> {
    timeline["marks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|mark| mark["index"].as_u64().unwrap())
        .collect()
}

#[test]
fn a_full_ring_reports_the_newest_marks_oldest_first_and_counts_the_dropped() {
    let mut ring = PhaseRing::default();
    let total = PHASE_MARK_CAPACITY as u64 + 3;
    for index in 0..total {
        ring.mark(PhaseName::ViewportAccept, &[], clock(index as f64));
    }
    let timeline = ring.timeline_json(clock(0.0), None);
    assert_eq!(timeline["dropped"], json!(3));
    assert_eq!(indexes(&timeline), (3..total).collect::<Vec<_>>());
}

#[test]
fn a_mark_is_stamped_against_the_time_origin_and_the_navigation_start() {
    let mut ring = PhaseRing::default();
    ring.mark(
        PhaseName::TerminalMount,
        &[("sessionId", json!("s1")), ("nested", json!({ "no": 1 }))],
        clock(40.0),
    );
    let timeline = ring.timeline_json(clock(50.0), None);
    let mark = &timeline["marks"][0];
    assert_eq!(mark["name"], json!("terminal_mount"));
    assert_eq!(mark["epochMs"], json!(1_000_040.0));
    assert_eq!(mark["sinceNavigationMs"], json!(30.0));
    // Detail is scalar-only, as v2's phase detail type.
    assert_eq!(mark["detail"], json!({ "sessionId": "s1" }));
    assert!(mark.get("onceKey").is_none());
}

#[test]
fn a_once_mark_stands_while_retained_and_is_recorded_again_after_eviction() {
    let mut ring = PhaseRing::default();
    let first = ring.mark_once(PhaseName::FirstCellReceive, "s1", &[], clock(1.0));
    let repeat = ring.mark_once(PhaseName::FirstCellReceive, "s1", &[], clock(2.0));
    let other = ring.mark_once(PhaseName::FirstCellReceive, "s2", &[], clock(3.0));
    assert_eq!(repeat, first);
    assert_ne!(other, first);
    assert!(ring.has_mark(PhaseName::FirstCellReceive, "s1"));
    assert!(!ring.has_mark(PhaseName::FirstCellApply, "s1"));
    assert_eq!(
        ring.timeline_json(clock(0.0), None)["marks"][0]["onceKey"],
        json!("s1")
    );
    for _ in 0..PHASE_MARK_CAPACITY {
        ring.mark(PhaseName::ViewportAccept, &[], clock(4.0));
    }
    assert!(!ring.has_mark(PhaseName::FirstCellReceive, "s1"));
    let again = ring.mark_once(PhaseName::FirstCellReceive, "s1", &[], clock(5.0));
    assert_ne!(again, first, "an evicted once mark no longer suppresses");
}

#[test]
fn the_timeline_reports_its_clocks_and_only_a_finite_driver_stamp() {
    let ring = PhaseRing::default();
    let timeline = ring.timeline_json(clock(0.0), Some(999_000.0));
    assert_eq!(timeline["capacity"], json!(PHASE_MARK_CAPACITY));
    assert_eq!(timeline["timeOriginEpochMs"], json!(1_000_000.0));
    assert_eq!(timeline["navigationStartEpochMs"], json!(1_000_010.0));
    assert_eq!(timeline["driverBeforeNavigationEpochMs"], json!(999_000.0));
    let unstamped = ring.timeline_json(clock(0.0), Some(f64::NAN));
    assert_eq!(unstamped["driverBeforeNavigationEpochMs"], Value::Null);
    assert_eq!(
        ring.timeline_json(clock(0.0), None)["driverBeforeNavigationEpochMs"],
        Value::Null
    );
}
