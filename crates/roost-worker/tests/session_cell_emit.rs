//! The cell-frame producer: what it ships, in what order, and what it refuses
//! to ship. These are the tests the emitter's incident history is written
//! against — a delta where a full was owed, a frame with a hole in its history,
//! a frame ahead of its own `opened`.

#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::time::Instant;

use roost_worker::outbox::{Lane, Outbox};

#[path = "session_emit_support/mod.rs"]
mod support;
use roost_worker::session::cell_scheduler::CellGate;
use roost_worker::session::cell_sink::local_cell_sink_id;
use roost_worker::session::emit::{
    CellEmitter, FrameOutcome, LIVE_DELTA_SCROLLBACK_ROWS_CAP, Withheld,
};

use support::{Answer, RecordFixture, RecordingSink, channel, numbered_lines};

#[test]
fn the_first_frame_is_full_and_the_next_is_a_delta() {
    let fixture = RecordFixture::new();
    let mut record = fixture.record(channel(1), 80, 24);
    let sink = RecordingSink::new("coord", Answer::Sent);
    let mut emitter = CellEmitter::new();
    emitter.register_sink(sink.clone());
    emitter.install_stream(&mut record, "stream-1");

    let first = emitter.emit_cell_frame(&mut record, true, 1_000);
    assert!(
        matches!(
            first,
            FrameOutcome::Full {
                installed: true,
                seq: 1,
                ..
            }
        ),
        "the first frame must be an installed full, got {first:?}"
    );
    emitter.ingest_pty_chunk(&mut record, b"hello", 1_010);
    let second = emitter.emit_cell_frame(&mut record, false, 1_020);
    assert!(
        matches!(second, FrameOutcome::Delta { seq: 2, .. }),
        "the frame after settled output must be a delta, got {second:?}"
    );

    let frames = sink.frames();
    assert_eq!(frames.len(), 2, "one full and one delta reached the sink");
    assert!(frames[0].full, "frame 1 claims to be a delta");
    assert!(!frames[1].full, "frame 2 claims to be a full");
    assert_eq!(frames[1].base_seq, frames[0].seq);
    assert_eq!(frames[0].cols, 80);
    assert_eq!(frames[0].rows, 24);
}

/// The alt screen is a different buffer, not a different view of the same one.
/// A delta across that boundary lands on the wrong rows, so the toggle has to
/// reframe — and say so on the frame, because the client decides what to keep
/// from `alt_screen` alone.
#[test]
fn entering_the_alt_screen_reframes_instead_of_shipping_a_delta() {
    let fixture = RecordFixture::new();
    let mut record = fixture.record(channel(2), 80, 24);
    let sink = RecordingSink::new("coord", Answer::Sent);
    let mut emitter = CellEmitter::new();
    emitter.register_sink(sink.clone());
    emitter.install_stream(&mut record, "stream-2");
    emitter.emit_cell_frame(&mut record, true, 1_000);
    emitter.ingest_pty_chunk(&mut record, b"before", 1_010);
    emitter.emit_cell_frame(&mut record, false, 1_020);

    emitter.ingest_pty_chunk(&mut record, b"\x1b[?1049h", 1_030);
    let toggled = emitter.emit_cell_frame(&mut record, false, 1_040);
    assert!(
        matches!(toggled, FrameOutcome::Full { .. }),
        "an alt-screen toggle must reframe, got {toggled:?}"
    );
    let frames = sink.frames();
    let last = frames.last().expect("the toggle shipped a frame");
    assert!(
        last.alt_screen,
        "the reframe does not say it is the alt screen"
    );
    // A reframe invalidates every row index a client holds, so it must say so
    // with a new grid epoch rather than looking like a snapshot of the same grid.
    assert_ne!(
        last.grid_epoch, frames[0].grid_epoch,
        "an alt-screen reframe kept the old grid epoch"
    );
}

