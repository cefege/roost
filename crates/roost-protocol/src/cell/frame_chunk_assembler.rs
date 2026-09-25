//! One bounded, in-order, atomically completing cell snapshot assembly.
//!
//! A multi-megabyte full arrives as whole-row parts. The assembler keeps at
//! most one partial snapshot, admits parts in order, and hands back a frame only
//! once every viewport row has occurred exactly once and history has reached
//! `scrollback_total` — so a replica never installs a half-baseline that would
//! paint rows no frame ever described.
//!
//! The order the checks run in is the behaviour, not an implementation detail:
//! a part that breaks several rules always reports the earliest one, and EVERY
//! rejection drops the partial, because one malformed part must not poison every
//! snapshot after it. The clock is a parameter: this crate reads no time.

use std::collections::{HashMap, HashSet};

use roost_proto::{PbCellGridChunk, PbCellGridFrame, PbCellRow};

use crate::cell::frame_chunk_validation::CellGridChunkErrorCode as Code;
use crate::cell::frame_chunk_validation::reject_cell_grid_chunk as reject;
use crate::cell::frame_chunk_validation::{
    CELL_GRID_CHUNK_STALL_MS, CELL_GRID_PART_MAX_BYTES, CELL_GRID_SNAPSHOT_MAX_BYTES,
    CELL_GRID_SNAPSHOT_MAX_CHUNKS, CellGridChunkError, CellGridLinkMapping,
    add_snapshot_history_rows, add_snapshot_rows, assert_snapshot_scalars,
    has_same_snapshot_metadata, quoted,
};
use crate::cell::frame_chunks::{create_cell_grid_frame_part, encoded_cell_grid_chunk_size};
use crate::viewport::is_terminal_uuid;

/// What one accepted part did to the snapshot in flight.
// `Complete` carries the assembled frame and `Pending` does not. Boxing the
// frame would move the one allocation from the path that already did the work
// onto the path that only records progress.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum CellGridChunkAssembly {
    /// Accepted, and more parts are still owed.
    Pending {
        snapshot_id: String,
        next_chunk_index: u32,
    },
    /// The last part arrived and the frame is complete.
    Complete {
        snapshot_id: String,
        frame: PbCellGridFrame,
    },
}

/// How far an in-flight baseline has assembled. `None` whenever no partial
/// exists — idle, completed or reset — so a single-frame baseline never reports
/// progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellGridSnapshotProgress {
    pub snapshot_id: String,
    pub received_chunks: u32,
    pub total_chunks: u32,
}

/// Everything one partial snapshot has accumulated. Held by value and dropped
/// whole on any rejection, so nothing survives a refused part.
#[derive(Debug)]
struct PartialSnapshot {
    snapshot_id: String,
    stream_id: String,
    chunk_count: u32,
    next_chunk_index: u32,
    total_bytes: u64,
    last_chunk_at_ms: u64,
    /// The first part's scalars: every later part must match them exactly.
    metadata: PbCellGridFrame,
    /// Viewport row slots, filled as their index arrives.
    rows: Vec<Option<PbCellRow>>,
    history_rows: Vec<PbCellRow>,
    next_history_index: u64,
    seen_rows: HashSet<u32>,
    links: HashMap<String, CellGridLinkMapping>,
    spans: u32,
}

/// One bounded, in-order, atomically completing snapshot assembly.
#[derive(Debug, Default)]
pub struct CellGridChunkAssembler {
    partial: Option<PartialSnapshot>,
}

impl CellGridChunkAssembler {
    pub fn new() -> Self {
        Self { partial: None }
    }

    /// The snapshot currently being assembled, if any.
    pub fn active_snapshot_id(&self) -> Option<&str> {
        self.partial
            .as_ref()
            .map(|partial| partial.snapshot_id.as_str())
    }

    /// Attach-progress read for a coordinator handing a baseline to a viewer.
    pub fn snapshot_progress(&self) -> Option<CellGridSnapshotProgress> {
        self.partial
            .as_ref()
            .map(|partial| CellGridSnapshotProgress {
                snapshot_id: partial.snapshot_id.clone(),
                received_chunks: partial.next_chunk_index,
                total_chunks: partial.chunk_count,
            })
    }

    /// Abandon whatever is in flight, for a resync or a stream change.
    pub fn reset(&mut self) {
        self.partial = None;
    }

