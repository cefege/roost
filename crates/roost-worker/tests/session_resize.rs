//! What a resize guarantees: it is taken to the keeper and settled at the
//! boundary, the existing core is resized in place, the bytes produced while the
//! boundary was unresolved are replayed behind it, and the history floor moves
//! only as far as the replay bound did. Mirrors `session/resize.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod session_support;

use roost_worker::session::resize::{PinInputs, ResizeOutcome, pin_for};

use session_support::{Harness, SESSION, channel, session_id};

/// A RESIZE WHOSE REPLAY BOUND PASSES THE RETAINED WINDOW MOVES THE FLOOR. The
/// rows are gone to the bound and not to eviction, which is the distinction a
/// page read makes when it answers "resize_replay" rather than "evicted".
#[test]
fn a_resize_whose_bound_passes_the_window_moves_the_floor_and_names_the_loss() {
    let pin = pin_for(PinInputs {
        at_mono_ms: 5_000,
        cols: 100,
        rows: 40,
        replayed_ring: true,
        ring_evicted: false,
        prev_dropped: 10,
        prev_total: 100,
        fresh_discarded: 20,
        fresh_count: 5,
        previous_replay_floor: 0,
    });
    assert_eq!(
        pin.sb_dropped, 75,
        "the floor rose by what the rebuild dropped"
    );
    assert_eq!(
        pin.replay_lost_rows, 65,
        "the rows under the old numbering that the fresh core does not have"
    );
    assert_eq!(
        pin.replay_floor, 75,
        "the watermark is what a page read compares its window against"
    );
    assert!(!pin.clamped);
}

/// A REBUILD THAT COMES BACK WITH MORE THAN IT HAD cannot be made continuous,
/// and the pin says so rather than reporting a floor that went backwards.
#[test]
fn a_rebuild_that_recovered_more_than_it_had_is_clamped_at_zero() {
    let pin = pin_for(PinInputs {
        at_mono_ms: 5_000,
        cols: 80,
        rows: 24,
        replayed_ring: true,
        ring_evicted: true,
        prev_dropped: 0,
        prev_total: 10,
        fresh_discarded: 0,
        fresh_count: 40,
        previous_replay_floor: 0,
    });
    assert!(pin.clamped);
    assert_eq!(pin.sb_dropped, 0, "the floor cannot go below zero");
    assert_eq!(pin.replay_lost_rows, 0);
    assert_eq!(
        pin.sb_origin, 0,
        "nothing was dropped, so nothing is offset"
    );
}

/// A RESIZE IS TAKEN TO THE KEEPER AND SETTLED AT ITS BOUNDARY, and the bytes
/// the capture held are parsed behind the new geometry rather than dropped.
#[test]
fn a_resize_is_applied_in_place_after_the_capture_closes() {
    let harness = Harness::new();
    harness.install(SESSION, 7, "/home/user/project", "/home/user/project");
    harness
        .delivery
        .capture
        .lock()
        .expect("held")
        .extend_from_slice(b"after");
    let outcome = harness
        .manager
        .resize_channel(channel(7), 100, 40)
        .expect("a held session");
    assert_eq!(
        outcome,
        ResizeOutcome::Applied {
            cols: 100,
            rows: 40
        }
    );
    assert_eq!(
        harness.keeper.resized.lock().expect("held").clone(),
        vec![(7, 1, 100, 40)],
        "the keeper was asked once, with this worker's first sequence for the channel"
    );
    assert_eq!(
        harness.delivery.frozen.lock().expect("held").clone(),
        vec![channel(7)],
        "the capture opened before the write, not after"
    );
    let geometry = harness
        .table
        .with_record(&session_id(SESSION), |record| {
            (record.terminal_core.cols(), record.terminal_core.rows())
        })
        .expect("live");
    assert_eq!(
        geometry,
        (100, 40),
        "the existing core was resized in place"
    );
}

/// A RESIZE THAT PROVES NOTHING CHANGES NOTHING. The capture is released and
/// the session keeps the geometry it had, because a resize that cannot be
/// proven is a resize that did not happen.
#[test]
fn a_resize_to_the_geometry_it_already_has_writes_nothing_to_the_keeper() {
    let harness = Harness::new();
    harness.install(SESSION, 7, "/home/user/project", "/home/user/project");
    assert_eq!(
        harness
            .manager
            .resize_channel(channel(7), 80, 24)
            .expect("held"),
        ResizeOutcome::Unchanged
    );
    assert!(
        harness.keeper.resized.lock().expect("held").is_empty(),
        "an unchanged geometry spends no sequence and moves no floor"
    );
}
