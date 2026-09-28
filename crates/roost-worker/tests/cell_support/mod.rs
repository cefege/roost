//! Shared harness for the `cell_*` tests: a scripted sink that records every
//! frame ATTEMPT (v2's `frameAttempts`), and a record + emitter pair driven on a
//! synthetic monotonic clock so the 16 ms cadence and the 1 s synchronized-output
//! ceiling are exact rather than slept.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use roost_protocol::cell::CellGridFrame;
use roost_protocol::cell::frame_chunks::CellGridSnapshotPart;
use roost_protocol::wire::brand::ChannelId;
use roost_worker::session::cell_sink::{CellSink, CellSinkResult, FrameTimings};
use roost_worker::session::emit::CellEmitter;
use roost_worker::session::types::SessionRecord;

#[path = "../session_emit_support/mod.rs"]
pub mod emit_support;

pub use emit_support::{RecordFixture, channel, stream_id};

/// A sink whose answers a test flips mid-run, recording every attempt.
pub struct ScriptedSink {
    id: String,
    pub drop_next_delta: AtomicBool,
    pub refuse_all: AtomicBool,
    pub overflow: AtomicBool,
    attempts: Mutex<Vec<(CellGridFrame, CellSinkResult)>>,
    overflows: AtomicUsize,
}

impl ScriptedSink {
    pub fn new(id: &str) -> Arc<Self> {
        Arc::new(Self {
            id: id.to_owned(),
            drop_next_delta: AtomicBool::new(false),
            refuse_all: AtomicBool::new(false),
            overflow: AtomicBool::new(false),
            attempts: Mutex::new(Vec::new()),
            overflows: AtomicUsize::new(0),
        })
    }

    /// Every frame handed to this sink, accepted or not.
    pub fn attempts(&self) -> Vec<CellGridFrame> {
        self.attempts
            .lock()
            .unwrap()
            .iter()
            .map(|(frame, _)| frame.clone())
            .collect()
    }

    /// Only the frames this sink accepted.
    pub fn frames(&self) -> Vec<CellGridFrame> {
        self.attempts
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, answer)| *answer == CellSinkResult::Sent)
            .map(|(frame, _)| frame.clone())
            .collect()
    }

    pub fn fulls(&self) -> Vec<bool> {
        self.attempts().iter().map(|frame| frame.full).collect()
    }

    pub fn overflow_notices(&self) -> usize {
        self.overflows.load(Ordering::SeqCst)
    }
}

impl CellSink for ScriptedSink {
    fn id(&self) -> &str {
        &self.id
    }

    fn send_frame(
        &self,
        _channel_id: ChannelId,
        frame: &CellGridFrame,
        _timings: FrameTimings,
    ) -> CellSinkResult {
        let answer = if self.overflow.load(Ordering::SeqCst) {
            CellSinkResult::Overflow
        } else if self.refuse_all.load(Ordering::SeqCst)
            || (!frame.full && self.drop_next_delta.swap(false, Ordering::SeqCst))
        {
            CellSinkResult::Dropped
        } else {
            CellSinkResult::Sent
        };
        self.attempts.lock().unwrap().push((frame.clone(), answer));
        answer
    }

    fn send_snapshot_part(
        &self,
        _channel_id: ChannelId,
        _part: &CellGridSnapshotPart,
        _timings: FrameTimings,
    ) -> CellSinkResult {
        if self.refuse_all.load(Ordering::SeqCst) {
            CellSinkResult::Dropped
        } else {
            CellSinkResult::Sent
        }
    }

    fn on_overflow(&self) {
        self.overflows.fetch_add(1, Ordering::SeqCst);
    }
}

/// The text of one viewport row, or "" when the frame does not carry it.
pub fn row_text(frame: &CellGridFrame, row: u32) -> String {
    frame
        .viewport_rows
        .iter()
        .find(|candidate| candidate.index == row)
        .map(|candidate| {
            candidate
                .spans
                .iter()
                .map(|span| span.text.as_str())
                .collect()
        })
        .unwrap_or_default()
}

/// One record, one emitter, a coordinator sink, and a synthetic clock.
pub struct Harness {
    pub fixture: RecordFixture,
    pub record: SessionRecord,
    pub emitter: CellEmitter,
    pub coord: Arc<ScriptedSink>,
    pub channel: ChannelId,
    pub t0: Instant,
}

pub const TEST_COLS: u16 = 80;
pub const TEST_ROWS: u16 = 24;

impl Harness {
    pub fn new(channel_number: i64) -> Self {
        let fixture = RecordFixture::new();
        let channel_id = channel(channel_number);
        let record = fixture.record(channel_id, TEST_COLS, TEST_ROWS);
        let coord = ScriptedSink::new("coord");
        let mut emitter = CellEmitter::new();
        emitter.register_cell_sink(coord.clone());
        Self {
            fixture,
            record,
            emitter,
            coord,
            channel: channel_id,
            t0: Instant::now(),
        }
    }

    pub fn at(&self, offset_ms: u64) -> Instant {
        self.t0 + Duration::from_millis(offset_ms)
    }

    pub fn wall(offset_ms: u64) -> i64 {
        1_000_000 + offset_ms as i64
    }

    /// v2 `enableStream`: commit the stream and install its baseline.
    pub fn enable_stream(&mut self, index: u32, offset_ms: u64) {
        self.emitter
            .install_stream(&mut self.record, &stream_id(index));
        let now = self.at(offset_ms);
        self.emitter
            .install_terminal_baseline_at(&mut self.record, Self::wall(offset_ms), now);
    }

    /// PTY bytes through the production ingest path.
    pub fn write(&mut self, bytes: &[u8], offset_ms: u64) {
        let now = self.at(offset_ms);
        self.emitter
            .ingest_pty_chunk_at(&mut self.record, bytes, Self::wall(offset_ms), now);
    }

    /// One cadence pass for this channel, as the driver runs it.
    pub fn run(&mut self, offset_ms: u64) {
        let now = self.at(offset_ms);
        self.emitter
            .run_cadence_work(&mut self.record, Self::wall(offset_ms), now);
    }

    pub fn emit(&mut self, force: bool, offset_ms: u64) {
        let now = self.at(offset_ms);
        self.emitter
            .emit_cell_frame_at(&mut self.record, force, Self::wall(offset_ms), now);
    }
}