    /// Drop a partial at the shared transport-pressure stall boundary. False
    /// when idle or when the gap has not reached the boundary yet, so a caller
    /// that sweeps on a timer can tell "nothing to do" from "dropped".
    pub fn expire(&mut self, now_ms: u64) -> bool {
        let Some(partial) = &self.partial else {
            return false;
        };
        if now_ms.saturating_sub(partial.last_chunk_at_ms) < CELL_GRID_CHUNK_STALL_MS {
            return false;
        }
        self.reset();
        true
    }

    /// Admit one part. `now_ms` is the caller's clock, read once for both the
    /// stall boundary and the partial's own timestamp.
    pub fn push(
        &mut self,
        chunk: &PbCellGridChunk,
        now_ms: u64,
    ) -> Result<CellGridChunkAssembly, CellGridChunkError> {
        match self.push_checked(chunk, now_ms) {
            Ok(assembly) => Ok(assembly),
            Err(error) => {
                self.reset();
                Err(error)
            }
        }
    }

    /// The rejection order, one rule at a time.
    fn push_checked(
        &mut self,
        chunk: &PbCellGridChunk,
        now_ms: u64,
    ) -> Result<CellGridChunkAssembly, CellGridChunkError> {
        if !is_terminal_uuid(&chunk.snapshot_id) {
            let reason = format!(
                "cell snapshot_id is not a UUID: {}",
                quoted(&chunk.snapshot_id)
            );
            return Err(reject(Code::InvalidSnapshotId, reason));
        }
        if chunk.chunk_count < 1 || chunk.chunk_count > CELL_GRID_SNAPSHOT_MAX_CHUNKS {
            let reason = format!(
                "cell snapshot chunk_count {} is outside 1..{CELL_GRID_SNAPSHOT_MAX_CHUNKS}",
                chunk.chunk_count
            );
            return Err(reject(Code::ChunkCount, reason));
        }
        if chunk.chunk_index >= chunk.chunk_count {
            let reason = format!(
                "cell snapshot chunk_index {} is outside its chunk_count",
                chunk.chunk_index
            );
            return Err(reject(Code::ChunkIndex, reason));
        }
        let Some(part) = chunk.part.as_option() else {
            return Err(reject(Code::MissingPart, "cell snapshot chunk has no part"));
        };
        assert_snapshot_scalars(part)?;
        if part.viewport_rows.is_empty() && part.scrollback_rows.is_empty() {
            return Err(reject(
                Code::MissingRow,
                "cell snapshot chunk carries no row",
            ));
        }
        let encoded_bytes = u64::from(encoded_cell_grid_chunk_size(chunk));
        if encoded_bytes > u64::from(CELL_GRID_PART_MAX_BYTES) {
            let ceiling = CELL_GRID_PART_MAX_BYTES;
            let reason = format!("cell snapshot chunk is {encoded_bytes} bytes; max {ceiling}");
            return Err(reject(Code::ChunkSize, reason));
        }

        if let Some(partial) = &self.partial
            && now_ms.saturating_sub(partial.last_chunk_at_ms) >= CELL_GRID_CHUNK_STALL_MS
        {
            let reason = format!(
                "cell snapshot {} stalled between chunks",
                partial.snapshot_id
            );
            return Err(reject(Code::SnapshotStalled, reason));
        }
        if let Some(partial) = &self.partial
            && partial.snapshot_id != chunk.snapshot_id
        {
            if chunk.chunk_index != 0 {
                let reason = "a replacement cell snapshot must start at chunk_index=0";
                return Err(reject(Code::ChunkOrder, reason));
            }
            if partial.stream_id != part.stream_id {
                let reason = "a partial snapshot can only be replaced within the same stream";
                return Err(reject(Code::MetadataMismatch, reason));
            }
            self.reset();
        }
        if self.partial.is_none() {
            if chunk.chunk_index != 0 {
                return Err(reject(
                    Code::ChunkOrder,
                    "cell snapshot must start at chunk_index=0",
                ));
            }
            self.partial = Some(PartialSnapshot::new(chunk, part, now_ms));
        }
        let Some(partial) = self.partial.as_mut() else {
            return Err(reject(
                Code::ChunkOrder,
                "cell snapshot must start at chunk_index=0",
            ));
        };
        let assembly = Self::admit(partial, chunk, part, encoded_bytes, now_ms)?;
        if matches!(assembly, CellGridChunkAssembly::Complete { .. }) {
            self.partial = None;
        }
        Ok(assembly)
    }

