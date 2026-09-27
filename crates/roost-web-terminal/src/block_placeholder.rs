//! The reserved pixel height of one block or gap of scrollback rows, and the
//! two row-height constants every scroll offset in the renderer derives from.
//!
//! Called by the scrollback layout (`cell_renderer/scrollback.rs`) to size an
//! exact-height placeholder, by the font-settled repair, and by the smoke
//! harness and the incident scanner, which name the function directly.
//!
//! It is pure arithmetic: an absolute row index is turned into a scroll offset
//! by multiplying by this height, so the value must be a BARE length. The
//! self-correcting `contain-intrinsic-size: auto <len>` form makes a browser
//! reuse a block's last RENDERED size, which understates `scrollHeight` for a
//! block that grew while skipped.

/// Rows one sealed scrollback block holds. A block is the eviction unit and the
/// backfill page size, so both numbers are this one constant.
pub const SCROLLBACK_BLOCK_ROWS: u32 = 250;

/// Row height used before a real measurement exists: a font that has not
/// settled, a pane that has never painted. Every derived offset multiplies this
/// rather than zero, because a zero height collapses the whole scroll space.
pub const DEFAULT_CELL_ROW_PX: f64 = 16.8;

/// The exact contain-intrinsic-size value for a measured block of rows.
///
/// `row_height` falls back to the default when no measurement exists, so a
/// placeholder never collapses to `0px` and a scroll offset derived from it
/// never lands outside its own row.
pub fn block_placeholder(rows: u32, row_height: f64) -> String {
    let height = if row_height > 0.0 {
        row_height
    } else {
        DEFAULT_CELL_ROW_PX
    };
    format!("{:.2}px", f64::from(rows) * height)
}

#[cfg(test)]
mod tests {
    use super::{block_placeholder, DEFAULT_CELL_ROW_PX, SCROLLBACK_BLOCK_ROWS};

    #[test]
    fn a_measured_block_height_is_two_decimals_of_rows_times_row_height() {
        assert_eq!(block_placeholder(250, 16.8), "4200.00px");
        assert_eq!(block_placeholder(1, 16.0), "16.00px");
        assert_eq!(block_placeholder(3, 16.75), "50.25px");
    }

    #[test]
    fn an_unmeasured_block_reserves_the_default_row_pitch() {
        let expected = format!("{:.2}px", f64::from(7) * DEFAULT_CELL_ROW_PX);
        assert_eq!(block_placeholder(7, 0.0), expected);
        assert_eq!(block_placeholder(7, -3.0), expected);
    }

    #[test]
    fn a_full_block_is_the_250_rows_the_layout_seals() {
        assert_eq!(SCROLLBACK_BLOCK_ROWS, 250);
        assert_eq!(block_placeholder(SCROLLBACK_BLOCK_ROWS, 0.0), "4200.00px");
    }

    #[test]
    fn the_placeholder_never_carries_the_self_correcting_auto_form() {
        assert!(!block_placeholder(250, 16.8).contains("auto"));
    }
}
