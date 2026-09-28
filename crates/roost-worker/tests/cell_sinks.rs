//! Ports `apps/worker/tests/session/session-cell-sinks.test.ts`: one built
//! frame, many transports. Each case asserts on the frames a sink was handed,
//! because "what reached this browser" is the only contract a renderer folds.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "cell_support/mod.rs"]
mod support;

use std::sync::atomic::Ordering;

use roost_worker::session::cell_sink::COORD_CELL_SINK_ID;
use support::{Harness, ScriptedSink, row_text};

fn with_local(harness: &mut Harness) -> std::sync::Arc<ScriptedSink> {
    let local = ScriptedSink::new("local:socket-1");
    harness.emitter.register_cell_sink(local.clone());
    local
}

#[test]
fn keeps_delivering_to_a_local_sink_while_the_coordinator_is_suspended() {
    let mut harness = Harness::new(11);
    let local = with_local(&mut harness);
    harness.enable_stream(1, 0);
    assert_eq!(local.fulls(), vec![true]);

    harness.emitter.suspend_cell_sink(COORD_CELL_SINK_ID);
    harness.write(b"\x1b[2;1HLOCAL-ONLY", 10);
    harness.emit(false, 10);
    assert_eq!(local.fulls(), vec![true, false]);
    assert!(row_text(&local.attempts()[1], 1).contains("LOCAL-ONLY"));
    assert_eq!(
        harness.coord.fulls(),
        vec![true],
        "a suspended sink was handed a frame"
    );

    harness.write(b"\x1b[3;1HSTILL-LOCAL", 20);
    harness.emit(false, 20);
    assert_eq!(
        local.fulls(),
        vec![true, false, false],
        "a suspension forced a re-baseline"
    );
}

#[test]
fn repairs_with_exactly_one_forced_full_when_an_active_sink_drops_a_delta() {
    let mut harness = Harness::new(12);
    harness.enable_stream(1, 0);
    assert_eq!(harness.coord.fulls(), vec![true]);

    harness.coord.drop_next_delta.store(true, Ordering::SeqCst);
    harness.write(b"\x1b[2;1HDROPPED", 10);
    harness.emit(false, 10);
    assert_eq!(harness.coord.fulls(), vec![true, false, true]);
    assert!(row_text(&harness.coord.attempts()[2], 1).contains("DROPPED"));

    harness.write(b"\x1b[3;1HAFTER-REPAIR", 20);
    harness.emit(false, 20);
    assert_eq!(
        harness.coord.fulls(),
        vec![true, false, true, false],
        "the repair repeated"
    );
}

#[test]
fn unregisters_only_the_local_sink_that_overflows() {
    let mut harness = Harness::new(13);
    let local = with_local(&mut harness);
    harness.enable_stream(1, 0);

    local.overflow.store(true, Ordering::SeqCst);
    harness.write(b"\x1b[2;1HOVERFLOW", 10);
    harness.emit(false, 10);
    assert_eq!(
        local.overflow_notices(),
        1,
        "the overflowing socket was not told exactly once"
    );
    assert!(!harness.emitter.sinks().contains("local:socket-1"));
    let local_attempts = local.attempts().len();

    assert_eq!(harness.coord.fulls(), vec![true, false]);
    harness.write(b"\x1b[3;1HCOORD-LIVE", 20);
    harness.emit(false, 20);
    assert_eq!(
        harness.coord.fulls(),
        vec![true, false, false],
        "the coordinator was re-baselined"
    );
    assert!(row_text(&harness.coord.attempts()[2], 2).contains("COORD-LIVE"));
    assert_eq!(
        local.attempts().len(),
        local_attempts,
        "a dropped sink kept receiving"
    );
}

#[test]
fn repairs_stream_wide_when_one_sink_drops_a_delta_its_sibling_took() {
    let mut harness = Harness::new(14);
    let local = with_local(&mut harness);
    harness.enable_stream(1, 0);

    harness.coord.drop_next_delta.store(true, Ordering::SeqCst);
    harness.write(b"\x1b[2;1HSPLIT", 10);
    harness.emit(false, 10);
    assert_eq!(harness.coord.fulls(), vec![true, false, true]);
    assert_eq!(local.fulls(), vec![true, false, true]);
    assert_eq!(
        local.attempts()[2].seq,
        harness.coord.attempts()[2].seq,
        "two repair builds"
    );
    let seqs: Vec<u64> = local.attempts().iter().map(|frame| frame.seq).collect();
    assert_eq!(
        seqs,
        vec![1, 2, 3],
        "the accepted delta's seq was re-used by the repair"
    );

    harness.write(b"\x1b[3;1HAFTER", 20);
    harness.emit(false, 20);
    assert_eq!(local.fulls(), vec![true, false, true, false]);
    assert_eq!(harness.coord.fulls(), vec![true, false, true, false]);
}

#[test]
fn a_sink_registering_mid_snapshot_does_not_darken_the_channel() {
    let mut harness = Harness::new(15);
    harness.write(b"BASE", 0);
    harness.coord.refuse_all.store(true, Ordering::SeqCst);
    harness.enable_stream(1, 1);
    assert_eq!(
        harness.coord.attempts().len(),
        1,
        "the coordinator's baseline is parked"
    );

    let local = with_local(&mut harness);
    harness.run(2);
    assert_eq!(
        local.fulls(),
        vec![true],
        "the new sink was left without a baseline"
    );

    harness.coord.refuse_all.store(false, Ordering::SeqCst);
    harness.emitter.resume_terminal_snapshots();
    harness.write(b"\x1b[2;1HAFTER", 10);
    harness.emit(false, 10);
    assert_eq!(local.fulls(), vec![true, false]);
    assert!(row_text(&local.attempts()[1], 1).contains("AFTER"));
    let last = harness.coord.attempts().last().cloned().unwrap();
    assert!(!last.full);
    assert_eq!(last.stream_id, support::stream_id(1));
}

#[test]
fn builds_one_frame_per_tick_and_fans_that_same_frame_to_every_sink() {
    let mut harness = Harness::new(16);
    let local = with_local(&mut harness);
    harness.enable_stream(1, 0);

    harness.write(b"\x1b[2;1HONE-BUILDER", 10);
    harness.emit(false, 10);
    let coord_seqs: Vec<u64> = harness
        .coord
        .attempts()
        .iter()
        .map(|frame| frame.seq)
        .collect();
    let local_seqs: Vec<u64> = local.attempts().iter().map(|frame| frame.seq).collect();
    assert_eq!(coord_seqs, local_seqs);
    assert_eq!(*coord_seqs.last().unwrap(), 2);
    let local_last = local.attempts().last().cloned().unwrap();
    let coord_last = harness.coord.attempts().last().cloned().unwrap();
    assert_eq!(row_text(&local_last, 1), row_text(&coord_last, 1));
    assert!(row_text(&local_last, 1).contains("ONE-BUILDER"));

    // An already-active sink resumes without minting a frame.
    harness.emitter.resume_cell_sink(COORD_CELL_SINK_ID);
    harness.run(20);
    assert_eq!(harness.coord.attempts().len(), 2);
}
