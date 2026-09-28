//! The deterministic screen hub harness: frame builders, a recording socket
//! sink, a recording replica owner, and deadlines the test fires by hand.
//!
//! Ports `apps/coord/tests/terminal/screen/terminal-screen-hub-harness.ts`.
//! Shared by the `terminal_screen_hub*`, `terminal_screen_fanout` and
//! `terminal_view_owner_screen` binaries; the recorders are `sink.rs`.

#![allow(dead_code, unused_imports, clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use roost_coord::terminal_screen::hub_contract::{ScreenTimers, TerminalScreenSocketSink};
use roost_coord::terminal_screen::screen_budget::TerminalScreenCaps;
use roost_coord::terminal_screen::{ScreenHub, ScreenReplicaSink};
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::buffa::MessageField;
use roost_proto::{FirehoseFrame, PbCellGridChunk, PbCellGridFrame, PbCellRow, PbCellSpan};
use roost_protocol::wire::SessionId;

mod sink;

pub use sink::{ManualTimers, RecordingOwner, Reentry, Served, TestSink};

pub const SESSION: &str = "40000000-0000-4000-8000-000000000001";
pub const OTHER_SESSION: &str = "40000000-0000-4000-8000-000000000002";
pub const STREAM: &str = "50000000-0000-4000-8000-000000000001";
pub const OTHER_STREAM: &str = "50000000-0000-4000-8000-000000000002";
pub const SNAPSHOT_A: &str = "60000000-0000-4000-8000-000000000001";
pub const SNAPSHOT_B: &str = "60000000-0000-4000-8000-000000000002";
pub const EPOCH: &str = "grid-epoch-a";

pub fn session() -> SessionId {
    SessionId::try_from(SESSION).unwrap()
}

pub fn other_session() -> SessionId {
    SessionId::try_from(OTHER_SESSION).unwrap()
}

/// One `(session, stream)` repair request, as the owner records it.
pub fn request(stream_id: &str) -> (String, String) {
    (SESSION.to_owned(), stream_id.to_owned())
}

pub fn row(index: u32, text: &str) -> PbCellRow {
    PbCellRow {
        index,
        spans: if text.is_empty() {
            Vec::new()
        } else {
            vec![PbCellSpan {
                text: text.to_owned(),
                columns: 1,
                fg: 256,
                bg: 256,
                ..Default::default()
            }]
        },
        ..Default::default()
    }
}

/// A full baseline of `rows` rows reading `r0`, `r1`, ... unless `texts` says
/// otherwise.
pub fn full_frame(
    stream_id: &str,
    seq: u64,
    cols: u32,
    rows: u32,
    texts: &[&str],
) -> PbCellGridFrame {
    PbCellGridFrame {
        session_id: "worker-owned".to_owned(),
        stream_id: stream_id.to_owned(),
        grid_epoch: EPOCH.to_owned(),
        cols,
        rows,
        full: true,
        seq,
        base_seq: 0,
        viewport_rows: (0..rows)
            .map(|index| {
                let fallback = format!("r{index}");
                let text = texts
                    .get(index as usize)
                    .copied()
                    .unwrap_or(fallback.as_str());
                row(index, text)
            })
            .collect(),
        cursor_visible: true,
        ..Default::default()
    }
}

/// The default two-row, eight-column baseline at `seq`, reading `texts`.
pub fn baseline(seq: u64, texts: &[&str]) -> PbCellGridFrame {
    full_frame(STREAM, seq, 8, 2, texts)
}

/// A delta patching `patch_index` to `text` on top of `base_seq`, flipping
/// every terminal mode the v2 harness flips, so a fold that drops one shows.
pub fn delta_frame(
    stream_id: &str,
    base_seq: u64,
    patch_index: u32,
    text: &str,
) -> PbCellGridFrame {
    PbCellGridFrame {
        session_id: "worker-owned".to_owned(),
        stream_id: stream_id.to_owned(),
        grid_epoch: EPOCH.to_owned(),
        cols: 8,
        rows: 2,
        full: false,
        seq: base_seq + 1,
        base_seq,
        viewport_rows: vec![row(patch_index, text)],
        cursor_row: 1,
        cursor_col: 2,
        cursor_visible: false,
        cursor_keys_app: true,
        bracketed_paste: true,
        mouse_tracking: 1000,
        mouse_sgr: true,
        focus_events: true,
        ..Default::default()
    }
}

