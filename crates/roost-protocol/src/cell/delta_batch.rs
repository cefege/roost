//! Sparse cell-delta batch folding for one browser paint.
//!
//! Reuses the single-delta validator in `diff_grid` and owns the mutable row
//! coordinates, so a caller that missed intermediate frames lands on the current
//! state with one bounded patch list. An invalid chain folds to `None` and
//! touches nothing: a batch that half-applied would paint a grid no frame ever
//! described.

use crate::cell::diff_grid::{apply_delta, clone_cell_grid_frame, delta_viewport_shift};
use crate::cell::types::{CellGridFrame, CellRow};

/// The result of folding a run of sparse deltas onto one starting frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellDeltaBatch {
    /// Independently owned canonical successor after every input delta.
    pub frame: CellGridFrame,
    /// Rows to patch in the successor's final viewport coordinates.
    pub dirty_rows: Vec<CellRow>,
    /// Newly appended history rows in their original arrival order.
    pub scrollback_append: Vec<CellRow>,
    /// Proven viewport movement from the starting frame, capped to its height.
    pub viewport_shift: u32,
}

/// Delta history is append-only: a gap would make one batched DOM append lie.
fn has_contiguous_scrollback_append(base: &CellGridFrame, delta: &CellGridFrame) -> bool {
    let Some(appended) = base
        .scrollback_total
        .checked_add(u64::try_from(delta.scrollback_append.len()).unwrap_or(u64::MAX))
    else {
        return false;
    };
    if delta.scrollback_total != appended {
        return false;
    }
    delta
        .scrollback_append
        .iter()
        .enumerate()
        .all(|(offset, row)| u64::from(row.index) == base.scrollback_total + offset as u64)
}

/// Fold contiguous sparse deltas into one independently owned canonical frame.
///
/// Neither `base` nor any supplied delta is mutated: a chain that turns out to
/// be invalid half way through leaves the caller's replicas exactly as they were.
pub fn fold_cell_delta_batch(
    base: &CellGridFrame,
    deltas: &[CellGridFrame],
) -> Option<CellDeltaBatch> {
    if deltas.is_empty() {
        return None;
    }

    let mut folded = clone_cell_grid_frame(base);
    let mut dirty_marks = vec![0u8; folded.rows as usize];
    let mut scrollback_append: Vec<CellRow> = Vec::new();
    let mut viewport_shift = 0u32;

    for delta in deltas {
        if delta.full || !has_contiguous_scrollback_append(&folded, delta) {
            return None;
        }
        let shift = delta_viewport_shift(&folded, delta);
        // Taken from the delta as it arrived, before the fold: the batch's own
        // history list and its patch marks are its own, and the delta's rows go
        // on to become the successor's rows.
        let appended = delta.scrollback_append.clone();
        let changed: Vec<u32> = delta.viewport_rows.iter().map(|row| row.index).collect();
        apply_delta(&mut folded, delta)?;

        if shift > 0 {
            dirty_marks.copy_within(shift as usize.., 0);
            for mark in dirty_marks
                .iter_mut()
                .skip(folded.rows as usize - shift as usize)
            {
                *mark = 0;
            }
        }
        for index in changed {
            dirty_marks[index as usize] = 1;
        }
        scrollback_append.extend(appended);
        viewport_shift = folded.rows.min(viewport_shift + shift);
    }

    let dirty_rows = if viewport_shift == folded.rows {
        folded.viewport_rows.clone()
    } else {
        folded
            .viewport_rows
            .iter()
            .enumerate()
            .filter(|(index, _)| dirty_marks[*index] != 0)
            .map(|(_, row)| row.clone())
            .collect()
    };

    Some(CellDeltaBatch {
        frame: folded,
        dirty_rows,
        scrollback_append,
        viewport_shift,
    })
}
