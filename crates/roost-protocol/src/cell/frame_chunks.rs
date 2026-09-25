//! Planning, materialising and sizing the bounded parts of one cell snapshot.
//!
//! A coordinator that retains a canonical full materialises only the part a
//! recipient asks for; a worker or browser may take every deterministic chunk at
//! once. Both go through the one plan computed here, so two recipients of one
//! source always agree on where the row boundaries are. The byte arithmetic
//! that plan is measured with is in `plan`.
//!
//! Every size is buffa's own encoded length: the 1 MiB part limit and the
//! 64 MiB snapshot limit ARE canonical protobuf byte counts, and a second size
//! arithmetic is how one side builds a part the other calls oversized.
mod plan;

use std::fmt;

use roost_proto::buffa::Message;
use roost_proto::{PbCellGridChunk, PbCellGridFrame, PbCellRow};

use self::plan::{PlannedSnapshotPart, plan_snapshot_parts};
use crate::cell::frame_chunk_validation::CELL_GRID_PART_MAX_BYTES;
use crate::cell::frame_chunk_validation::CellGridChunkErrorCode as Code;
use crate::cell::frame_chunk_validation::reject_cell_grid_chunk as reject;
use crate::cell::frame_chunk_validation::{CellGridChunkError, assert_cell_grid_snapshot, quoted};
use crate::viewport::is_terminal_uuid;

/// Canonical encoded size of one frame, in bytes.
pub fn encoded_cell_grid_frame_size(frame: &PbCellGridFrame) -> u32 {
    frame.encoded_len()
}

/// Canonical encoded size of one chunk, in bytes.
pub fn encoded_cell_grid_chunk_size(chunk: &PbCellGridChunk) -> u32 {
    chunk.encoded_len()
}

/// Why a part is over the byte limit.
fn chunk_size_reason(encoded_bytes: u32) -> String {
    let ceiling = CELL_GRID_PART_MAX_BYTES;
    format!("cell snapshot chunk is {encoded_bytes} bytes; maximum is {ceiling}")
}

/// Encode one chunk, refusing anything over the part limit before a byte is made.
pub fn encode_cell_grid_chunk(chunk: &PbCellGridChunk) -> Result<Vec<u8>, CellGridChunkError> {
    let encoded_bytes = encoded_cell_grid_chunk_size(chunk);
    if encoded_bytes > CELL_GRID_PART_MAX_BYTES {
        return Err(reject(Code::ChunkSize, chunk_size_reason(encoded_bytes)));
    }
    Ok(chunk.encode_to_vec())
}

/// A chunk that could not become bytes: refused by the contract, or not protobuf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellGridChunkDecodeError {
    Contract(CellGridChunkError),
    Wire(roost_proto::buffa::DecodeError),
}

impl fmt::Display for CellGridChunkDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Contract(error) => write!(formatter, "{error}"),
            Self::Wire(error) => write!(formatter, "cell snapshot chunk is not protobuf: {error}"),
        }
    }
}

impl std::error::Error for CellGridChunkDecodeError {}

/// Decode one chunk, refusing an oversized buffer before it is parsed.
pub fn decode_cell_grid_chunk(encoded: &[u8]) -> Result<PbCellGridChunk, CellGridChunkDecodeError> {
    if encoded.len() > CELL_GRID_PART_MAX_BYTES as usize {
        let reason = chunk_size_reason(encoded.len() as u32);
        return Err(CellGridChunkDecodeError::Contract(reject(
            Code::ChunkSize,
            reason,
        )));
    }
    PbCellGridChunk::decode_from_slice(encoded).map_err(CellGridChunkDecodeError::Wire)
}

/// One part: the source frame's whole scalar metadata plus exactly the rows
/// this part owns, with unknown fields dropped.
pub fn create_cell_grid_frame_part(
    frame: &PbCellGridFrame,
    rows: &[PbCellRow],
    scrollback_rows: &[PbCellRow],
) -> PbCellGridFrame {
    let mut part = frame.clone();
    part.viewport_rows = rows.to_vec();
    part.scrollback_rows = scrollback_rows.to_vec();
    part.scrollback_append = Vec::new();
    part.__buffa_unknown_fields = Default::default();
    part
}

/// What a cursor hands back for one index: the whole frame, or a chunk.
#[derive(Debug, Clone, PartialEq)]
pub enum CellGridSnapshotPart {
    Frame(PbCellGridFrame),
    Chunk(PbCellGridChunk),
}

/// One validated immutable full plus its part plan. A cursor owns its snapshot
/// UUID, so many recipients can stream the same canonical source.
#[derive(Debug, Clone)]
pub struct CellGridSnapshotSource {
    frame: PbCellGridFrame,
    /// `None` when the whole frame fitted inside one part.
    parts: Option<Vec<PlannedSnapshotPart>>,
}

