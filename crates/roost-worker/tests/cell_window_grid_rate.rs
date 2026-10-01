//! The cell cadence's window GRID: a rate that does not carry the scheduler's
//! overshoot.
//!
//! `smoke/terminal/terminal-render.spec.ts:30` ("streaming sequence repair
//! leaves an off-bottom reader fixed") streams 300 lines from a shell loop of
//! `printf` + `sleep 0.01` and requires the browser's wire-frame counter to
//! climb by more than 200 while it runs. That burst lasts as long as the
//! generator takes — 2.99 s is its floor at a perfect 10 ms per iteration,
//! 3.42 s measured on this tree — so the spec's 200 frames demand a sustained
//! period of 15.9-17.1 ms, which is the 16 ms coalesce window itself, with no
//! room for a timer.
//!
//! The window is a RATE, and a rate is only the window if the deadlines that
//! produce it are one window apart. A deadline derived from the pass that ran
//! the previous one is one window plus however late that pass started, and the
//! lateness is paid again on every frame: measured on the real driver
//! (`CellCadence::drive` over a real `CellEmitter`, chunks paced at the keeper's
//! 16 ms drain) the same stream shipped 196-200 frames in 3.42 s at a median
//! gap of 17.1-17.2 ms, where the window itself is 16 ms. Anchoring each window
//! on the deadline that came due instead of on the pass that ran it measured
//! 211-212 frames at a median gap of 15.9 ms, on every run.
//!
//! These two tests pin the arithmetic, not the wall clock: both drive the
//! cadence with a synthetic clock that runs each pass a DIFFERENT amount late,
//! which is what a runtime's 1 ms timer wheel does, and neither can be made to
//! pass by a machine that happens to be idle.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::{Duration, Instant};

#[path = "session_emit_support/mod.rs"]
mod support;

use roost_worker::session::cell_scheduler::CELL_EMIT_COALESCE;
use roost_worker::session::emit::CellEmitter;
use roost_worker::session::types::SessionRecord;
use support::{Answer, RecordFixture, RecordingSink, channel, stream_id};

/// The lateness a tokio timer hands a deadline, cycling: the wheel is 1 ms
/// granular, so the overshoot is neither zero nor constant.
const OVERSHOOTS: [Duration; 4] = [
    Duration::from_micros(500),
    Duration::from_micros(1_900),
    Duration::from_micros(1_200),
    Duration::from_micros(1_600),
];

/// The oracle's own arithmetic: `sleep 0.01` per line, measured on this tree.
const BURST: Duration = Duration::from_millis(3_420);
/// The frames that burst owes the browser, from the spec's threshold.
const OWED: usize = 200;

/// A live stream with its baseline installed, the sink that counts what the
/// cadence ships, and the clock the cadence will be driven on.
fn armed() -> (CellEmitter, SessionRecord, Arc<RecordingSink>, Instant) {
    let fixture = RecordFixture::new();
    let mut record = fixture.record(channel(9), 110, 32);
    let sink = RecordingSink::new("window-grid", Answer::Sent);
    let mut emitter = CellEmitter::new();
    emitter.register_sink(sink.clone());
    emitter.install_stream(&mut record, &stream_id(9));
    emitter.emit_cell_frame(&mut record, true, 1_000);
    (emitter, record, sink, Instant::now())
}

/// One cadence pass for everything owed at `at`, exactly as
/// `runtime::cell_cadence::run_pass` runs it.
fn pass(emitter: &mut CellEmitter, record: &mut SessionRecord, at: Instant, epoch_ms: i64) {
    for _owed in &emitter.cadence_work(at).channels {
        emitter.run_cadence_work(record, epoch_ms, at);
    }
}

/// One line of the oracle's generator.
fn line(index: u64) -> String {
    format!("READERLINE-{index:04} streaming\r\n")
}

/// The deadline the cadence owes after a pass, which is what its driver sleeps
/// on. A trailing pass always rearms; `expect` is the fixture's exemption.
fn owed(emitter: &CellEmitter, at: Instant) -> Instant {
    emitter
        .cadence_work(at)
        .next_deadline
        .expect("a trailing pass rearms the window")
}

/// A paced stream driven by a clock that runs every pass late, reporting the
/// windows it armed and how many frames it shipped while the stream was live.
struct Burst {
    windows: Vec<Duration>,
    shipped: usize,
    clock: Instant,
    base: Instant,
}

fn paced_burst() -> Burst {
    let (mut emitter, mut record, sink, base) = armed();
    let mut epoch_ms = 1_010i64;
    let mut clock = base;
    // The leading emit: a chunk with nothing armed ships at once and starts
    // the grid.
    emitter.ingest_pty_chunk_at(&mut record, line(0).as_bytes(), epoch_ms, clock);
    pass(&mut emitter, &mut record, clock, epoch_ms);
    let mut windows = vec![owed(&emitter, clock) - base];
    let before = sink.frames().len();
    let mut index = 1u64;
    while clock < base + BURST {
        epoch_ms += 11;
        emitter.ingest_pty_chunk_at(&mut record, line(index).as_bytes(), epoch_ms, clock);
        index += 1;
        // The window comes due, and the pass that runs it starts late.
        clock = base + windows[windows.len() - 1] + OVERSHOOTS[index as usize % OVERSHOOTS.len()];
        pass(&mut emitter, &mut record, clock, epoch_ms);
        windows.push(owed(&emitter, clock) - base);
    }
    Burst {
        windows,
        shipped: sink.frames().len() - before,
        clock,
        base,
    }
}

/// The window grid is one window per window, whatever the pass that ran the
/// last one cost. Before the anchor, every window here was 16 ms plus its own
/// pass's lateness, and the stream ran at 17.3 ms per frame.
#[test]
fn a_late_pass_does_not_push_the_next_window_out() {
    let burst = paced_burst();
    let mut drift = Duration::ZERO;
    for pair in burst.windows.windows(2) {
        drift = drift.max(pair[1] - pair[0]);
    }
    assert_eq!(
        drift,
        CELL_EMIT_COALESCE,
        "the window grid advanced by {:?} at its widest over {} windows; a window \
         carries the lateness of the pass that armed it, so the stream runs \
         below the rate the window claims",
        drift,
        burst.windows.len()
    );
    assert!(
        burst.clock > burst.base + BURST,
        "the run stopped before the burst it claims to have driven"
    );
}

/// The same arithmetic read as the spec reads it: a burst of the oracle's own
/// length owes the browser its 200 frames. Before the anchor this run shipped
/// 197 of them at a mean 17.3 ms per frame, which is the 196 the browser saw.
#[test]
fn the_oracle_burst_earns_its_frames_from_the_window() {
    let burst = paced_burst();
    let windows = burst.windows.len().saturating_sub(1);
    let covered = burst.windows[windows] - burst.windows[0];
    assert!(
        burst.shipped >= OWED,
        "a {BURST:?} paced burst shipped {} frames, short of the {OWED} the \
         browser's counter is asked to climb: {} windows covered {covered:?}, so \
         the sustained rate was {:?} a frame",
        burst.shipped,
        windows,
        covered / windows as u32
    );
}