    /// Everything that needs the partial to exist: the part's agreement with
    /// the first one, its position in the order, the snapshot byte budget, and
    /// then the rows themselves.
    fn admit(
        partial: &mut PartialSnapshot,
        chunk: &PbCellGridChunk,
        part: &PbCellGridFrame,
        encoded_bytes: u64,
        now_ms: u64,
    ) -> Result<CellGridChunkAssembly, CellGridChunkError> {
        if chunk.chunk_count != partial.chunk_count {
            let reason = "cell snapshot chunk_count changed during assembly";
            return Err(reject(Code::ChunkCount, reason));
        }
        if !has_same_snapshot_metadata(&partial.metadata, part) {
            return Err(reject(
                Code::MetadataMismatch,
                "cell snapshot scalar metadata changed between chunks",
            ));
        }
        if chunk.chunk_index != partial.next_chunk_index {
            let expected = partial.next_chunk_index;
            let reason = format!("cell snapshot expected chunk_index={expected}");
            return Err(reject(Code::ChunkOrder, reason));
        }
        if partial.total_bytes + encoded_bytes > CELL_GRID_SNAPSHOT_MAX_BYTES {
            let ceiling = CELL_GRID_SNAPSHOT_MAX_BYTES;
            let reason = format!("cell snapshot exceeds {ceiling} encoded bytes");
            return Err(reject(Code::SnapshotSize, reason));
        }

        // History accumulates first and viewport rows second, so a part's two
        // row families are charged against the span cap in a fixed order.
        let history = add_snapshot_history_rows(
            part,
            &part.scrollback_rows,
            partial.next_history_index,
            &mut partial.links,
            partial.spans,
        )?;
        partial.spans = history.spans;
        partial.next_history_index = history.next_index;
        partial
            .history_rows
            .extend(part.scrollback_rows.iter().cloned());
        partial.spans = add_snapshot_rows(
            part,
            &part.viewport_rows,
            &mut partial.seen_rows,
            &mut partial.links,
            partial.spans,
        )?;
        for row in &part.viewport_rows {
            partial.rows[row.index as usize] = Some(row.clone());
        }
        partial.total_bytes += encoded_bytes;
        partial.next_chunk_index += 1;
        partial.last_chunk_at_ms = now_ms;

        if partial.next_chunk_index < partial.chunk_count {
            let snapshot_id = partial.snapshot_id.clone();
            let next_chunk_index = partial.next_chunk_index;
            return Ok(CellGridChunkAssembly::Pending {
                snapshot_id,
                next_chunk_index,
            });
        }
        Ok(CellGridChunkAssembly::Complete {
            snapshot_id: partial.snapshot_id.clone(),
            frame: completed_frame(partial)?,
        })
    }
}

/// The last part arrived. The snapshot must be whole before it is handed back,
/// and a frame that is missing a row is refused rather than painted.
fn completed_frame(partial: &mut PartialSnapshot) -> Result<PbCellGridFrame, CellGridChunkError> {
    if partial.seen_rows.len() != partial.rows.len() {
        let reason = format!(
            "cell snapshot has {} of {} required viewport rows",
            partial.seen_rows.len(),
            partial.rows.len()
        );
        return Err(reject(Code::MissingRow, reason));
    }
    if partial.next_history_index != partial.metadata.scrollback_total {
        return Err(reject(
            Code::MissingRow,
            "cell snapshot history does not reach scrollback_total",
        ));
    }
    let mut rows = Vec::with_capacity(partial.rows.len());
    for (index, row) in partial.rows.iter().enumerate() {
        let Some(row) = row else {
            let reason = format!("cell snapshot is missing viewport row {index}");
            return Err(reject(Code::MissingRow, reason));
        };
        rows.push(row.clone());
    }
    Ok(create_cell_grid_frame_part(
        &partial.metadata,
        &rows,
        &partial.history_rows,
    ))
}

impl PartialSnapshot {
    /// The state a first part establishes: its scalars, an empty row table the
    /// size of the grid it describes, and history positioned at `sb_base`.
    fn new(chunk: &PbCellGridChunk, part: &PbCellGridFrame, now_ms: u64) -> Self {
        Self {
            snapshot_id: chunk.snapshot_id.clone(),
            stream_id: part.stream_id.clone(),
            chunk_count: chunk.chunk_count,
            next_chunk_index: 0,
            total_bytes: 0,
            last_chunk_at_ms: now_ms,
            metadata: create_cell_grid_frame_part(part, &[], &[]),
            rows: vec![None; part.rows as usize],
            history_rows: Vec::new(),
            next_history_index: part.sb_base,
            seen_rows: HashSet::new(),
            links: HashMap::new(),
            spans: 0,
        }
    }
}
