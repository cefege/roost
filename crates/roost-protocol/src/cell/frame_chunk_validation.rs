//! The cell snapshot-part limits, the chunk rejection vocabulary, and the
//! shape every part is held to before it becomes bytes.
//!
//! Both sides of the chunk protocol depend on this file: the assembler accepts
//! parts with it and the planner builds them. A divergence in which fault a part
//! reports first is a snapshot one side builds and the other refuses. The
//! per-row span and hyperlink accumulation every part goes through lives in
//! `row_accounting`.
//!
//! The byte limits are CANONICAL PROTOBUF ENCODED lengths, measured in
//! `frame_chunks` with buffa's own arithmetic, never re-derived here.

mod row_accounting;

use std::collections::{HashMap, HashSet};
use std::fmt;

use roost_proto::{PbCellGridFrame, PbCellRow};

use self::CellGridChunkErrorCode as Code;
use self::reject_cell_grid_chunk as reject;
use self::row_accounting::{account_spans, assert_part_row_spans, record_span_links};
use crate::viewport::{
    TERMINAL_MAX_COLS, TERMINAL_MAX_ROWS, TerminalGeometry, is_terminal_geometry, is_terminal_uuid,
};

/// Maximum canonical protobuf encoding of one snapshot chunk.
pub const CELL_GRID_PART_MAX_BYTES: u32 = 1_048_576;
/// Maximum sum of encoded chunks accepted for one atomic snapshot.
pub const CELL_GRID_SNAPSHOT_MAX_BYTES: u64 = 67_108_864;
/// Maximum parts one snapshot may be split into.
pub const CELL_GRID_SNAPSHOT_MAX_CHUNKS: u32 = 256;
/// Tallest grid a snapshot may describe: the core limit, restated for the contract.
pub const CELL_GRID_SNAPSHOT_MAX_ROWS: u32 = TERMINAL_MAX_ROWS;
/// Maximum spans one snapshot may carry across both row families.
pub const CELL_GRID_SNAPSHOT_MAX_SPANS: u32 = 65_536;
/// Maximum distinct hyperlink runs one snapshot may intern.
pub const CELL_GRID_SNAPSHOT_MAX_LINK_MAPPINGS: usize = 1_024;
/// How long a partial snapshot waits for its next part before it is stalled.
pub const CELL_GRID_CHUNK_STALL_MS: u64 = 10_000;
/// Largest coordinator fanout stamp, and the room its encoding must leave.
pub const CELL_GRID_COORD_FANOUT_STAMP_MAX: u64 = u64::MAX;
pub const CELL_GRID_COORD_FANOUT_STAMP_MAX_ENCODED_BYTES: u32 = 12;

/// Why a snapshot part was refused. Every member is a contract string:
/// `protocol/conformance/cell-chunks/*.json` names outcomes as `error:<code>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellGridChunkErrorCode {
    InvalidSnapshotId,
    InvalidStreamId,
    MissingPart,
    InvalidFull,
    InvalidGeometry,
    InvalidSequence,
    ChunkCount,
    ChunkIndex,
    ChunkOrder,
    ChunkSize,
    SnapshotSize,
    SnapshotStalled,
    MetadataMismatch,
    RowIndex,
    DuplicateRow,
    MissingRow,
    SpanLimit,
    LinkLimit,
    LinkConflict,
    SingleRowOversize,
}

impl CellGridChunkErrorCode {
    /// The contract string, byte-for-byte as the conformance vectors spell it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidSnapshotId => "invalid-snapshot-id",
            Self::InvalidStreamId => "invalid-stream-id",
            Self::MissingPart => "missing-part",
            Self::InvalidFull => "invalid-full",
            Self::InvalidGeometry => "invalid-geometry",
            Self::InvalidSequence => "invalid-sequence",
            Self::ChunkCount => "chunk-count",
            Self::ChunkIndex => "chunk-index",
            Self::ChunkOrder => "chunk-order",
            Self::ChunkSize => "chunk-size",
            Self::SnapshotSize => "snapshot-size",
            Self::SnapshotStalled => "snapshot-stalled",
            Self::MetadataMismatch => "metadata-mismatch",
            Self::RowIndex => "row-index",
            Self::DuplicateRow => "duplicate-row",
            Self::MissingRow => "missing-row",
            Self::SpanLimit => "span-limit",
            Self::LinkLimit => "link-limit",
            Self::LinkConflict => "link-conflict",
            Self::SingleRowOversize => "single-row-oversize",
        }
    }
}

