//! The fixed bounds pipeline evidence is shaped by: age buckets, the
//! sixteen-count histogram cap, keeper in-flight accounting, and lane depths
//! that count only queued writers. Ports the bucket and stage rules of
//! `apps/worker/src/terminal/terminal-pipeline-snapshot-bounds.ts` and
//! `sampleKeeperFacts`/lane reads of `terminal-pipeline-snapshot.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use roost_proto::{TerminalPipelineReason, TerminalPipelineStage};
use roost_worker::keeper_pool::PendingInputUsage;
use roost_worker::session::control_lanes::{ControlKind, LaneSnapshot};
use roost_worker::session::keeper_admission::AdmissionKind;
use roost_worker::terminal_pipeline::bounds::{
    StageInput, TERMINAL_PIPELINE_MAX_HISTOGRAM_BUCKETS, histogram_for_ages, histogram_index, stage,
};
use roost_worker::terminal_pipeline::{control_facts, keeper_facts_from_pending};

#[test]
fn ages_land_in_their_inclusive_upper_bound_and_everything_past_thirty_seconds_is_the_last() {
    let cases = [
        (0, 0),
        (1, 0),
        (2, 1),
        (3, 2),
        (1_000, 10),
        (1_001, 11),
        (30_000, 14),
        (30_001, 15),
        (u64::MAX, 15),
    ];
    for (age_ms, bucket) in cases {
        assert_eq!(histogram_index(age_ms), bucket, "age {age_ms}ms");
    }
    assert!(
        histogram_for_ages(&[]).is_empty(),
        "no ages is no histogram, not sixteen zeroes"
    );
    let buckets = histogram_for_ages(&[5, 5, 40_000]);
    assert_eq!(buckets.len(), TERMINAL_PIPELINE_MAX_HISTOGRAM_BUCKETS);
    assert_eq!(
        (buckets[3], buckets[15], buckets.iter().sum::<u64>()),
        (2, 1, 3)
    );
}

#[test]
fn a_stage_never_carries_more_than_sixteen_counts() {
    let snapshot = stage(
        TerminalPipelineStage::WorkerKeeper,
        TerminalPipelineReason::None,
        StageInput {
            histogram_buckets: (0..20).collect(),
            ..StageInput::default()
        },
    );
    assert_eq!(snapshot.histogram_buckets, (0..16).collect::<Vec<u64>>());
}

#[test]
fn keeper_facts_age_each_pending_input_and_the_ledger_only_raises_the_frame_count() {
    let now = Instant::now();
    let started = |ago_ms: u64| {
        now.checked_sub(Duration::from_millis(ago_ms))
            .expect("the clock is past boot")
    };
    let pending = PendingInputUsage {
        started: vec![started(3), started(40_000)],
        commands: 5,
        bytes: 77,
    };
    let facts = keeper_facts_from_pending(&pending, now);
    assert_eq!(
        (facts.input_frames, facts.input_bytes, facts.resize_frames),
        (5, 77, 0)
    );
    assert!(facts.oldest_age_ms >= 40_000);
    assert_eq!(facts.histogram_buckets.len(), 16);
    assert_eq!(
        (facts.histogram_buckets[2], facts.histogram_buckets[15]),
        (1, 1)
    );

    let fewer_commands = PendingInputUsage {
        started: vec![started(1), started(1)],
        commands: 1,
        bytes: 0,
    };
    assert_eq!(
        keeper_facts_from_pending(&fewer_commands, now).input_frames,
        2
    );
    let idle = keeper_facts_from_pending(
        &PendingInputUsage {
            started: Vec::new(),
            commands: 0,
            bytes: 0,
        },
        now,
    );
    assert!(idle.histogram_buckets.is_empty() && idle.oldest_age_ms == 0);
}

#[test]
fn lane_depths_count_only_writers_queued_behind_the_holder() {
    let held = LaneSnapshot {
        control_depth: 3,
        control_running: Some(ControlKind::TerminalStream),
        control_running_since_ms: 1_000,
        admission_depth: 1,
        admission_holder: Some(AdmissionKind::TerminalInput),
        admission_held_since_ms: 1_900,
    };
    let facts = control_facts(held, 2_000);
    assert_eq!((facts.control_depth, facts.admission_depth), (2, 0));
    assert_eq!(
        (facts.control_running_age_ms, facts.admission_held_age_ms),
        (Some(1_000), Some(100))
    );

    let idle = control_facts(LaneSnapshot::default(), 2_000);
    assert_eq!(
        (
            idle.control_depth,
            idle.control_running_age_ms,
            idle.admission_held_age_ms
        ),
        (0, None, None)
    );
}
