//! The byte arithmetic behind one snapshot's part plan: what a row costs inside
//! a part, what the repeated header costs, and where the whole-row boundaries
//! fall.
//!
//! Every number here is buffa's own encoded length, measured through the real
//! encoder rather than estimated: a second size arithmetic is how one side
//! builds a part the other calls oversized. The parent owns the public
//! planning surface and materialising a cursor's part.

use roost_proto::buffa::Message;
use roost_proto::buffa::encoding::varint_len;
use roost_proto::{PbCellGridChunk, PbCellGridFrame, PbCellRow};

use super::create_cell_grid_frame_part;
use super::{encoded_cell_grid_chunk_size, encoded_cell_grid_frame_size};
use crate::cell::frame_chunk_validation::CELL_GRID_PART_MAX_BYTES;
use crate::cell::frame_chunk_validation::CELL_GRID_SNAPSHOT_MAX_BYTES;
use crate::cell::frame_chunk_validation::CELL_GRID_SNAPSHOT_MAX_CHUNKS;
use crate::cell::frame_chunk_validation::CellGridChunkError;
use crate::cell::frame_chunk_validation::CellGridChunkErrorCode as Code;
use crate::cell::frame_chunk_validation::reject_cell_grid_chunk as reject;

/// The identity the planner measures against: a UUID is always 36 bytes.
const PLANNING_SNAPSHOT_ID: &str = "00000000-0000-4000-8000-000000000000";

/// A row's field tag is one byte: fields 9 and 10 both tag in one byte.
const ROW_FIELD_TAG_BYTES: u32 = 1;

/// One row of a plan: its family and the grid index it answers to.
///
/// The index, not the position, is the identity. A snapshot's viewport rows
/// are addressed by `0..rows` and its history rows by `sb_base + offset`, and a
/// row can arrive at a different position in the vector than the position it
/// occupies in the grid. Looking up by index is what makes a row that is
/// missing a refusal rather than a wrong row silently shipped.
#[derive(Debug, Clone, Copy)]
pub(super) struct SnapshotEntry {
    pub(super) history: bool,
    pub(super) index: u32,
}

/// One planned part and the encoded part size it was planned against.
#[derive(Debug, Clone)]
pub(super) struct PlannedSnapshotPart {
    pub(super) entries: Vec<SnapshotEntry>,
    pub(super) encoded_part_bytes: u32,
}

impl SnapshotEntry {
    pub(super) fn family(&self) -> &'static str {
        if self.history { "history" } else { "viewport" }
    }

    /// The row this entry names in the source frame, or a refusal.
    ///
    /// A miss means the frame changed between validation and planning, which
    /// is not something the wire can do — so it is reported, never assumed.
    pub(super) fn row<'a>(
        &self,
        frame: &'a PbCellGridFrame,
    ) -> Result<&'a PbCellRow, CellGridChunkError> {
        let rows = if self.history {
            &frame.scrollback_rows
        } else {
            &frame.viewport_rows
        };
        rows.iter()
            .find(|row| row.index == self.index)
            .ok_or_else(|| {
                let reason = format!(
                    "validated cell snapshot {} row {} changed during planning",
                    self.family(),
                    self.index
                );
                reject(Code::MissingRow, reason)
            })
    }
}

/// The frame's rows in part order: retained history first, then the viewport.
/// `assert_cell_grid_snapshot` ran immediately above and proved history is dense
/// from `sb_base` and the viewport covers `0..rows-1` once, so each row's
/// POSITION is its index and the walk needs no lookup.
fn snapshot_entries(frame: &PbCellGridFrame) -> Vec<SnapshotEntry> {
    let mut entries = Vec::with_capacity(frame.scrollback_rows.len() + frame.rows as usize);
    for offset in 0..frame.scrollback_rows.len() {
        entries.push(SnapshotEntry {
            history: true,
            // `sb_base` is a u64 line number and a row index is a u32; a
            // snapshot whose history base cannot be a row index is refused by
            // the snapshot check long before the planner reads it.
            index: u32::try_from(frame.sb_base.saturating_add(offset as u64)).unwrap_or(u32::MAX),
        });
    }
    for index in 0..frame.rows {
        entries.push(SnapshotEntry {
            history: false,
            index,
        });
    }
    entries
}

/// One row's whole cost inside a part: its tag, its length prefix, its bytes.
fn row_entry_bytes(row: &PbCellRow) -> u32 {
    let row_bytes = u64::from(row.encoded_len());
    ROW_FIELD_TAG_BYTES + varint_len(row_bytes) as u32 + row_bytes as u32
}

/// The scalar part every chunk repeats, measured through the real encoder.
fn chunk_header_bytes(snapshot_id: &str, chunk_index: u32, chunk_count: u32) -> u32 {
    let header = PbCellGridChunk {
        snapshot_id: snapshot_id.to_owned(),
        chunk_index,
        chunk_count,
        part: roost_proto::buffa::MessageField::none(),
        __buffa_unknown_fields: Default::default(),
    };
    encoded_cell_grid_chunk_size(&header)
}

