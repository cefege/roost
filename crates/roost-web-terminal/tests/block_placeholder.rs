//! The reserved height of a block or gap of rows: the number every scroll
//! position in the pane is derived from.
//!
//! A placeholder that is off by a pixel, or that carries the self-correcting
//! `auto` form, makes an absolute row index mean two different things in two
//! frames — which is how a reader lands on a different row after a tab switch
//! with no user action at all.

use roost_web_terminal::{DEFAULT_CELL_ROW_PX, SCROLLBACK_BLOCK_ROWS, block_placeholder};

#[test]
fn a_measured_block_height_is_two_decimals_of_rows_times_row_height() {
    assert_eq!(block_placeholder(250, 16.8), "4200.00px");
    assert_eq!(block_placeholder(1, 16.0), "16.00px");
    assert_eq!(block_placeholder(3, 16.75), "50.25px");
    assert_eq!(block_placeholder(0, 16.75), "0.00px");
}

#[test]
fn an_unmeasured_block_reserves_the_default_row_pitch() {
    let expected = format!("{:.2}px", f64::from(7) * DEFAULT_CELL_ROW_PX);
    assert_eq!(block_placeholder(7, 0.0), expected);
    assert_eq!(block_placeholder(7, -3.0), expected);
}

#[test]
fn a_full_block_is_the_250_rows_the_layout_seals_and_the_pager_pages() {
    assert_eq!(SCROLLBACK_BLOCK_ROWS, 250);
    assert_eq!(block_placeholder(SCROLLBACK_BLOCK_ROWS, 0.0), "4200.00px");
}

#[test]
fn the_placeholder_is_a_bare_length_never_the_self_correcting_auto_form() {
    for rows in [1u32, 12, 250, 2000] {
        let height = block_placeholder(rows, 18.0);
        assert!(!height.contains("auto"), "{height}");
        assert!(height.ends_with("px"), "{height}");
        assert_eq!(height.split('.').count(), 2, "{height}");
    }
}

#[test]
fn two_placeholders_for_the_same_interval_agree_however_the_height_was_reached() {
    // A measured height and the default pitch for an unmeasured pane are two
    // paths to the same reservation; they must not disagree in unit.
    assert_eq!(
        block_placeholder(SCROLLBACK_BLOCK_ROWS, DEFAULT_CELL_ROW_PX),
        block_placeholder(SCROLLBACK_BLOCK_ROWS, 0.0)
    );
}
