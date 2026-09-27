//! The painted grid's geometry, in client (viewport) pixel space, and the pure
//! arithmetic that derives it from a measured box.
//!
//! `CellGridRenderer::viewport_cell_geometry` calls it after a paint; the mouse
//! and selection controllers call it to turn a pointer position into a cell.
//! The derivation is a function of the box, the grid and the measured row
//! height, so it is separated from the DOM read that supplies them.

/// Where the painted grid sits on screen, and how big one cell is.
///
/// `left` and `top` are the origin of CELL (1,1) — the top-left of the PAINTED
/// row box, NOT of the scroll container: the scrollback sheet and the history
/// spacer sit above the rows inside that container, so the container's top is
/// hundreds of pixels off in any pane with history.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerminalCellGeometry {
    /// Client-space left edge of the painted row box.
    pub left: f64,
    /// Client-space top edge of the painted row box.
    pub top: f64,
    /// Width of one grid column, in client pixels.
    pub cell_width: f64,
    /// Height of one grid row, in client pixels.
    pub row_height: f64,
    /// Columns the grid paints.
    pub cols: u32,
    /// Rows the grid paints.
    pub rows: u32,
}

/// Which cell a client-space point lands on, 1-based, clamped INTO the grid.
///
/// Clamping is not cosmetic: a pane letterboxes (the rows are pinned to
/// `cols` x `1ch`, so a wider pane has margin) and the last row rarely ends
/// exactly at the container's bottom, so an unclamped report hands the
/// application a column past `cols` or a row past `rows` — which real TUIs
/// mishandle rather than ignore. A gesture in the margin belongs to the
/// nearest edge cell.
pub fn cell_from_point(
    geometry: TerminalCellGeometry,
    client_x: f64,
    client_y: f64,
) -> (u32, u32) {
    let column = 1 + ((client_x - geometry.left) / geometry.cell_width).floor() as i64;
    let row = 1 + ((client_y - geometry.top) / geometry.row_height).floor() as i64;
    (
        column.clamp(1, i64::from(geometry.cols.max(1))) as u32,
        row.clamp(1, i64::from(geometry.rows.max(1))) as u32,
    )
}

/// Derive the grid geometry from a measured viewport box, or `None` when the
/// measurement cannot support one.
///
/// A zero or negative width means the pane has not laid out, and a zero
/// columns count means there is no grid to divide by; reporting a geometry from
/// either would turn every pointer position into a divide by zero at the one
/// moment the pane is least able to say so.
pub fn grid_geometry_from_box(
    cols: u32,
    rows: u32,
    left: f64,
    top: f64,
    width: f64,
    row_height: f64,
) -> Option<TerminalCellGeometry> {
    if cols == 0 || row_height <= 0.0 {
        return None;
    }
    let cell_width = width / f64::from(cols);
    if cell_width <= 0.0 {
        return None;
    }
    Some(TerminalCellGeometry {
        left,
        top,
        cell_width,
        row_height,
        cols,
        rows,
    })
}
