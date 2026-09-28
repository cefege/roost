//! Pixels to cells in both directions: the measured box to the cols/rows a pane
//! claims (`terminalCellGeometry.ts`, pinned in v2 through
//! `apps/web/tests/wtermSizeEstimate.dom.test.ts`), and a pointer to the 1-based
//! cell it lands on (`apps/web/tests/renderer/terminalMouse.test.ts`
//! "cellFromPoint"). No measurable box means no geometry, never a fabricated 1x1.

use roost_protocol::viewport::TerminalGeometry;
use roost_web_terminal::cell_geometry::{
    BoxPadding, CELL_PROBE_TEXT, TerminalCellBox, cell_box_from_probe, css_px,
    terminal_geometry_for_box,
};
use roost_web_terminal::{TerminalCellGeometry, cell_from_point, grid_geometry_from_box};

/// `.wterm` padding on the fake display box v2's scene builder mounts.
const WTERM_PADDING: BoxPadding = BoxPadding {
    left: 16.0,
    right: 16.0,
    top: 12.0,
    bottom: 12.0,
};

/// The cell a probe inside `.cell-grid` measures: ten 10px advances, 20px rows.
fn grid_cell() -> TerminalCellBox {
    cell_box_from_probe(10.0 * CELL_PROBE_TEXT.len() as f64, 20.0).expect("a laid-out probe")
}

/// A 10x4 grid whose origin is nonzero on both axes, so a mixed-up origin
/// cannot pass by symmetry.
fn grid() -> TerminalCellGeometry {
    TerminalCellGeometry {
        left: 100.0,
        top: 200.0,
        cell_width: 8.0,
        row_height: 16.0,
        cols: 10,
        rows: 4,
    }
}

#[test]
fn the_display_box_minus_its_padding_divides_into_the_claimed_geometry() {
    let claimed = terminal_geometry_for_box(832.0, 424.0, WTERM_PADDING, grid_cell());
    assert_eq!(claimed, Some(TerminalGeometry { cols: 80, rows: 20 }));
}

#[test]
fn a_mid_layout_box_yields_no_geometry_instead_of_one_by_one() {
    assert_eq!(
        terminal_geometry_for_box(4.0, 3.0, WTERM_PADDING, grid_cell()),
        None
    );
    let unmeasured = TerminalCellBox {
        width: 0.0,
        height: 20.0,
    };
    assert_eq!(
        terminal_geometry_for_box(832.0, 424.0, WTERM_PADDING, unmeasured),
        None
    );
}

#[test]
fn a_probe_with_no_layout_is_no_measurement_and_a_laid_out_one_averages_ten_cells() {
    assert_eq!(cell_box_from_probe(0.0, 20.0), None);
    assert_eq!(cell_box_from_probe(84.0, 0.0), None);
    assert_eq!(
        cell_box_from_probe(84.0, 16.8),
        Some(TerminalCellBox {
            width: 8.4,
            height: 16.8
        })
    );
}

#[test]
fn computed_padding_reads_the_way_parse_float_does() {
    assert_eq!(css_px("16px"), 16.0);
    assert_eq!(css_px("12.5px"), 12.5);
    assert_eq!(css_px(""), 0.0);
    assert_eq!(css_px("auto"), 0.0);
}

#[test]
fn a_measured_box_divides_into_cells_and_an_unmeasurable_one_yields_none() {
    let derived = grid_geometry_from_box(80, 24, 100.0, 200.0, 640.0, 16.0).expect("measured");
    assert_eq!((derived.cell_width, derived.row_height), (8.0, 16.0));
    assert_eq!((derived.cols, derived.rows), (80, 24));
    assert!(grid_geometry_from_box(80, 24, 0.0, 0.0, 640.0, 0.0).is_none());
    assert!(grid_geometry_from_box(80, 24, 0.0, 0.0, 0.0, 16.0).is_none());
}

#[test]
fn a_point_inside_the_grid_resolves_to_its_own_cell_one_based() {
    assert_eq!(cell_from_point(grid(), 100.0, 200.0), (1, 1));
    // Last pixel of cell (1,1) still belongs to it; the next one steps.
    assert_eq!(cell_from_point(grid(), 107.9, 215.9), (1, 1));
    assert_eq!(cell_from_point(grid(), 108.0, 216.0), (2, 2));
    assert_eq!(cell_from_point(grid(), 132.0, 248.0), (5, 4));
    assert_eq!(cell_from_point(grid(), 179.5, 263.5), (10, 4));
}

#[test]
fn the_letterbox_margin_clamps_to_the_first_and_last_column() {
    for (x, column) in [(40.0, 1), (99.5, 1), (180.0, 10), (4000.0, 10)] {
        assert_eq!(cell_from_point(grid(), x, 210.0).0, column, "x = {x}");
    }
}

#[test]
fn above_the_first_row_and_below_the_last_row_clamp_into_the_grid() {
    for y in [199.5, -500.0] {
        assert_eq!(cell_from_point(grid(), 110.0, y), (2, 1), "y = {y}");
    }
    for y in [264.0, 9000.0] {
        assert_eq!(cell_from_point(grid(), 110.0, y), (2, 4), "y = {y}");
    }
}

#[test]
fn a_fractional_row_height_resolves_the_right_row_at_row_one_mid_grid_and_the_last_row() {
    // 14px x line-height 1.2 = 16.8px: rounding it drifts a whole row by row 20.
    let tall = TerminalCellGeometry {
        left: 0.0,
        top: 0.0,
        cell_width: 8.4,
        row_height: 16.8,
        cols: 80,
        rows: 24,
    };
    let row_at = |y: f64| cell_from_point(tall, 0.0, y).1;
    let column_at = |x: f64| cell_from_point(tall, x, 0.0).0;
    assert_eq!([row_at(0.0), row_at(16.79), row_at(16.8)], [1, 1, 2]);
    assert_eq!([row_at(184.8), row_at(201.5), row_at(201.6)], [12, 12, 13]);
    assert_eq!([row_at(386.5), row_at(403.1)], [24, 24]);
    assert_eq!(
        [column_at(8.39), column_at(8.4), column_at(663.6)],
        [1, 2, 80]
    );
}
