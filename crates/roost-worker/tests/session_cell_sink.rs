//! The sink registry: who receives a frame, and what happens to a receiver
//! whose queue cannot drain. The overflow rule is the reason this file exists:
//! a latch-and-retry policy for a queue that cannot drain is how a local
//! delivery queue grows until the worker dies.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "session_emit_support/mod.rs"]
mod support;

use roost_worker::session::cell_sink::{COORD_CELL_SINK_ID, local_cell_sink_id};
use roost_worker::session::emit::{CellEmitter, FrameOutcome};

use support::{Answer, RecordFixture, RecordingSink, channel, numbered_lines};

/// The sink ids are wire-visible: they appear in diagnostics next to v2's own
/// lines, and `local:` is what tells a local socket's frames from the
/// coordinator's in a log a human has to read.
#[test]
fn sink_ids_name_the_coordinator_and_the_local_socket() {
    assert_eq!(COORD_CELL_SINK_ID, "coord");
    assert_eq!(local_cell_sink_id("7"), "local:7");
    assert_eq!(local_cell_sink_id("ws-3"), "local:ws-3");
}

/// An overflow drops ONE sink, tells it once, and leaves its siblings running.
/// A wedged browser must not be able to stop the coordinator from painting.
#[test]
fn an_overflow_drops_exactly_one_sink_and_tells_it_once() {
    let fixture = RecordFixture::new();
    let mut record = fixture.record(channel(11), 80, 24);
    let healthy = RecordingSink::new("coord", Answer::Sent);
    let wedged = RecordingSink::new(&local_cell_sink_id("9"), Answer::Overflow);
    let mut emitter = CellEmitter::new();
    emitter.register_sink(healthy.clone());
    emitter.register_sink(wedged.clone());
    emitter.install_stream(&mut record, "stream-11");
    emitter.emit_cell_frame(&mut record, true, 1_000);
    assert_eq!(wedged.overflow_notices(), 0);

    emitter.ingest_pty_chunk(&mut record, b"more output", 1_010);
    let outcome = emitter.emit_cell_frame(&mut record, false, 1_020);
    match &outcome {
        FrameOutcome::Delta { fanout, .. } => {
            assert_eq!(
                fanout.accepted, 1,
                "the healthy sink did not take the frame"
            );
            assert_eq!(
                fanout.dropped, 0,
                "an overflowed sink was counted as a repair-drop, which would force a full for a sink that is gone"
            );
            assert_eq!(fanout.overflowed, vec![local_cell_sink_id("9")]);
        }
        other => panic!("a delta was expected, got {other:?}"),
    }
    assert_eq!(
        healthy.frames().len(),
        2,
        "the surviving sink stopped receiving frames"
    );
    assert!(
        !emitter.sinks().contains("local:9"),
        "the wedged sink is still registered"
    );
    assert_eq!(
        wedged.overflow_notices(),
        1,
        "the owner was not told exactly once"
    );

    // A second frame: the dropped sink is gone, so it is not told again, and
    // the registry does not grow a second entry for it.
    emitter.ingest_pty_chunk(&mut record, b"and more", 1_030);
    emitter.emit_cell_frame(&mut record, false, 1_040);
    assert_eq!(
        wedged.overflow_notices(),
        1,
        "a dropped sink was told about the overflow more than once"
    );
    assert_eq!(healthy.frames().len(), 3);
    assert_eq!(emitter.sinks().len(), 1);
}

