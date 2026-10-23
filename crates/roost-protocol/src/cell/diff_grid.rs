//! Reconstructs terminal frames and viewport shifts for replicas.
//! Emitters provide immutable epochs and consecutive sequence chains.
//! Invalid delta geometry or row numbering leaves the held frame intact.
//! Viewport checkpoints omit history without discarding incoming shift data.

use std::borrow::BorrowMut;

use crate::cell::types::{CellGridFrame, CellRow, no_image_placements};

/// Give a replica or renderer independent mutable row coordinates while sharing
/// immutable span arrays. `apply_delta` replaces row slots and renumbers rows, so
/// copying only the outer arrays is not enough — the row objects themselves must
/// be fresh.
pub fn clone_cell_grid_frame(frame: &CellGridFrame) -> CellGridFrame {
    let mut fresh = clone_cell_grid_frame_without_history(frame);
    fresh.scrollback_rows = clone_rows(&frame.scrollback_rows);
    fresh
}

/// `clone_cell_grid_frame` minus `scrollback_rows`, which comes back empty.
/// For a fold that never reads the base's history: copying a renderer's held
/// history on every paint is the cost this avoids.
pub fn clone_cell_grid_frame_without_history(frame: &CellGridFrame) -> CellGridFrame {
    clone_frame_without_retained_history(frame, true)
}

fn clone_frame_without_retained_history(
    frame: &CellGridFrame,
    retain_append: bool,
) -> CellGridFrame {
    CellGridFrame {
        stream_id: frame.stream_id.clone(),
        grid_epoch: frame.grid_epoch.clone(),
        cols: frame.cols,
        rows: frame.rows,
        cursor_row: frame.cursor_row,
        cursor_col: frame.cursor_col,
        cursor_visible: frame.cursor_visible,
        alt_screen: frame.alt_screen,
        cursor_keys_app: frame.cursor_keys_app,
        bracketed_paste: frame.bracketed_paste,
        mouse_tracking: frame.mouse_tracking,
        mouse_sgr: frame.mouse_sgr,
        focus_events: frame.focus_events,
        kitty_keyboard_flags: frame.kitty_keyboard_flags,
        full: frame.full,
        viewport_rows: clone_rows(&frame.viewport_rows),
        scrollback_rows: Vec::new(),
        scrollback_append: if retain_append {
            clone_rows(&frame.scrollback_append)
        } else {
            Vec::new()
        },
        scrollback_total: frame.scrollback_total,
        sb_base: frame.sb_base,
        base_seq: frame.base_seq,
        seq: frame.seq,
        image_placements: frame.image_placements.clone(),
    }
}

/// Convert a reconstructed frame into a viewport-only canonical checkpoint.
/// Fresh history arrays sever any alias to an inbound delta before it is reused
/// for live delivery.
pub fn normalize_cell_grid_frame(frame: &mut CellGridFrame) {
    frame.full = true;
    frame.base_seq = 0;
    frame.scrollback_rows = Vec::new();
    frame.scrollback_append = Vec::new();
    frame.sb_base = frame.scrollback_total;
    // A full frame always states its image set, even when it has none.
    frame
        .image_placements
        .get_or_insert_with(no_image_placements);
}

/// Applies a frame to a viewport-only checkpoint without copying retained history.
/// Incoming delta history still participates in viewport-shift detection.
/// A missing baseline or invalid delta leaves the checkpoint unchanged.
pub fn apply_viewport_frame<Checkpoint>(
    checkpoint: &mut Option<Checkpoint>,
    frame: &CellGridFrame,
) -> Option<()>
where
    Checkpoint: BorrowMut<CellGridFrame> + From<CellGridFrame>,
{
    if let Some(current) = checkpoint {
        let current: &mut CellGridFrame = current.borrow_mut();
        apply_delta_with_history(current, frame, false)?;
        normalize_cell_grid_frame(current);
    } else if frame.full {
        let mut current = clone_frame_without_retained_history(frame, false);
        normalize_cell_grid_frame(&mut current);
        *checkpoint = Some(current.into());
    } else {
        return None;
    }
    Some(())
}