/// THE TRIPWIRE. A delta's history append starts at the retained floor, so a
/// client that fell further behind than the ring's depth would splice an
/// invisible hole into its scrollback and never know. Past the cap the honest
/// frame is a full — and a capped checkpoint is VIEWPORT-ONLY, because
/// carrying its history is exactly what the cap exists to refuse.
#[test]
fn a_delta_past_the_row_cap_becomes_a_viewport_only_full() {
    let fixture = RecordFixture::new();
    let mut record = fixture.record(channel(3), 80, 24);
    let sink = RecordingSink::new("coord", Answer::Sent);
    let mut emitter = CellEmitter::new();
    emitter.register_sink(sink.clone());
    emitter.install_stream(&mut record, "stream-3");
    let baseline = emitter.emit_cell_frame(&mut record, true, 1_000);
    let baseline_seq = match baseline {
        FrameOutcome::Full { seq, .. } => seq,
        other => panic!("the baseline must be a full, got {other:?}"),
    };

    // More history rows in one coalesce window than a delta may append.
    let flood = numbered_lines(LIVE_DELTA_SCROLLBACK_ROWS_CAP as usize + 40);
    emitter.ingest_pty_chunk(&mut record, flood.as_bytes(), 1_010);
    let capped = emitter.emit_cell_frame(&mut record, false, 1_020);
    assert!(
        matches!(
            capped,
            FrameOutcome::Full {
                installed: true,
                ..
            }
        ),
        "a delta appending past the row cap must become a full, got {capped:?}"
    );

    let frames = sink.frames();
    let checkpoint = frames.last().expect("the checkpoint shipped a frame");
    assert!(checkpoint.full, "the capped frame is not marked full");
    assert!(
        checkpoint.scrollback_append.is_empty(),
        "a capped checkpoint carried a history append, which is the hole it exists to avoid"
    );
    assert!(
        checkpoint.scrollback_rows.is_empty(),
        "a capped checkpoint carried retained history"
    );
    assert_eq!(
        checkpoint.sb_base, checkpoint.scrollback_total,
        "a capped checkpoint's base must equal its total, or the client's history re-aliases"
    );
    assert_ne!(
        checkpoint.seq, baseline_seq,
        "the capped checkpoint reused a sequence number"
    );
    // A capped checkpoint is NOT a semantic reframe: the grid did not change,
    // so bumping the epoch would invalidate row indexes for nothing.
    assert_eq!(
        checkpoint.grid_epoch, frames[0].grid_epoch,
        "a capped checkpoint advanced the grid epoch"
    );
}

/// A delta too large for one wire part cannot be chunked: a delta has no
/// snapshot identity, so a receiver assembling it from parts could not know it
/// had lost one. It is rebuilt as a full, which does.
#[test]
fn a_delta_too_large_for_one_part_is_escalated_to_a_full() {
    const COLS: u16 = 3000;
    const ROWS: u16 = 60;
    let fixture = RecordFixture::new();
    let mut record = fixture.record(channel(4), COLS, ROWS);
    let sink = RecordingSink::new("coord", Answer::Sent);
    let mut emitter = CellEmitter::new();
    emitter.register_sink(sink.clone());
    emitter.install_stream(&mut record, "stream-4");
    emitter.emit_cell_frame(&mut record, true, 1_000);

    // One styled cell per column: a span per cell is what pushes a delta past
    // the 1 MiB part limit, which a plain run of text never would.
    let mut styled = String::new();
    for row in 0..ROWS {
        for col in 0..COLS {
            styled.push_str(&format!("\x1b[38;5;{}mX", (row + col) % 256));
        }
        styled.push_str("\r\n");
    }
    emitter.ingest_pty_chunk(&mut record, styled.as_bytes(), 1_010);
    let escalated = emitter.emit_cell_frame(&mut record, false, 1_020);
    assert!(
        matches!(
            escalated,
            FrameOutcome::Full {
                installed: true,
                ..
            }
        ),
        "an oversized delta must be escalated to a full, got {escalated:?}"
    );
    for frame in sink.frames().iter().skip(1) {
        assert!(frame.full, "an oversized delta was shipped as a delta");
    }
    assert!(
        !sink.parts().is_empty(),
        "a full past the part limit must ship as snapshot parts, and none did"
    );
}

/// A channel with no installed stream has no stream id to address a frame by,
/// so nothing may be handed to a sink for it — not even a well-formed full.
#[test]
fn a_channel_with_no_installed_stream_ships_nothing() {
    let fixture = RecordFixture::new();
    let mut record = fixture.record(channel(5), 80, 24);
    let sink = RecordingSink::new("coord", Answer::Sent);
    let mut emitter = CellEmitter::new();
    emitter.register_sink(sink.clone());

    emitter.ingest_pty_chunk(&mut record, b"prompt", 1_000);
    let withheld = emitter.emit_cell_frame(&mut record, true, 1_010);
    assert_eq!(
        withheld,
        FrameOutcome::Withheld(Withheld::NoStream),
        "an unadopted channel must not emit"
    );
    assert!(sink.frames().is_empty(), "a frame reached a sink anyway");
}

