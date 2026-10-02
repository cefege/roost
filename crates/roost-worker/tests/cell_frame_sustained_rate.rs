//! The product's SUSTAINED cell-frame rate, at the producer's own granularity.
//!
//! `smoke/terminal/terminal-render.spec.ts:30` drives a `printf`/`sleep` loop
//! (one line every ~12 ms) and requires the browser's frame counter to climb by
//! more than 200, so the wire owes ~55 frames a second while it streams. The
//! frames that reach it are the ones the cadence builds from PTY CHUNKS, and a
//! chunk is whatever the keeper's drain hands over — so the rate the product can
//! possibly deliver is set by the chunk cadence, and the emitter's job is to
//! turn every chunk into exactly one frame without adding latency of its own.
//!
//! `cell_cadence_frame_rate.rs` pins the other half of that contract: chunks
//! faster than the coalesce window collapse to one frame per window. This file
//! pins the half the browser oracle actually runs in — a producer SLOWER than
//! the window, where every chunk is a frame — and the latency half, which no
//! count can see: a frame whose emit stamp has moved past the arrival stamp of
//! the bytes it carries is a frame the reader watched arrive late.
//!
//! WHY THE CADENCES BELOW ARE THE TWO INTERESTING ONES, AND WHAT THEY MEASURE.
//! The keeper forwards a PTY's output as its reader hands it over
//! (`crates/roost-keeper/src/server/connection.rs`), so the chunk cadence the
//! worker sees is the PRODUCER'S. A producer at the coalesce window (16 ms) and
//! one at twice it (32 ms) are both driven here so the rate law is pinned on
//! each side of the window rather than at one point of it, because the rate a
//! reader sees is the producer's, not the emitter's.
//!
//! The latency half is read off the frames' own stamps: a frame whose emit
//! stamp has moved past the arrival stamp of the bytes it carries
//! (`pty_out_ms != worker_emit_ms`) is a frame the reader watched arrive late.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use roost_protocol::cell::CellGridFrame;
use roost_protocol::cell::frame_chunks::CellGridSnapshotPart;
use roost_protocol::wire::brand::ChannelId;
use roost_worker::session::cell_sink::{CellSink, CellSinkResult, FrameTimings};
use roost_worker::session::emit::{CELL_EMIT_COALESCE_MS, CellEmitter};
use roost_worker::session::types::SessionRecord;

#[path = "session_emit_support/mod.rs"]
mod support;
use support::{RecordFixture, channel, stream_id};

/// A sink that keeps every frame with the two clocks it was measured with.
#[derive(Default)]
struct TimedSink {
    log: Mutex<Vec<(CellGridFrame, FrameTimings)>>,
}

impl TimedSink {
    fn taken(&self) -> Vec<(CellGridFrame, FrameTimings)> {
        self.log.lock().unwrap().clone()
    }
}

impl CellSink for TimedSink {
    fn id(&self) -> &str {
        "rate"
    }

    fn send_frame(
        &self,
        _channel_id: ChannelId,
        frame: &CellGridFrame,
        timings: FrameTimings,
    ) -> CellSinkResult {
        self.log.lock().unwrap().push((frame.clone(), timings));
        CellSinkResult::Sent
    }

    fn send_snapshot_part(
        &self,
        _channel_id: ChannelId,
        _part: &CellGridSnapshotPart,
        _timings: FrameTimings,
    ) -> CellSinkResult {
        CellSinkResult::Sent
    }
}

/// What one paced run shipped, and the virtual time it took.
struct Run {
    taken: Vec<(CellGridFrame, FrameTimings)>,
    elapsed: Duration,
    owed_at_rest: usize,
}

