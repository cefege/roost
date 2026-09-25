//! Hand-built protobuf chunk fixtures for the chunked cell-snapshot tests.
//!
//! The messages are assembled directly rather than run through an encoder, so
//! each rule in the contract can be violated in isolation and the code that
//! reports it identified by name.

use roost_proto::{PbCellGridChunk, PbCellGridFrame, PbCellRow, PbCellSpan};
use roost_protocol::cell::frame_chunk_assembler::{CellGridChunkAssembler, CellGridChunkAssembly};
use roost_protocol::cell::frame_chunk_validation::CellGridChunkErrorCode;
use roost_protocol::cell::frame_chunks::create_cell_grid_frame_part;

pub const STREAM: &str = "00000000-0000-4000-8000-000000000001";
pub const OTHER_STREAM: &str = "00000000-0000-4000-8000-000000000002";
pub const SNAPSHOT: &str = "00000000-0000-4000-8000-000000000011";
pub const REPLACEMENT: &str = "00000000-0000-4000-8000-000000000012";

pub fn span(text: &str) -> PbCellSpan {
    PbCellSpan {
        text: text.to_owned(),
        fg: 256,
        bg: 256,
        columns: 1,
        ..Default::default()
    }
}

pub fn row(index: u32, text: &str) -> PbCellRow {
    PbCellRow {
        index,
        spans: vec![span(text)],
        ..Default::default()
    }
}

/// A valid authoritative full: `rows` viewport rows of `r<index>`, no history.
pub fn frame(rows: u32) -> PbCellGridFrame {
    PbCellGridFrame {
        session_id: "s".to_owned(),
        stream_id: STREAM.to_owned(),
        grid_epoch: "g".to_owned(),
        cols: 8,
        rows,
        full: true,
        viewport_rows: (0..rows)
            .map(|index| row(index, &format!("r{index}")))
            .collect(),
        seq: 1,
        ..Default::default()
    }
}

pub fn chunk_with(
    source: &PbCellGridFrame,
    viewport: Vec<PbCellRow>,
    scrollback: Vec<PbCellRow>,
    index: u32,
    count: u32,
    snapshot_id: &str,
) -> PbCellGridChunk {
    let part = create_cell_grid_frame_part(source, &viewport, &scrollback);
    PbCellGridChunk {
        snapshot_id: snapshot_id.to_owned(),
        chunk_index: index,
        chunk_count: count,
        part: roost_proto::buffa::MessageField::some(part),
        ..Default::default()
    }
}

pub fn chunk_of(
    source: &PbCellGridFrame,
    viewport: Vec<PbCellRow>,
    index: u32,
    count: u32,
) -> PbCellGridChunk {
    chunk_with(source, viewport, Vec::new(), index, count, SNAPSHOT)
}

/// The rejection code a push reported, failing the test when it was accepted.
pub fn pushed_code(
    assembler: &mut CellGridChunkAssembler,
    chunk: &PbCellGridChunk,
    now_ms: u64,
) -> CellGridChunkErrorCode {
    match assembler.push(chunk, now_ms) {
        Ok(assembly) => panic!("expected a rejection, got {assembly:?}"),
        Err(error) => error.code,
    }
}

/// The rejection code one part alone draws, with nothing else in flight.
pub fn part_code(part: PbCellGridFrame) -> CellGridChunkErrorCode {
    let chunk = PbCellGridChunk {
        snapshot_id: SNAPSHOT.to_owned(),
        chunk_index: 0,
        chunk_count: 1,
        part: roost_proto::buffa::MessageField::some(part),
        ..Default::default()
    };
    pushed_code(&mut CellGridChunkAssembler::new(), &chunk, 0)
}

pub fn accepted(chunk: &PbCellGridChunk) -> PbCellGridFrame {
    let mut assembler = CellGridChunkAssembler::new();
    match assembler.push(chunk, 0) {
        Ok(CellGridChunkAssembly::Complete { frame, .. }) => frame,
        other => panic!("expected one complete snapshot, got {other:?}"),
    }
}