/// Number of held viewport rows reusable by a global viewport shift.
///
/// A matching history/head boundary identifies a shift candidate, but a
/// partial-region scroll can keep held footer rows fixed. Reuse a global shift
/// only when `delta.viewport_rows` supplies the complete final viewport: those
/// final-coordinate rows overwrite every row after transfer. Compare the
/// boundary row directly — this neither hashes nor walks the held viewport.
pub fn delta_viewport_shift(base: &CellGridFrame, delta: &CellGridFrame) -> u32 {
    if delta.viewport_rows.len() != base.rows as usize {
        return 0;
    }
    let shift = u32::try_from(delta.scrollback_append.len())
        .unwrap_or(u32::MAX)
        .min(base.rows);
    if shift == 0 {
        return 0;
    }
    let Some(held) = base.viewport_rows.first() else {
        return 0;
    };
    let Some(appended) = delta.scrollback_append.first() else {
        return 0;
    };
    // Every span field participates, so the derived equality IS this comparison,
    // and an `Arc` compares what it points at rather than where. A row keeps its
    // cells' link indices when it scrolls out of the viewport, so a link
    // mismatch means these are genuinely different rows.
    if held.spans != appended.spans {
        return 0;
    }
    shift
}

/// Apply a delta onto a client's held frame in place. `None` when the delta
/// belongs to a different immutable grid epoch, or its sequence, geometry or row
/// numbering does not line up. A full frame replaces the held frame wholesale.
///
/// The delta is borrowed, never mutated: `fold_cell_delta_batch` reads a delta's
/// rows after applying it, and an invalid chain must leave every input untouched.
pub fn apply_delta(base: &mut CellGridFrame, delta: &CellGridFrame) -> Option<()> {
    apply_delta_with_history(base, delta, true)
}

fn apply_delta_with_history(
    base: &mut CellGridFrame,
    delta: &CellGridFrame,
    retain_history: bool,
) -> Option<()> {
    if delta.full {
        *base = if retain_history {
            delta.clone()
        } else {
            clone_frame_without_retained_history(delta, false)
        };
        return Some(());
    }
    if delta.stream_id != base.stream_id
        || delta.base_seq != base.seq
        || delta.base_seq.checked_add(1) != Some(delta.seq)
        || delta.grid_epoch != base.grid_epoch
        || delta.cols != base.cols
        || delta.rows != base.rows
        || delta.alt_screen != base.alt_screen
        || base.viewport_rows.len() != base.rows as usize
        || !delta.scrollback_rows.is_empty()
    {
        return None;
    }

    let mut changed = vec![false; base.viewport_rows.len()];
    for row in &delta.viewport_rows {
        let slot = row.index as usize;
        if slot >= changed.len() || changed[slot] {
            return None;
        }
        changed[slot] = true;
    }

    let scrolled = delta_viewport_shift(base, delta);
    if scrolled > 0 {
        let moved = scrolled as usize;
        // A rotation, not a copy: the surviving rows keep the very cells they
        // were holding, so a renderer that owns their DOM nodes keeps them.
        base.viewport_rows.rotate_left(moved);
        for index in 0..(base.rows as usize - moved) {
            base.viewport_rows[index].index = index as u32;
        }
    }
    // Overwrite changed rows by index; every other row keeps its transferred span
    // array and, on the browser side, its existing DOM node.
    for row in &delta.viewport_rows {
        base.viewport_rows[row.index as usize] = row.clone();
    }

    if retain_history {
        base.scrollback_rows
            .extend(delta.scrollback_append.iter().cloned());
    }
    base.scrollback_append.clear();

    base.stream_id.clone_from(&delta.stream_id);
    base.grid_epoch.clone_from(&delta.grid_epoch);
    base.cols = delta.cols;
    base.rows = delta.rows;
    base.cursor_row = delta.cursor_row;
    base.cursor_col = delta.cursor_col;
    base.cursor_visible = delta.cursor_visible;
    base.alt_screen = delta.alt_screen;
    base.cursor_keys_app = delta.cursor_keys_app;
    base.bracketed_paste = delta.bracketed_paste;
    base.mouse_tracking = delta.mouse_tracking;
    base.mouse_sgr = delta.mouse_sgr;
    base.focus_events = delta.focus_events;
    base.kitty_keyboard_flags = delta.kitty_keyboard_flags;
    base.full = true;
    base.scrollback_total = delta.scrollback_total;
    base.base_seq = 0;
    base.seq = delta.seq;
    // A delta states its image set only when it changed, and then replaces it.
    if let Some(placements) = &delta.image_placements {
        base.image_placements = Some(placements.clone());
    }
    // sb_base is deliberately untouched: deltas never move the held window's base.
    Some(())
}

fn clone_rows(rows: &[CellRow]) -> Vec<CellRow> {
    rows.to_vec()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod viewport_tests;