impl fmt::Display for CellGridChunkErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A refused snapshot part. The code is the contract outcome a caller branches
/// on; the reason is the diagnosis that goes straight into a log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellGridChunkError {
    pub code: Code,
    pub reason: String,
}

impl CellGridChunkError {
    /// Build the error a rejected part reports, so a caller reading the code
    /// never has to know which layer noticed the fault.
    pub fn new(code: Code, reason: impl Into<String>) -> Self {
        Self {
            code,
            reason: reason.into(),
        }
    }
}

impl fmt::Display for CellGridChunkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.reason)
    }
}

impl std::error::Error for CellGridChunkError {}

/// Build the error a rejected part reports, keeping every rejection site on one shape.
pub fn reject_cell_grid_chunk(code: Code, reason: impl Into<String>) -> CellGridChunkError {
    CellGridChunkError::new(code, reason)
}

/// Validate the scalar half of one part, before any row is compared across parts.
pub fn assert_snapshot_scalars(frame: &PbCellGridFrame) -> Result<(), CellGridChunkError> {
    if !frame.full || frame.base_seq != 0 {
        return Err(reject(
            Code::InvalidFull,
            "snapshot part must be a full frame with base_seq=0",
        ));
    }
    if !is_terminal_uuid(&frame.stream_id) {
        return Err(reject(
            Code::InvalidStreamId,
            format!(
                "snapshot stream_id is not a UUID: {}",
                quoted(&frame.stream_id)
            ),
        ));
    }
    if frame.grid_epoch.is_empty() {
        return Err(reject(Code::InvalidFull, "snapshot grid_epoch is empty"));
    }
    let geometry = TerminalGeometry {
        cols: frame.cols,
        rows: frame.rows,
    };
    if !is_terminal_geometry(&geometry) {
        return Err(reject(
            Code::InvalidGeometry,
            format!(
                "snapshot geometry {}x{} is outside 1..{TERMINAL_MAX_COLS}x1..{TERMINAL_MAX_ROWS}",
                frame.cols, frame.rows
            ),
        ));
    }
    if frame.seq < 1 {
        return Err(reject(
            Code::InvalidSequence,
            format!("snapshot seq must be positive, got {}", frame.seq),
        ));
    }
    if !frame.scrollback_append.is_empty() {
        return Err(reject(
            Code::InvalidFull,
            "authoritative snapshot parts cannot carry scrollback append",
        ));
    }
    if frame.sb_base > frame.scrollback_total {
        return Err(reject(
            Code::InvalidFull,
            "authoritative snapshot sb_base is outside its history",
        ));
    }
    Ok(())
}

/// One interned hyperlink run. `key` is the core's RUN identity: a key that maps
/// to two URIs is a conflict, because a renderer groups a wrapped link by it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellGridLinkMapping {
    pub key: String,
    pub uri: String,
}

/// What one complete authoritative snapshot proved about its own cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellGridSnapshotStats {
    pub spans: u32,
    pub link_mappings: usize,
}

/// The span and next-index state a history walk carries across parts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotHistoryRows {
    pub spans: u32,
    pub next_index: u64,
}

/// Add viewport rows: each index occurs exactly once across the snapshot, and
/// each row is bounded by the current grid width.
pub fn add_snapshot_rows(
    frame: &PbCellGridFrame,
    rows: &[PbCellRow],
    seen_rows: &mut HashSet<u32>,
    links: &mut HashMap<String, CellGridLinkMapping>,
    starting_spans: u32,
) -> Result<u32, CellGridChunkError> {
    let mut spans = starting_spans;
    for row in rows {
        if row.index >= frame.rows {
            return Err(reject(
                Code::RowIndex,
                format!(
                    "viewport row {} is outside 0..{}",
                    row.index,
                    frame.rows - 1
                ),
            ));
        }
        if !seen_rows.insert(row.index) {
            return Err(reject(
                Code::DuplicateRow,
                format!("viewport row {} occurs more than once", row.index),
            ));
        }
        assert_part_row_spans(row, frame.cols)?;
        spans = account_spans(spans, row)?;
        record_span_links(row, links)?;
    }
    Ok(spans)
}

