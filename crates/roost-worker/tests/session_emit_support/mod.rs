// Fakes for the cell-emission tests. An integration test is its own crate and
// `expect` is denied outside `#[cfg(test)]`, so the exemption lives here rather
// than in every test that builds a fixture.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)]

//! The record and the sink a cell-emission test drives.
//!
//! The sink records FRAMES, not calls: a test that asserts "the sink was
//! called" is satisfied by a producer that ships the wrong bytes, and the whole
//! class of defect these tests exist for — a delta where a full was owed, a
//! frame with a hole in its history, a frame ahead of its own `opened` — lives
//! in the bytes, not in the call.

use std::sync::{Arc, Mutex};

use roost_protocol::cell::CellGridFrame;
use roost_protocol::cell::frame_chunks::CellGridSnapshotPart;
use roost_protocol::wire::brand::{ChannelId, SessionId, TraceId, WorkerFp};
use roost_term::{CellEmitState, RioCore, TerminalCore};
use roost_worker::event_store::{DurableEventKind, Reservation, Store};
use roost_worker::session::cell_sink::{CellSink, CellSinkResult, FrameTimings};
use roost_worker::session::ring::ScrollbackRing;
use roost_worker::session::types::{SessionIdentity, SessionRecord};
use roost_worker::shell_spec::ShellSpec;

/// What this sink answers for every frame.
///
/// `Dropped` and `Overflow` refuse DELTAS and take every full, which is how the
/// v2 tests they port drive a refusing sink: the repair tests answer `sent` for
/// any `frame.full` (`apps/worker/tests/session/session-cell-sinks.test.ts:60`,
/// `:122`), and the overflow test flips its sink to `overflow` only after the
/// baseline landed (same file, `:91-97`). A sink that refused its baseline too
/// would park a cursor, and no delta would ever be built to refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Sent,
    /// Refuses every delta and keeps its registration.
    Dropped,
    /// Overflows on every delta: the registry drops the sink and tells it once.
    Overflow,
    /// Accepts the first `n` frames and parts, then overflows.
    SentThenOverflow(usize),
}

#[derive(Debug, Default)]
struct SinkLog {
    frames: Vec<(CellGridFrame, roost_proto::PbCellGridFrame)>,
    parts: Vec<CellGridSnapshotPart>,
    overflows: usize,
}

/// A sink that keeps everything it was handed.
pub struct RecordingSink {
    id: String,
    answer: Answer,
    log: Mutex<SinkLog>,
}

impl RecordingSink {
    pub fn new(id: &str, answer: Answer) -> Arc<Self> {
        Arc::new(Self {
            id: id.to_owned(),
            answer,
            log: Mutex::new(SinkLog::default()),
        })
    }

    /// The frames this sink was handed, in order.
    pub fn frames(&self) -> Vec<CellGridFrame> {
        self.log
            .lock()
            .unwrap()
            .frames
            .iter()
            .map(|(frame, _)| frame.clone())
            .collect()
    }

    /// The clocks the producer measured the LAST frame with. A sink cannot
    /// measure these itself, so this is how a test sees that the arrival stamp
    /// and the emit stamp travel with the frame.
    pub fn last_timings(&self) -> Option<FrameTimings> {
        self.log
            .lock()
            .unwrap()
            .frames
            .last()
            .map(|(_, wire)| FrameTimings {
                pty_out_ms: i64::try_from(wire.pty_out_ms).unwrap(),
                worker_emit_ms: i64::try_from(wire.worker_emit_ms).unwrap(),
            })
    }

    /// The wire conversion the LAST frame was handed with.
    pub fn last_wire(&self) -> Option<roost_proto::PbCellGridFrame> {
        self.log
            .lock()
            .unwrap()
            .frames
            .last()
            .map(|(_, wire)| wire.clone())
    }

    /// The parked snapshot parts this sink was handed, in order.
    pub fn parts(&self) -> Vec<CellGridSnapshotPart> {
        self.log.lock().unwrap().parts.clone()
    }

    /// How many times the registry told this sink it had been dropped.
    pub fn overflow_notices(&self) -> usize {
        self.log.lock().unwrap().overflows
    }

    /// Whether this sink's queue has taken `count` items already.
    fn saturated(&self, count: usize) -> bool {
        match self.answer {
            Answer::SentThenOverflow(limit) => count >= limit,
            _ => false,
        }
    }
}

impl CellSink for RecordingSink {
    fn id(&self) -> &str {
        &self.id
    }

