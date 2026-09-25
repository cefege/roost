//! The cell and grid value model: a span, a row, a whole frame, the geometry
//! helpers that relate text to terminal columns, and the chunked snapshot
//! protocol that ships one over the wire.
//!
//! `types` is the foundation and the only place terminal-column arithmetic
//! lives; `diff_grid` and `delta_batch` consume it; the `frame_chunk_*` family
//! and `proto` turn a frame into wire bytes and back.

pub mod delta_batch;
pub mod diff_grid;
pub mod frame_chunk_assembler;
pub mod frame_chunk_validation;
pub mod frame_chunks;
pub mod proto;
pub mod types;

pub use delta_batch::{CellDeltaBatch, fold_cell_delta_batch};
pub use diff_grid::{
    apply_delta, clone_cell_grid_frame, delta_viewport_shift, normalize_cell_grid_frame,
};
pub use frame_chunk_assembler::{
    CellGridChunkAssembler, CellGridChunkAssembly, CellGridSnapshotProgress,
};
pub use frame_chunk_validation::{
    CELL_GRID_CHUNK_STALL_MS, CELL_GRID_PART_MAX_BYTES, CELL_GRID_SNAPSHOT_MAX_BYTES,
    CELL_GRID_SNAPSHOT_MAX_CHUNKS, CELL_GRID_SNAPSHOT_MAX_LINK_MAPPINGS,
    CELL_GRID_SNAPSHOT_MAX_ROWS, CELL_GRID_SNAPSHOT_MAX_SPANS, CellGridChunkError,
    CellGridChunkErrorCode,
};
pub use frame_chunks::{chunk_cell_grid_frame, create_cell_grid_snapshot_source};
pub use proto::{
    cell_frame_to_proto, cell_row_from_proto, cell_row_from_proto_bounded, cell_row_to_proto,
    proto_to_cell_frame,
};
pub use types::{
    CELL_BLINK, CELL_BOLD, CELL_DIM, CELL_INVISIBLE, CELL_ITALIC, CELL_REVERSE, CELL_STRIKE,
    CELL_UNDERLINE, CellGridFrame, CellRow, CellSpan, ColumnRange, DEFAULT_COLOR,
    MAX_LINK_URI_BYTES, MouseTracking, SB_SNAPSHOT_HISTORY_ROWS, as_mouse_tracking,
    assert_cell_row_spans, column_span, column_text, link_uri_within_cap, row_columns,
    span_is_atomic, spans_text, text_offset_to_column, text_offset_to_column_end,
    text_range_to_columns,
};
