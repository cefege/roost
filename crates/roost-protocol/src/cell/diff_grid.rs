//! Frame-to-frame diffing: how a replica turns two consecutive grid frames into
//! a scroll shift plus the rows that changed.
//!
//! The producing side is the cell emitter's delta decision, and ITS reframe rule
//! (cols/rows change, alt-screen toggle, scrollback shrink → full frame) is what
//! makes a delta safely applicable here. A delta that fails an admission fact
//! below is refused, never approximated: a replica that guessed would freeze a
//! stale repaint generation into history.

use crate::cell::types::{CellGridFrame, CellRow};

/// Give a replica or renderer independent mutable row coordinates while sharing
/// immutable span arrays. `apply_delta` replaces row slots and renumbers rows, so
/// copying only the outer arrays is not enough — the row objects themselves must
/// be fresh.
pub fn clone_cell_grid_frame(frame: &CellGridFrame) -> CellGridFrame {
    let mut fresh = frame.clone();
    fresh.viewport_rows = clone_rows(&frame.viewport_rows);
    fresh.scrollback_rows = clone_rows(&frame.scrollback_rows);
    fresh.scrollback_append = clone_rows(&frame.scrollback_append);
    fresh
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
    if delta.full {
        *base = delta.clone();
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

    // Scrollback is append-only, so the held history extends rather than copies.
    base.scrollback_rows
        .extend(delta.scrollback_append.iter().cloned());
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
    base.full = true;
    base.scrollback_total = delta.scrollback_total;
    base.base_seq = 0;
    base.seq = delta.seq;
    // sb_base is deliberately untouched: deltas never move the held window's base.
    Some(())
}

fn clone_rows(rows: &[CellRow]) -> Vec<CellRow> {
    rows.to_vec()
}
