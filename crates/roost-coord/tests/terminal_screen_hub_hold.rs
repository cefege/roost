//! Deltas that arrive while a chunked baseline assembles: held in a bounded
//! hold and folded after it as if the run had never been interrupted, dropped
//! to the one-resync latch when the hold overflows, superseded by an ordinary
//! full, and cleared by a new stream.
//!
//! Ports the "delta hold during assembly" cases of
//! `apps/coord/tests/terminal/screen/terminal-screen-hub-chunks.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_screen_hub_support;

use roost_coord::terminal_screen::hub_state::TERMINAL_SCREEN_ASSEMBLY_HOLD_MAX_FRAMES;
use terminal_screen_hub_support::{
    Harness, OTHER_STREAM, SNAPSHOT_A, SNAPSHOT_B, STREAM, TestSink, baseline, delta, delta_frame,
    full_frame, harness, request, row_chunks, session, texts, watch,
};

/// What a fresh socket is seeded with, as the replica's observable content.
fn served_texts(h: &Harness, socket_id: &str) -> Vec<String> {
    let probe = TestSink::queuing();
    watch(&h.hub, &probe, socket_id);
    assert!(h.hub.seed_socket(socket_id, &session()));
    texts(&probe.last_seeded())
}

// v2 "holds live deltas during chunk assembly and folds them like an
// uninterrupted run".
#[test]
fn deltas_held_during_assembly_fold_as_if_the_run_was_never_interrupted() {
    let held = harness();
    let sink = TestSink::queuing();
    watch(&held.hub, &sink, "socket-a");
    held.hub.expect_stream(&session(), STREAM, 8, 2);
    held.frame(baseline(1, &["old-0", "old-1"]));

    let partial = row_chunks(&baseline(3, &["mid-0", "mid-1"]), SNAPSHOT_A);
    held.chunk(&partial[0], 0);
    held.frame(delta(1, "stale"));
    held.frame(delta(3, "live"));
    assert!(held.requests().is_empty());
    assert_eq!(
        held.replica(),
        Some((1, true)),
        "the old baseline serves until the new one lands"
    );

    held.chunk(&partial[1], 0);
    assert!(held.requests().is_empty());
    assert_eq!(texts(&sink.last_seeded()), ["mid-0", "mid-1"]);
    assert_eq!(
        sink.delta_texts(),
        [["live"]],
        "the pre-baseline delta is skipped, not folded"
    );
    assert_eq!(held.replica(), Some((4, true)));

    let direct = harness();
    direct.hub.expect_stream(&session(), STREAM, 8, 2);
    direct.frame(baseline(1, &["old-0", "old-1"]));
    direct.frame(baseline(3, &["mid-0", "mid-1"]));
    direct.frame(delta(3, "live"));
    assert_eq!(held.replica(), direct.replica());
    assert_eq!(served_texts(&held, "probe"), served_texts(&direct, "probe"));
}

// v2 "falls back to the resync latch when the delta hold overflows".
#[test]
fn a_hold_that_overflows_falls_back_to_the_single_resync_latch() {
    let h = harness();
    h.hub.expect_stream(&session(), STREAM, 8, 2);
    let partial = row_chunks(&baseline(2, &[]), SNAPSHOT_A);
    h.chunk(&partial[0], 0);
    for _ in 0..TERMINAL_SCREEN_ASSEMBLY_HOLD_MAX_FRAMES {
        h.frame(delta(1, "changed"));
    }
    assert!(
        h.requests().is_empty(),
        "a full hold is still within bounds"
    );

    h.frame(delta(1, "changed"));
    assert_eq!(h.requests(), [request(STREAM)]);
    assert_eq!(h.replica(), None);

    h.chunk(&partial[1], 0);
    assert_eq!(
        h.requests(),
        [request(STREAM)],
        "the abandoned transfer asks for nothing more"
    );
    assert_eq!(h.replica(), None);
}

// v2 "an ordinary full mid-assembly still supersedes the partial".
#[test]
fn an_ordinary_full_mid_assembly_supersedes_the_partial() {
    let h = harness();
    let sink = TestSink::queuing();
    watch(&h.hub, &sink, "socket-a");
    h.hub.expect_stream(&session(), STREAM, 8, 2);
    h.frame(baseline(1, &["old-0", "old-1"]));
    let partial = row_chunks(&baseline(3, &["part-0", "part-1"]), SNAPSHOT_A);
    h.chunk(&partial[0], 0);
    h.frame(delta(1, "held"));

    h.frame(baseline(5, &["fresh-0", "fresh-1"]));
    assert!(h.requests().is_empty());
    assert_eq!(h.replica(), Some((5, true)));
    assert_eq!(texts(&sink.last_seeded()), ["fresh-0", "fresh-1"]);

    h.frame(delta(5, "after"));
    assert_eq!(h.replica(), Some((6, true)));
    assert_eq!(
        sink.delta_texts(),
        [["after"]],
        "the held delta died with the partial"
    );
    assert!(h.requests().is_empty());
}

// v2 "minting a stream clears the delta hold".
#[test]
fn a_new_stream_clears_the_hold() {
    let h = harness();
    h.hub.expect_stream(&session(), STREAM, 8, 2);
    h.chunk(&row_chunks(&baseline(3, &[]), SNAPSHOT_A)[0], 0);
    for _ in 0..TERMINAL_SCREEN_ASSEMBLY_HOLD_MAX_FRAMES {
        h.frame(delta(1, "changed"));
    }
    assert!(h.requests().is_empty());

    h.hub.expect_stream(&session(), OTHER_STREAM, 8, 2);
    let assembled = row_chunks(&full_frame(OTHER_STREAM, 9, 8, 2, &[]), SNAPSHOT_B);
    h.chunk(&assembled[0], 0);
    h.frame(delta_frame(OTHER_STREAM, 9, 1, "changed"));
    assert!(
        h.requests().is_empty(),
        "the old stream's full hold did not carry over"
    );

    h.chunk(&assembled[1], 0);
    assert!(h.requests().is_empty());
    assert_eq!(h.replica(), Some((10, true)));
}