/// The v2 harness's default delta: row 1 reads `text` on top of `base_seq`.
pub fn delta(base_seq: u64, text: &str) -> PbCellGridFrame {
    delta_frame(STREAM, base_seq, 1, text)
}

/// `source` split into one chunk per row group.
pub fn chunks(
    source: &PbCellGridFrame,
    groups: &[Vec<PbCellRow>],
    snapshot_id: &str,
) -> Vec<PbCellGridChunk> {
    let count = u32::try_from(groups.len()).unwrap();
    groups
        .iter()
        .enumerate()
        .map(|(index, group)| {
            let mut part = source.clone();
            part.viewport_rows = group.clone();
            PbCellGridChunk {
                snapshot_id: snapshot_id.to_owned(),
                chunk_index: u32::try_from(index).unwrap(),
                chunk_count: count,
                part: MessageField::some(part),
                ..Default::default()
            }
        })
        .collect()
}

/// `source` split into one chunk per viewport row.
pub fn row_chunks(source: &PbCellGridFrame, snapshot_id: &str) -> Vec<PbCellGridChunk> {
    let groups: Vec<Vec<PbCellRow>> = source
        .viewport_rows
        .iter()
        .map(|row| vec![row.clone()])
        .collect();
    chunks(source, &groups, snapshot_id)
}

pub fn texts(frame: &PbCellGridFrame) -> Vec<String> {
    frame
        .viewport_rows
        .iter()
        .map(|row| row.spans.iter().map(|span| span.text.as_str()).collect())
        .collect()
}

/// The cell grid an outbound delta carries.
pub fn grid_of(frame: &FirehoseFrame) -> PbCellGridFrame {
    match &frame.frame {
        Some(Frame::CellGrid(grid)) => (**grid).clone(),
        other => panic!("a delta is a cell grid frame, got {other:?}"),
    }
}

/// A hub, its owner's recording, its deadlines and its clock.
pub struct Harness {
    pub hub: Arc<ScreenHub>,
    pub owner: Arc<RecordingOwner>,
    pub timers: Arc<ManualTimers>,
    pub clock: Arc<AtomicU64>,
}

impl Harness {
    pub fn requests(&self) -> Vec<(String, String)> {
        self.owner.requests.lock().unwrap().clone()
    }

    pub fn fresh_streams(&self) -> Vec<(String, String, String)> {
        self.owner.fresh_streams.lock().unwrap().clone()
    }

    pub fn unavailable(&self) -> Vec<(String, String)> {
        self.owner.unavailable.lock().unwrap().clone()
    }

    pub fn advance(&self, by_ms: u64) {
        self.clock.fetch_add(by_ms, Ordering::Relaxed);
    }

    /// Publish a whole frame for the default session.
    pub fn frame(&self, mut frame: PbCellGridFrame) {
        self.hub.publish_frame(&session(), &mut frame, 0);
    }

    /// Publish one chunk for the default session, received at `at_ms`.
    pub fn chunk(&self, chunk: &PbCellGridChunk, at_ms: i64) {
        self.hub
            .publish_chunk(&session(), &mut chunk.clone(), at_ms);
    }

    /// The replica's `(seq, servable)` for the default session.
    pub fn replica(&self) -> Option<(u64, bool)> {
        let seq = self.hub.current_seq(&session())?;
        Some((seq, self.hub.has_valid_cache(&session())))
    }
}

pub fn harness() -> Harness {
    harness_with_caps(TerminalScreenCaps {
        max_resident_rows: 65_536,
        max_resident_spans: 2_097_152,
    })
}

pub fn harness_with_caps(caps: TerminalScreenCaps) -> Harness {
    let owner = Arc::new(RecordingOwner::default());
    let timers = Arc::new(ManualTimers::default());
    let clock = Arc::new(AtomicU64::new(0));
    let read_clock = Arc::clone(&clock);
    let hub = Arc::new(ScreenHub::with_deadlines(
        caps,
        Arc::clone(&owner) as Arc<dyn ScreenReplicaSink>,
        Arc::clone(&timers) as Arc<dyn ScreenTimers>,
        Arc::new(move || read_clock.load(Ordering::Relaxed)),
    ));
    Harness {
        hub,
        owner,
        timers,
        clock,
    }
}

/// Register `sink` as `socket_id` and have it watch the default session.
pub fn watch(hub: &Arc<ScreenHub>, sink: &Arc<TestSink>, socket_id: &str) {
    hub.register_socket(
        socket_id,
        Arc::clone(sink) as Arc<dyn TerminalScreenSocketSink>,
    );
    hub.set_watching(socket_id, &session(), true);
}