/// Drive `chunks` PTY chunks through the emitter at `interval` apart, running
/// the cadence exactly as `runtime::cell_cadence` runs it: a pass for every
/// owed channel, then a wait bounded by whichever comes first, the next chunk
/// or the cadence's own deadline.
fn paced(interval: Duration, chunks: u64) -> Run {
    let fixture = RecordFixture::new();
    let mut record = fixture.record(channel(9), 110, 32);
    let sink = Arc::new(TimedSink::default());
    let mut emitter = CellEmitter::new();
    emitter.register_sink(sink.clone());
    emitter.install_stream(&mut record, &stream_id(9));
    emitter.emit_cell_frame(&mut record, true, 1_000);

    let base = Instant::now();
    let mut clock = base;
    let mut next_chunk = base + interval;
    let mut epoch_ms = 1_010i64;
    let mut chunk = 0u64;

    fn pass(emitter: &mut CellEmitter, record: &mut SessionRecord, at: Instant, epoch_ms: i64) {
        for _owed in &emitter.cadence_work(at).channels {
            emitter.run_cadence_work(record, epoch_ms, at);
        }
    }

    while chunk < chunks {
        pass(&mut emitter, &mut record, clock, epoch_ms);
        let after = emitter.cadence_work(clock);
        let ingest_at = (chunk < chunks).then_some(next_chunk);
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
            epoch_ms += interval.as_millis() as i64;
            emitter.ingest_pty_chunk_at(
                &mut record,
                format!("READERLINE-{chunk:04} streaming\r\n").as_bytes(),
                epoch_ms,
                clock,
            );
            chunk += 1;
            next_chunk = clock + interval;
        }
    }
    // Settle the trailing window, so the count is the stream's and not the
    // clock's.
    for _settle in 0..4 {
        if emitter.cadence_work(clock).channels.is_empty() {
            break;
        }
        pass(&mut emitter, &mut record, clock, epoch_ms);
        clock += Duration::from_millis(CELL_EMIT_COALESCE_MS as u64);
    }
    Run {
        taken: sink.taken(),
        elapsed: clock - base,
        owed_at_rest: emitter.cadence_work(clock).channels.len(),
    }
}

/// The rate law, asserted on both sides of the coalesce window: the emitter
/// never loses a chunk group, never ships one the producer did not earn, and
/// never delays a frame past the arrival of the bytes it carries.
fn assert_rate_law(interval: Duration, chunks: u64, what: &str) -> Run {
    let run = paced(interval, chunks);
    assert_eq!(
        run.owed_at_rest, 0,
        "{what}: an emission was still owed once the stream went quiet"
    );
    // The baseline this fixture installs is frame zero of the run; the frames
    // under test are everything after it.
    let shipped = run.taken.len().saturating_sub(1);
    let owed = usize::try_from(chunks).unwrap();
    if interval >= Duration::from_millis(CELL_EMIT_COALESCE_MS as u64) {
        assert_eq!(
            shipped, owed,
            "{what}: {chunks} chunks at {interval:?} shipped {shipped} frames; a producer \
             slower than the {CELL_EMIT_COALESCE_MS}ms window owes one frame per chunk group"
        );
    } else {
        let windows =
            usize::try_from(run.elapsed.as_millis() / CELL_EMIT_COALESCE_MS as u128).unwrap_or(0);
        assert!(
            shipped + 1 >= windows && shipped <= windows + 1,
            "{what}: {chunks} chunks at {interval:?} shipped {shipped} frames over {:?}, \
             which is neither one per {CELL_EMIT_COALESCE_MS}ms window ({windows}) nor fewer",
            run.elapsed
        );
    }
    let mut previous = 0u64;
    for (index, (frame, timings)) in run.taken.iter().enumerate().skip(1) {
        assert!(
            frame.seq > previous,
            "{what}: frame {index} carries sequence {} after {previous}",
            frame.seq
        );
        previous = frame.seq;
        assert_eq!(
            timings.pty_out_ms,
            timings.worker_emit_ms,
            "{what}: frame {index} was emitted {}ms after the bytes it carries arrived; \
             a frame the reader watches arrive late is a frame the coalesce window spent",
            timings.worker_emit_ms - timings.pty_out_ms
        );
    }
    run
}

/// A producer slower than the window is the case the browser oracle runs in:
/// every chunk group is a frame, and the sustained rate is the producer's.
#[test]
fn a_producer_slower_than_the_window_ships_one_frame_per_chunk_group() {
    let interval = Duration::from_millis(32);
    let run = assert_rate_law(interval, 120, "a producer at twice the window");
    let rate = run.taken.len().saturating_sub(1) as f64 / run.elapsed.as_secs_f64();
    assert!(
        (30.0..=32.0).contains(&rate),
        "a {interval:?} producer sustained {rate:.1} frames/s; the producer's cadence is the rate"
    );
}

/// A producer at the coalesce window: the same stream at 16 ms earns twice the
/// frames, which is the whole of the difference between a rate set by the
/// producer and one set by the window.
#[test]
fn a_producer_at_the_window_earns_one_frame_per_chunk_group() {
    let interval = Duration::from_millis(CELL_EMIT_COALESCE_MS as u64);
    let run = assert_rate_law(interval, 120, "a producer at the coalesce window");
    let rate = run.taken.len().saturating_sub(1) as f64 / run.elapsed.as_secs_f64();
    assert!(
        (60.0..=63.0).contains(&rate),
        "a {interval:?} producer sustained {rate:.1} frames/s; the producer's cadence is the rate"
    );
}
