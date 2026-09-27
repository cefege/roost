//! The painted grid's geometry: the arithmetic that turns a measured viewport
//! box into a cell box, and the pointer arithmetic that turns a client-space
//! point into a cell the terminal can be told about.
//!
//! Clamping is not cosmetic. A pane letterboxes — the rows are pinned to `cols`
//! x `1ch`, so a wider pane has margin — and the last row rarely ends exactly
//! at the container's bottom, so an unclamped report hands the application a
//! column past `cols` or a row past `rows`, which real TUIs mishandle rather
//! than ignore.

use roost_web_terminal::{TerminalCellGeometry, cell_from_point, grid_geometry_from_box};

fn geometry() -> TerminalCellGeometry {
    TerminalCellGeometry {
        left: 100.0,
        top: 200.0,
        cell_width: 8.0,
        row_height: 16.0,
        cols: 80,
        rows: 24,
    }
}

#[test]
fn a_measured_box_divides_into_cells() {
    let derived = grid_geometry_from_box(80, 24, 100.0, 200.0, 640.0, 16.0).expect("measured");
    assert_eq!(derived.cell_width, 8.0);
    assert_eq!(derived.row_height, 16.0);
    assert_eq!(derived.cols, 80);
    assert_eq!(derived.rows, 24);
}

#[test]
fn a_box_that_cannot_support_a_grid_yields_no_geometry() {
    assert!(grid_geometry_from_box(0, 24, 0.0, 0.0, 640.0, 16.0).is_none());
    assert!(grid_geometry_from_box(80, 24, 0.0, 0.0, 640.0, 0.0).is_none());
    assert!(grid_geometry_from_box(80, 24, 0.0, 0.0, 0.0, 16.0).is_none());
}

#[test]
fn a_point_inside_the_grid_names_the_cell_it_lands_on_one_based() {
    let (column, row) = cell_from_point(geometry(), 100.0, 200.0);
    assert_eq!((column, row), (1, 1));
    let (column, row) = cell_from_point(geometry(), 107.9, 215.9);
    assert_eq!((column, row), (1, 1));
    let (column, row) = cell_from_point(geometry(), 108.0, 216.0);
    assert_eq!((column, row), (2, 2));
}

#[test]
fn a_point_in_the_letterbox_margin_belongs_to_the_nearest_edge_cell() {
    let (column, row) = cell_from_point(geometry(), -50.0, 200.0);
    assert_eq!((column, row), (1, 1));
    let (column, row) = cell_from_point(geometry(), 10_000.0, 10_000.0);
    assert_eq!((column, row), (80, 24));
    let (column, row) = cell_from_point(geometry(), 100.0, -50.0);
    assert_eq!((column, row), (1, 1));
}

#[test]
fn a_point_below_the_last_row_still_reports_a_row_the_grid_has() {
    let (column, row) = cell_from_point(geometry(), 100.0, 10_000.0);
    assert_eq!(row, 24);
    assert_eq!(column, 1);
}