/// Add retained history rows, which are dense and ordered: the first continues
/// at `next_index` and each sits below `scrollback_total`.
pub fn add_snapshot_history_rows(
    frame: &PbCellGridFrame,
    rows: &[PbCellRow],
    next_index: u64,
    links: &mut HashMap<String, CellGridLinkMapping>,
    starting_spans: u32,
) -> Result<SnapshotHistoryRows, CellGridChunkError> {
    let mut spans = starting_spans;
    let mut next = next_index;
    for row in rows {
        if u64::from(row.index) != next || u64::from(row.index) >= frame.scrollback_total {
            return Err(reject(
                Code::RowIndex,
                format!("history row {} does not continue at {next}", row.index),
            ));
        }
        // A retained line keeps its write-time width, so the current grid does
        // not bound it; only the row's own spans can be wrong.
        assert_part_row_spans(row, 0)?;
        spans = account_spans(spans, row)?;
        record_span_links(row, links)?;
        next += 1;
    }
    Ok(SnapshotHistoryRows {
        spans,
        next_index: next,
    })
}

/// Validate one complete authoritative viewport snapshot, whole or assembled.
pub fn assert_cell_grid_snapshot(
    frame: &PbCellGridFrame,
) -> Result<CellGridSnapshotStats, CellGridChunkError> {
    assert_snapshot_scalars(frame)?;
    if frame.rows > CELL_GRID_SNAPSHOT_MAX_ROWS {
        return Err(reject(
            Code::InvalidGeometry,
            format!("snapshot has more than {CELL_GRID_SNAPSHOT_MAX_ROWS} rows"),
        ));
    }
    let mut seen_rows = HashSet::new();
    let mut links = HashMap::new();
    let spans = add_snapshot_rows(frame, &frame.viewport_rows, &mut seen_rows, &mut links, 0)?;
    let history = add_snapshot_history_rows(
        frame,
        &frame.scrollback_rows,
        frame.sb_base,
        &mut links,
        spans,
    )?;
    if history.next_index != frame.scrollback_total {
        return Err(reject(
            Code::MissingRow,
            "snapshot history does not reach scrollback_total",
        ));
    }
    if seen_rows.len() != frame.rows as usize {
        return Err(reject(
            Code::MissingRow,
            format!(
                "snapshot has {} of {} required viewport rows",
                seen_rows.len(),
                frame.rows
            ),
        ));
    }
    Ok(CellGridSnapshotStats {
        spans: history.spans,
        link_mappings: links.len(),
    })
}

/// True when two parts describe the same grid at the same point in the stream.
/// The rows are excluded: a part carries whole rows and nothing else.
pub fn has_same_snapshot_metadata(a: &PbCellGridFrame, b: &PbCellGridFrame) -> bool {
    a.session_id == b.session_id
        && a.stream_id == b.stream_id
        && a.grid_epoch == b.grid_epoch
        && a.cols == b.cols
        && a.rows == b.rows
        && a.cursor_row == b.cursor_row
        && a.cursor_col == b.cursor_col
        && a.cursor_visible == b.cursor_visible
        && a.alt_screen == b.alt_screen
        && a.full == b.full
        && a.scrollback_total == b.scrollback_total
        && a.seq == b.seq
        && a.sb_base == b.sb_base
        && a.base_seq == b.base_seq
        && a.cursor_keys_app == b.cursor_keys_app
        && a.bracketed_paste == b.bracketed_paste
        && a.mouse_tracking == b.mouse_tracking
        && a.mouse_sgr == b.mouse_sgr
        && a.focus_events == b.focus_events
        && a.pty_out_ms == b.pty_out_ms
        && a.worker_emit_ms == b.worker_emit_ms
        && a.coord_recv_ms == b.coord_recv_ms
        && a.coord_fanout_ms == b.coord_fanout_ms
}

/// A value as the wire spells a quoted string, for a reason that names it.
pub fn quoted(value: &str) -> String {
    serde_json::Value::String(value.to_owned()).to_string()
}