/// A gate names the reason emission stopped, and the reason is what a stalled
/// emitter is diagnosed from. The bytes keep accumulating; nothing is lost.
#[test]
fn a_held_channel_emits_nothing_and_says_which_gate_holds_it() {
    let fixture = RecordFixture::new();
    let mut record = fixture.record(channel(6), 80, 24);
    let sink = RecordingSink::new("coord", Answer::Sent);
    let mut emitter = CellEmitter::new();
    emitter.register_sink(sink.clone());
    emitter.install_stream(&mut record, "stream-6");
    emitter.emit_cell_frame(&mut record, true, 1_000);

    emitter.hold_frames(channel(6), CellGate::SyncOutput, 1_005);
    emitter.ingest_pty_chunk(&mut record, b"half a repaint", 1_010);
    assert_eq!(
        emitter.emit_cell_frame(&mut record, false, 1_020),
        FrameOutcome::Withheld(Withheld::Gate)
    );
    let held = emitter
        .gate_suppression(channel(6))
        .expect("the gate is still open");
    assert_eq!(held.gate, CellGate::SyncOutput);
    assert_eq!(held.since_ms, 1_005);
    assert_eq!(held.suppressed, 1);

    emitter.release_frames(channel(6));
    assert!(emitter.gate_suppression(channel(6)).is_none());
    assert!(
        emitter.is_dirty(channel(6)),
        "the withheld work was not remembered for the release"
    );
    let resumed = emitter.emit_cell_frame(&mut record, false, 1_030);
    assert!(
        matches!(resumed, FrameOutcome::Delta { .. }),
        "the release must let the owed frame out, got {resumed:?}"
    );
}

/// A cell frame ahead of its own `opened` is a frame the browser cannot place,
/// and the failure looks like a terminal that never paints. The producer's part
/// in that ordering is that a cell frame can only reach the TERMINAL lane: a
/// sink has no way to put one in the durable lane, so the outbox's own drain
/// order is the whole guarantee.
#[test]
fn a_cell_frame_goes_to_the_terminal_lane_and_drains_behind_the_opened_event() {
    let fixture = RecordFixture::new();
    let mut record = fixture.record(channel(7), 80, 24);
    let mut outbox = Outbox::default();
    let sink = RecordingSink::new("coord", Answer::Sent);
    let mut emitter = CellEmitter::new();
    emitter.register_sink(sink.clone());
    emitter.install_stream(&mut record, "stream-7");
    emitter.emit_cell_frame(&mut record, true, 1_000);

    // What the coordinator sink does with a frame: encode it and admit it to
    // the terminal lane. The encoding is the sink's business; the lane is the
    // property under test.
    for frame in sink.frames() {
        outbox
            .admit(
                Lane::Terminal,
                frame.seq.to_le_bytes().to_vec(),
                "cell",
                Instant::now(),
            )
            .expect("the terminal lane has room");
    }
    // The `opened` event is admitted afterwards, as a durable pump that was
    // waiting on the spawn would admit it.
    outbox
        .admit(
            Lane::Durable,
            b"opened".to_vec(),
            "session.opened",
            Instant::now(),
        )
        .expect("the durable lane has room");

    let first = outbox.drain_one(Instant::now()).expect("something drains");
    assert_eq!(
        first.lane,
        Lane::Durable,
        "the durable event must drain before the cell frame it announces"
    );
    assert_eq!(first.label, "session.opened");
}

/// A suspended transport is told nothing, not even a refusal: a dead
/// coordinator that keeps being handed frames is a dead coordinator that cannot
/// be told it is dead.
#[test]
fn a_suspended_sink_is_handed_nothing_and_owes_a_full_on_resume() {
    let fixture = RecordFixture::new();
    let mut record = fixture.record(channel(8), 80, 24);
    let coord = RecordingSink::new("coord", Answer::Sent);
    let local = RecordingSink::new(&local_cell_sink_id("7"), Answer::Sent);
    let mut emitter = CellEmitter::new();
    emitter.register_sink(coord.clone());
    emitter.register_sink(local.clone());
    emitter.install_stream(&mut record, "stream-8");
    emitter.emit_cell_frame(&mut record, true, 1_000);

    emitter.suspend_sink("coord");
    emitter.ingest_pty_chunk(&mut record, b"while suspended", 1_010);
    emitter.emit_cell_frame(&mut record, false, 1_020);
    assert_eq!(
        coord.frames().len(),
        1,
        "a suspended sink was handed a frame"
    );
    assert_eq!(local.frames().len(), 2, "the live sibling stopped painting");

    emitter.resume_sink("coord");
    let after = emitter.emit_cell_frame(&mut record, false, 1_030);
    assert!(
        matches!(after, FrameOutcome::Full { .. }),
        "a resumed sink owes a full, not a delta, got {after:?}"
    );
    assert!(
        coord.frames().last().expect("a frame arrived").full,
        "the resumed sink was handed a delta"
    );
}
