//! The replica's decoded-frame counters the smoke backdoor reads as
//! `cellFrameCount`, `cellFullFrameCount`, `lastFullFrameSbRows` and
//! `cellGridEpoch`: counted after the generation and stream fences and a
//! successful decode, before the fold judges the frame (v2
//! `apps/web/src/store/terminal-stream-replica.ts` `noteWireFrame`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use support::{EPOCH, NEXT_EPOCH, OTHER_STREAM, bound_replica, delta, full, row, sync_token};

#[test]
fn a_full_then_a_delta_count_two_frames_one_full_and_the_fulls_history_rows() {
    let mut replica = bound_replica(4);
    let token = sync_token(1, 1);
    let mut baseline = full(4);
    baseline.scrollback_rows = vec![row(0, "h0"), row(1, "h1"), row(2, "h2")];
    baseline.scrollback_total = 3;
    let _ = replica.admit_frame(&baseline, false, &token, 0);
    let _ = replica.admit_frame(&delta(1, 4, 2), false, &token, 0);

    let counts = &replica.frame_counts;
    assert_eq!(counts.frames(), 2);
    assert_eq!(counts.full_frames(), 1);
    assert_eq!(counts.last_full_scrollback_rows(), 3);
    assert_eq!(counts.grid_epoch(), EPOCH);
}

#[test]
fn before_any_full_the_history_row_count_is_the_minus_one_sentinel() {
    let replica = bound_replica(4);
    assert_eq!(replica.frame_counts.last_full_scrollback_rows(), -1);
    assert_eq!(replica.frame_counts.frames(), 0);
    assert_eq!(replica.frame_counts.grid_epoch(), "");
}

#[test]
fn a_delta_the_fold_refuses_is_still_counted_but_another_streams_frame_is_not() {
    let mut replica = bound_replica(4);
    let token = sync_token(1, 1);
    let mut orphan = delta(7, 4, 1);
    orphan.grid_epoch = NEXT_EPOCH.to_owned();
    let _ = replica.admit_frame(&orphan, false, &token, 0);
    assert_eq!(replica.frame_counts.frames(), 1);
    assert_eq!(replica.frame_counts.full_frames(), 0);
    assert_eq!(replica.frame_counts.grid_epoch(), NEXT_EPOCH);

    let mut foreign = full(4);
    foreign.stream_id = OTHER_STREAM.to_owned();
    let _ = replica.admit_frame(&foreign, false, &token, 0);
    assert_eq!(replica.frame_counts.frames(), 1);
}

#[test]
fn a_frame_from_a_retired_generation_is_not_counted() {
    let mut replica = bound_replica(4);
    let _ = replica.admit_frame(&full(4), false, &sync_token(2, 1), 0);
    assert_eq!(replica.frame_counts.frames(), 0);
}