    fn send_frame(
        &self,
        _channel_id: ChannelId,
        frame: &CellGridFrame,
        wire: &roost_proto::PbCellGridFrame,
    ) -> CellSinkResult {
        let mut log = self.log.lock().unwrap();
        if self.saturated(log.frames.len() + log.parts.len()) {
            return CellSinkResult::Overflow;
        }
        // These two answers were declared and never returned, so every sink in
        // the sink tests answered `Sent` and no refusal was ever exercised.
        if !frame.full {
            match self.answer {
                Answer::Dropped => return CellSinkResult::Dropped,
                Answer::Overflow => return CellSinkResult::Overflow,
                Answer::Sent | Answer::SentThenOverflow(_) => {}
            }
        }
        log.frames.push((frame.clone(), wire.clone()));
        CellSinkResult::Sent
    }

    fn send_snapshot_part(
        &self,
        _channel_id: ChannelId,
        part: &CellGridSnapshotPart,
        _timings: FrameTimings,
    ) -> CellSinkResult {
        let mut log = self.log.lock().unwrap();
        if self.saturated(log.frames.len() + log.parts.len()) {
            return CellSinkResult::Overflow;
        }
        log.parts.push(part.clone());
        CellSinkResult::Sent
    }

    fn on_overflow(&self) {
        self.log.lock().unwrap().overflows += 1;
    }
}

/// A channel id, from the number a test thinks in.
pub fn channel(value: i64) -> ChannelId {
    ChannelId::try_from(value).expect("a test channel id is a small integer")
}

/// A session id a test can name.
pub fn session_id() -> SessionId {
    SessionId::try_from("6f1d0f2c-6b3a-4f2e-9a11-0d2f5b7c8e90".to_owned())
        .expect("the test session id is a uuid")
}

// NO `sha256:` prefix. `WorkerFp` carries the bare 64-character lowercase hex
// of the pubkey digest; the prefix is a display convention, and carrying it
// here made every consumer of this fixture fail `check()` with a length error
// that reads as a protocol defect rather than a fixture defect.
pub fn worker_fp() -> WorkerFp {
    WorkerFp::try_from(
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_owned(),
    )
    .expect("the test fingerprint is well formed")
}

/// A coordinator-minted stream id for `index`.
///
/// `CellGridFrame`'s identity admission requires a UUID (`roost_protocol::
/// viewport::is_terminal_uuid`): a stream id that is not one is a coordinator
/// that never minted it, and the emitter refuses to build a frame for it — so
/// a fixture that used `stream-1` produced no frames at all and every later
/// assertion saw the withheld baseline instead. One shape, many values, so
/// tests that need two distinct streams still have two.
pub fn stream_id(index: u32) -> String {
    format!("00000000-0000-4000-8000-{index:012x}")
}

pub fn shell_spec(cwd: &str) -> ShellSpec {
    ShellSpec {
        version: 1,
        platform: roost_host::HostPlatform::Linux,
        executable: "/bin/sh".to_owned(),
        argv: vec!["-l".to_owned()],
        cwd: cwd.to_owned(),
        env: vec![("TERM".to_owned(), "xterm-256color".to_owned())],
    }
}

/// A record with a real core and a real durable claim behind it.
///
/// The store is not decoration: the claims these records carry are the ones a
/// close consumes, and a test that used invented ids would not notice a spawn
/// that released somebody else's capacity.
pub struct RecordFixture {
    pub store: Store,
    pub close_reservation: Reservation,
}

impl RecordFixture {
    pub fn new() -> Self {
        let mut store = Store::new();
        let close_reservation = store
            .reserve_default(DurableEventKind::Closed)
            .expect("a fresh store has room for one close claim");
        Self {
            store,
            close_reservation,
        }
    }

    /// A record for `channel_id`, with a core of the given geometry.
    pub fn record(&self, channel_id: ChannelId, cols: u16, rows: u16) -> SessionRecord {
        SessionRecord::new(
            SessionIdentity {
                session_id: session_id(),
                channel_id,
                socket_path: format!("mux:{channel_id}"),
                cwd: "/tmp".to_owned(),
                shell_spec: shell_spec("/tmp"),
                session_trace_id: TraceId::try_from("0123456789abcdef".to_owned())
                    .expect("the test trace id is hexadecimal"),
                spawned_at_ms: 1_000,
            },
            self.close_reservation,
            Box::new(RioCore::new(cols, rows)) as Box<dyn TerminalCore + Send>,
            CellEmitState::new("epoch-base", "stream-placeholder"),
            ScrollbackRing::default(),
        )
    }
}

/// `count` numbered lines, each ending in CRLF so the core scrolls on each one.
pub fn numbered_lines(count: usize) -> String {
    let mut text = String::with_capacity(count * 8);
    for line in 0..count {
        text.push_str(&format!("line{line}\r\n"));
    }
    text
}