impl CellGridSnapshotSource {
    /// How many parts every cursor of this source walks.
    pub fn part_count(&self) -> u32 {
        match &self.parts {
            Some(parts) => u32::try_from(parts.len()).unwrap_or(u32::MAX),
            None => 1,
        }
    }

    /// One recipient's view of the source, addressed by its own snapshot UUID.
    pub fn create_cursor(
        &self,
        snapshot_id: &str,
    ) -> Result<CellGridSnapshotCursor<'_>, CellGridChunkError> {
        if self.parts.is_some() && !is_terminal_uuid(snapshot_id) {
            let reason = format!("cell snapshot_id is not a UUID: {}", quoted(snapshot_id));
            return Err(reject(Code::InvalidSnapshotId, reason));
        }
        Ok(CellGridSnapshotCursor {
            source: self,
            snapshot_id: snapshot_id.to_owned(),
            part_count: self.part_count(),
        })
    }
}

/// One recipient's cursor: the same plan, stamped with one snapshot UUID.
#[derive(Debug, Clone)]
pub struct CellGridSnapshotCursor<'a> {
    source: &'a CellGridSnapshotSource,
    snapshot_id: String,
    part_count: u32,
}

impl CellGridSnapshotCursor<'_> {
    pub fn part_count(&self) -> u32 {
        self.part_count
    }

    /// Materialise one part. The plan is fixed, so the same index twice is the
    /// same part.
    pub fn materialize(&self, part_index: u32) -> Result<CellGridSnapshotPart, CellGridChunkError> {
        if part_index >= self.part_count {
            let last = self.part_count - 1;
            let reason = format!("cell snapshot part index {part_index} is outside 0..{last}");
            return Err(reject(Code::ChunkIndex, reason));
        }
        let Some(parts) = &self.source.parts else {
            return Ok(CellGridSnapshotPart::Frame(self.source.frame.clone()));
        };
        let mut viewport_rows = Vec::new();
        let mut scrollback_rows = Vec::new();
        for entry in &parts[part_index as usize].entries {
            let row = entry.row(&self.source.frame)?;
            if entry.history {
                scrollback_rows.push(row.clone());
            } else {
                viewport_rows.push(row.clone());
            }
        }
        let frame = &self.source.frame;
        let part = create_cell_grid_frame_part(frame, &viewport_rows, &scrollback_rows);
        let chunk = PbCellGridChunk {
            snapshot_id: self.snapshot_id.clone(),
            chunk_index: part_index,
            chunk_count: self.part_count,
            part: roost_proto::buffa::MessageField::some(part),
            __buffa_unknown_fields: Default::default(),
        };
        let encoded_bytes = encoded_cell_grid_chunk_size(&chunk);
        if encoded_bytes > CELL_GRID_PART_MAX_BYTES {
            let reason = format!("cell snapshot chunk {part_index} is {encoded_bytes} bytes");
            return Err(reject(Code::ChunkSize, reason));
        }
        Ok(CellGridSnapshotPart::Chunk(chunk))
    }
}

/// Validate once, plan whole-row boundaries once, then materialise only a
/// cursor's part. `force_chunking` keeps a small full as one frame.
pub fn create_cell_grid_snapshot_source(
    frame: &PbCellGridFrame,
    force_chunking: bool,
) -> Result<CellGridSnapshotSource, CellGridChunkError> {
    assert_cell_grid_snapshot(frame)?;
    if !force_chunking && encoded_cell_grid_frame_size(frame) <= CELL_GRID_PART_MAX_BYTES {
        return Ok(CellGridSnapshotSource {
            frame: frame.clone(),
            parts: None,
        });
    }
    let parts = plan_snapshot_parts(frame)?;
    Ok(CellGridSnapshotSource {
        frame: frame.clone(),
        parts: Some(parts),
    })
}

/// Deterministically split a complete full into whole-row bounded chunks.
pub fn chunk_cell_grid_frame(
    frame: &PbCellGridFrame,
    snapshot_id: &str,
) -> Result<Vec<PbCellGridChunk>, CellGridChunkError> {
    let source = create_cell_grid_snapshot_source(frame, true)?;
    let cursor = source.create_cursor(snapshot_id)?;
    let mut chunks = Vec::with_capacity(cursor.part_count() as usize);
    for part_index in 0..cursor.part_count() {
        match cursor.materialize(part_index)? {
            CellGridSnapshotPart::Chunk(chunk) => chunks.push(chunk),
            CellGridSnapshotPart::Frame(_) => {
                let reason = "a forced cell snapshot chunk was not chunked";
                return Err(reject(Code::ChunkCount, reason));
            }
        }
    }
    Ok(chunks)
}