/// The part field's tag, measured rather than assumed: a chunk holding the empty
/// part, minus its header, its length prefix and its bytes, is the tag.
fn part_field_tag_bytes(empty_part: &PbCellGridFrame, empty_bytes: u32, header: u32) -> u32 {
    let probe = PbCellGridChunk {
        snapshot_id: PLANNING_SNAPSHOT_ID.to_owned(),
        chunk_index: CELL_GRID_SNAPSHOT_MAX_CHUNKS - 1,
        chunk_count: CELL_GRID_SNAPSHOT_MAX_CHUNKS,
        part: roost_proto::buffa::MessageField::some(empty_part.clone()),
        __buffa_unknown_fields: Default::default(),
    };
    let prefix = varint_len(u64::from(empty_bytes)) as u32;
    encoded_cell_grid_chunk_size(&probe) - header - prefix - empty_bytes
}

/// One chunk's whole cost: header, part tag, the part's length prefix, the part.
fn encoded_chunk_bytes(header_bytes: u32, part_tag_bytes: u32, encoded_part_bytes: u32) -> u32 {
    let prefix = varint_len(u64::from(encoded_part_bytes)) as u32;
    header_bytes + part_tag_bytes + prefix + encoded_part_bytes
}

/// Greedily fill parts with whole rows, never splitting one. The budget is
/// measured against the widest header any chunk can have, so a recipient's own
/// snapshot UUID cannot push a planned part over the limit.
pub(super) fn plan_snapshot_parts(
    frame: &PbCellGridFrame,
) -> Result<Vec<PlannedSnapshotPart>, CellGridChunkError> {
    let empty_part = create_cell_grid_frame_part(frame, &[], &[]);
    let base_part_bytes = encoded_cell_grid_frame_size(&empty_part);
    let max_chunks = CELL_GRID_SNAPSHOT_MAX_CHUNKS;
    let largest_header_bytes = chunk_header_bytes(PLANNING_SNAPSHOT_ID, max_chunks - 1, max_chunks);
    let tag_bytes = part_field_tag_bytes(&empty_part, base_part_bytes, largest_header_bytes);
    let mut parts: Vec<PlannedSnapshotPart> = Vec::new();
    let mut entries: Vec<SnapshotEntry> = Vec::new();
    let mut encoded_part_bytes = base_part_bytes;

    for entry in snapshot_entries(frame) {
        let row = entry.row(frame)?;
        let contribution = row_entry_bytes(row);
        let next_part_bytes = encoded_part_bytes + contribution;
        let next_bytes = encoded_chunk_bytes(largest_header_bytes, tag_bytes, next_part_bytes);
        if next_bytes <= CELL_GRID_PART_MAX_BYTES {
            entries.push(entry);
            encoded_part_bytes = next_part_bytes;
            continue;
        }
        if entries.is_empty() {
            return Err(reject(Code::SingleRowOversize, row_oversize_reason(entry)));
        }
        parts.push(PlannedSnapshotPart {
            entries,
            encoded_part_bytes,
        });
        entries = vec![entry];
        encoded_part_bytes = base_part_bytes + contribution;
        let row_bytes = encoded_chunk_bytes(largest_header_bytes, tag_bytes, encoded_part_bytes);
        if row_bytes > CELL_GRID_PART_MAX_BYTES {
            return Err(reject(Code::SingleRowOversize, row_oversize_reason(entry)));
        }
    }
    if !entries.is_empty() {
        parts.push(PlannedSnapshotPart {
            entries,
            encoded_part_bytes,
        });
    }
    if parts.is_empty() || parts.len() > CELL_GRID_SNAPSHOT_MAX_CHUNKS as usize {
        let ceiling = CELL_GRID_SNAPSHOT_MAX_CHUNKS;
        let reason = format!(
            "cell snapshot requires {} chunks; maximum is {ceiling}",
            parts.len()
        );
        return Err(reject(Code::ChunkCount, reason));
    }
    // Each part is measured under the header its own index and the plan's own
    // length will cost, not under the widest header the budget reserved.
    let chunk_count = u32::try_from(parts.len()).unwrap_or(u32::MAX);
    let total_bytes: u64 = parts
        .iter()
        .enumerate()
        .map(|(index, part)| {
            let header = chunk_header_bytes(PLANNING_SNAPSHOT_ID, index as u32, chunk_count);
            u64::from(encoded_chunk_bytes(
                header,
                tag_bytes,
                part.encoded_part_bytes,
            ))
        })
        .sum();
    if total_bytes > CELL_GRID_SNAPSHOT_MAX_BYTES {
        let ceiling = CELL_GRID_SNAPSHOT_MAX_BYTES;
        let reason = format!("cell snapshot is {total_bytes} encoded bytes; maximum is {ceiling}");
        return Err(reject(Code::SnapshotSize, reason));
    }
    Ok(parts)
}

/// Why one row cannot share a part, and so cannot be split either.
fn row_oversize_reason(entry: SnapshotEntry) -> String {
    format!(
        "cell snapshot {} row {} cannot fit in {CELL_GRID_PART_MAX_BYTES} bytes",
        entry.family(),
        entry.index
    )
}
