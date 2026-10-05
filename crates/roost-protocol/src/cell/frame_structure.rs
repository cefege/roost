//! What a wire cell frame claims to contain must be what it contains: legal
//! geometry, one row per viewport index, and — for a full — history covering
//! `[sb_base, scrollback_total)` exactly. Called by `super::proto` on both
//! directions (a frame it built, and a frame it is about to decode); depends on
//! `crate::viewport` for the geometry bound.

use roost_proto::PbCellGridFrame;

use crate::error::{ProtocolError, ProtocolResult};
use crate::viewport::{TerminalGeometry, assert_terminal_geometry};

/// Refuse a frame whose rows do not match the geometry and history it names.
pub(crate) fn assert_frame_structure(frame: &PbCellGridFrame) -> ProtocolResult<()> {
    let geometry = TerminalGeometry {
        cols: frame.cols,
        rows: frame.rows,
    };
    assert_terminal_geometry(&geometry).map_err(|error| error.within("cell_frame"))?;
    // `rows` is bounded by `assert_terminal_geometry` above, so a flag per row
    // is a small fixed allocation rather than a hash set per frame.
    let mut seen = vec![false; frame.rows as usize];
    let mut seen_count = 0usize;
    for row in &frame.viewport_rows {
        if row.index >= frame.rows {
            return Err(ProtocolError::new(
                format!("cell_frame.viewport_rows[{}].index", row.index),
                format!(
                    "cell viewport row {} is outside 0..{}",
                    row.index,
                    frame.rows - 1
                ),
            ));
        }
        let slot = &mut seen[row.index as usize];
        if *slot {
            return Err(ProtocolError::new(
                format!("cell_frame.viewport_rows[{}].index", row.index),
                format!("cell viewport row {} occurs more than once", row.index),
            ));
        }
        *slot = true;
        seen_count += 1;
    }
    if !frame.full {
        if !frame.scrollback_rows.is_empty() {
            return Err(ProtocolError::new(
                "cell_frame.scrollback_rows",
                "cell delta cannot carry scrollback_rows",
            ));
        }
        return Ok(());
    }
    if seen_count != frame.rows as usize {
        return Err(ProtocolError::new(
            "cell_frame.viewport_rows",
            format!(
                "full cell frame has {} of {} required viewport rows",
                seen_count, frame.rows
            ),
        ));
    }
    if !frame.scrollback_append.is_empty() {
        return Err(ProtocolError::new(
            "cell_frame.scrollback_append",
            "full cell frame cannot carry scrollback_append",
        ));
    }
    if frame.sb_base > frame.scrollback_total
        || frame.scrollback_rows.len() as u64 != frame.scrollback_total - frame.sb_base
    {
        return Err(ProtocolError::new(
            "cell_frame.scrollback_rows",
            format!(
                "full cell frame history does not cover [{}, {}) exactly",
                frame.sb_base, frame.scrollback_total
            ),
        ));
    }
    for (offset, row) in frame.scrollback_rows.iter().enumerate() {
        let expected = frame.sb_base + offset as u64;
        if u64::from(row.index) != expected {
            return Err(ProtocolError::new(
                format!("cell_frame.scrollback_rows[{offset}].index"),
                format!("full cell frame history row {expected} is missing or out of order"),
            ));
        }
    }
    Ok(())
}
