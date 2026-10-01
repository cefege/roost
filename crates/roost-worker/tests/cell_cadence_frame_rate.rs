//! The cell cadence's frame-RATE contract, which the browser oracle measures
//! from the other end of the wire.
//!
//! `smoke/terminal/terminal-render.spec.ts` "streaming sequence repair leaves an
//! off-bottom reader fixed" drives a paced `printf`/`sleep` loop and requires the
//! wire to carry more than 200 new cell frames. A frame is only built when a
//! scheduled emission comes due, so the number of frames a paced stream earns is
//! the cadence's own coalesce window: one frame per `CELL_EMIT_COALESCE_MS`
//! while bytes keep arriving, and never more. Both bounds are load-bearing — a
//! cadence that stops rearming starves the browser's counter, and one that
//! re-arms per chunk instead of per window ships a frame per PTY write, which is
//! the megabyte flood the direct-carrier specs exist to bound.
//!
//! The producer's INPUT GRANULARITY is the keeper's, not the cadence's, so
//! this drives one ingest per interval: what is asserted here is the window's
//! rate, for whatever chunks the cadence is handed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::{Duration, Instant};

#[path = "session_emit_support/mod.rs"]
mod support;
use roost_worker::session::emit::{CELL_EMIT_COALESCE_MS, CellEmitter};
use roost_worker::session::types::SessionRecord;
use support::{Answer, RecordFixture, RecordingSink, channel, stream_id};

/// How far apart the paced chunks arrive. Well inside the coalesce window, so
/// the LEADING edge alone could ship a frame per chunk — which is exactly what
/// the upper bound refuses.
const CHUNK_INTERVAL: Duration = Duration::from_millis(4);

/// The frames a run of this length owes, plus the one window the trailing pass
/// is still owed at its boundary.
fn window(elapsed: Duration) -> usize {
    usize::try_from(elapsed.as_millis() / CELL_EMIT_COALESCE_MS as u128).unwrap_or(0)
}

/// A record with a live stream and its baseline installed, plus the sink that
/// counts what the cadence actually shipped. The fixture outlives the record
/// it built, exactly as the other cell-emission tests hold it.
fn armed() -> (
    RecordFixture,
    SessionRecord,
    Arc<RecordingSink>,
    CellEmitter,
) {
    let fixture = RecordFixture::new();
    let mut record = fixture.record(channel(1), 110, 32);
    let sink = RecordingSink::new("coord", Answer::Sent);
    let mut emitter = CellEmitter::new();
    emitter.register_sink(sink.clone());
    emitter.install_stream(&mut record, &stream_id(1));
    emitter.emit_cell_frame(&mut record, true, 1_000);
    (fixture, record, sink, emitter)
}

/// What one paced run earned, and how long its stream ran.
struct Paced {
    shipped: usize,
    elapsed: Duration,
    owed_at_rest: usize,
}

/// Run `lines` paced chunks through the emitter, driving the cadence exactly as
/// `runtime::cell_cadence` drives it: a pass per wake, then a wait bounded by
/// whichever comes first, the next chunk or the cadence's own deadline.
///
/// `snapshot_every` re-arms the coordinator's repair path (v2
/// `requestTerminalSnapshot`: retire the stream's delivery, then install a
/// fresh baseline) on every Nth chunk, because a client that cannot fold a
/// delta asks for one on a timer, and a repair that costs the stream its
/// delivery is the obvious way to turn that timer into a stall.
fn paced(snapshot_every: Option<u64>, lines: u64) -> Paced {
    let (_fixture, mut record, sink, mut emitter) = armed();
    let base = Instant::now();
    let mut clock = base;
    let mut next_chunk = base + CHUNK_INTERVAL;
    let mut epoch_ms = 1_010i64;
    let mut chunk = 0u64;

    // One pass per owed channel. The record names its own channel, so the
    // schedule list is the repeat count, not an argument. The two metadata
    // lanes are deliberately not modelled: they owe the coordinator, not cells.
    fn pass(emitter: &mut CellEmitter, record: &mut SessionRecord, at: Instant, epoch_ms: i64) {
        for _owed in &emitter.cadence_work(at).channels {
            emitter.run_cadence_work(record, epoch_ms, at);
        }
    }

    loop {
        pass(&mut emitter, &mut record, clock, epoch_ms);
        let after = emitter.cadence_work(clock);
        if !after.channels.is_empty() {
            continue;
        }
        let ingest_at = (chunk < lines).then_some(next_chunk);
        let wake = match (ingest_at, after.next_deadline) {
            (None, None) => break,
            (Some(when), None) | (None, Some(when)) => when,
            (Some(ingest), Some(deadline)) => ingest.min(deadline),
        };
        if wake <= clock {
            break;
        }
        clock = wake;
        if ingest_at == Some(clock) {
            epoch_ms += CHUNK_INTERVAL.as_millis() as i64;
            emitter.ingest_pty_chunk_at(
                &mut record,
                format!("READERLINE-{chunk:04} streaming\r\n").as_bytes(),
                epoch_ms,
                clock,
            );
            if snapshot_every.is_some_and(|every| chunk > 0 && chunk.is_multiple_of(every)) {
                emitter.retire_stream_delivery(record.channel_id());
                emitter.clear_stream_delivery_dirty(record.channel_id());
                emitter.install_terminal_baseline(&mut record, epoch_ms);
            }
            chunk += 1;
            next_chunk = clock + CHUNK_INTERVAL;
        }
    }
    // Settle the trailing window, so the count is the stream's and not the
    // clock's: an emission the cadence armed and never ran would otherwise be
    // invisible here and visible as a terminal that stopped painting.
    for _settle in 0..4 {
        if emitter.cadence_work(clock).channels.is_empty() {
            break;
        }
        pass(&mut emitter, &mut record, clock, epoch_ms);
        clock += Duration::from_millis(CELL_EMIT_COALESCE_MS as u64);
    }
    Paced {
        shipped: sink.frames().len(),
        elapsed: clock - base,
        owed_at_rest: emitter.cadence_work(clock).channels.len(),
    }
}

/// The rate every run owes: a stream that keeps producing must not go quiet on
/// the wire, and nothing may still be owed once it has.
fn assert_no_starvation(rate: &Paced, what: &str) {
    assert_eq!(
        rate.owed_at_rest, 0,
        "{what}: an emission was still owed once the stream went quiet"
    );
    assert!(
        rate.shipped + 1 >= window(rate.elapsed),
        "{what}: {} frames over {:?} is short of the one frame per \
         {CELL_EMIT_COALESCE_MS}ms coalesce window a paced stream owes ({})",
        rate.shipped,
        rate.elapsed,
        window(rate.elapsed)
    );
}

#[test]
fn paced_output_earns_one_frame_per_coalesce_window() {
    let rate = paced(None, 300);
    assert_no_starvation(&rate, "a paced stream");
    assert!(
        rate.shipped <= window(rate.elapsed) + 1,
        "a paced stream shipped {} frames over {:?}, more than one per \
         {CELL_EMIT_COALESCE_MS}ms coalesce window ({}); the leading edge shipped per chunk",
        rate.shipped,
        rate.elapsed,
        window(rate.elapsed)
    );
}

/// A baseline request OWNS a full: it retires the stream's delivery, so the
/// frame it forces is not a coalesced one and the run may legitimately ship
/// more than a window's worth. It may never ship LESS.
#[test]
fn a_repeated_baseline_request_does_not_starve_the_cadence() {
    assert_no_starvation(
        &paced(Some(7), 300),
        "a baseline request every seventh chunk",
    );
}