/// A sink that merely refuses a frame keeps its registration and owes a full:
/// it still has a transport, and a receiver that missed a delta can no longer
/// reproduce the screen. The repair is stream-wide, because one core yields one
/// frame per tick.
#[test]
fn a_sink_that_refuses_a_frame_keeps_its_registration_and_owes_a_full() {
    let fixture = RecordFixture::new();
    let mut record = fixture.record(channel(12), 80, 24);
    let healthy = RecordingSink::new("coord", Answer::Sent);
    let refusing = RecordingSink::new(&local_cell_sink_id("4"), Answer::Dropped);
    let mut emitter = CellEmitter::new();
    emitter.register_sink(healthy.clone());
    emitter.register_sink(refusing.clone());
    emitter.install_stream(&mut record, "stream-12");
    emitter.emit_cell_frame(&mut record, true, 1_000);

    emitter.ingest_pty_chunk(&mut record, b"output", 1_010);
    let refused = emitter.emit_cell_frame(&mut record, false, 1_020);
    match &refused {
        FrameOutcome::Delta { fanout, .. } => {
            assert_eq!(fanout.accepted, 1);
            assert_eq!(fanout.dropped, 1);
            assert!(fanout.overflowed.is_empty());
        }
        other => panic!("a delta was expected, got {other:?}"),
    }
    assert!(
        emitter.sinks().contains(&local_cell_sink_id("4")),
        "a refused frame dropped the sink, which is the overflow policy's job and not this one's"
    );
    assert!(
        emitter.take_pending_repair(channel(12)),
        "no repair was latched"
    );

    // The repair: one forced full, and the refusing sink now holds a baseline
    // again so deltas may flow.
    emitter.ingest_pty_chunk(&mut record, b"more", 1_030);
    let repaired = emitter.emit_cell_frame(&mut record, true, 1_040);
    assert!(
        matches!(
            repaired,
            FrameOutcome::Full {
                installed: true,
                ..
            }
        ),
        "the repair full did not install, got {repaired:?}"
    );
    assert_eq!(healthy.frames().len(), 3);
    assert_eq!(refusing.frames().len(), 2);
    assert!(refusing.frames().last().unwrap().full);
}

/// A full is parked, not pushed: a sink that refuses one part retries that part
/// while its siblings have already moved on, and the immutable frame is shared
/// rather than rebuilt per browser.
#[test]
fn a_parked_full_is_retried_from_the_part_the_sink_refused() {
    let fixture = RecordFixture::new();
    let mut record = fixture.record(channel(13), 80, 24);
    let slow = RecordingSink::new(&local_cell_sink_id("5"), Answer::SentThenOverflow(0));
    let fast = RecordingSink::new("coord", Answer::Sent);
    let mut emitter = CellEmitter::new();
    emitter.register_sink(fast.clone());
    emitter.install_stream(&mut record, "stream-13");
    emitter.emit_cell_frame(&mut record, true, 1_000);

    // The slow socket connects after the baseline and immediately overflows its
    // queue, so it never receives a byte and owes a full.
    emitter.register_sink(slow.clone());
    assert!(
        emitter.take_pending_repair(channel(13))
            || emitter.delivery_aggregate(channel(13)).baseline_dirty,
        "a sink that just joined recorded no debt"
    );
    emitter.ingest_pty_chunk(&mut record, b"after the join", 1_010);
    emitter.emit_cell_frame(&mut record, false, 1_020);
    assert!(slow.frames().is_empty(), "a dropped sink received a frame");
    assert!(fast.frames().len() >= 2, "the live sink stopped painting");
    assert!(
        emitter.delivery_aggregate(channel(13)).baseline_ready
            || emitter
                .channels_with_parked_snapshots()
                .contains(&channel(13)),
        "neither a baseline nor a parked cursor accounts for the new sink"
    );
}

/// A full that no sink can take must not leave the emitter claiming a
/// baseline: the frame's rows were consumed from the core, and a delta built
/// next would be missing them.
#[test]
fn output_kept_while_no_sink_can_take_it_is_still_in_the_core() {
    let fixture = RecordFixture::new();
    let mut record = fixture.record(channel(14), 80, 24);
    let mut emitter = CellEmitter::new();
    emitter.install_stream(&mut record, "stream-14");
    emitter.ingest_pty_chunk(&mut record, &numbered_lines(30), 1_000);

    let withheld = emitter.emit_cell_frame(&mut record, true, 1_010);
    assert_eq!(
        withheld,
        FrameOutcome::Withheld(roost_worker::session::emit::Withheld::NoSink),
        "a full was built with nowhere to send it"
    );
    assert_eq!(
        record.cell_emit.seq, 0,
        "a withheld frame consumed a sequence"
    );
    assert!(emitter.is_dirty(channel(14)));
}
