//! Pixels to terminal cells, one implementation of each direction. The cell
//! probe, padding subtraction and "no measurement, never 1x1" rule port
//! `apps/web/src/client/terminal-stream/terminalCellGeometry.ts` (the live
//! geometry claim and the pre-spawn size hint both measure through it);
//! `TerminalCellGeometry`/`cell_from_point` port `apps/web/src/renderer/terminalMouse.ts`.
//! All arithmetic is native; only the two DOM reads are wasm32 adapters.

use roost_protocol::viewport::TerminalGeometry;

/// Ten cells per probe, so a sub-pixel advance averages out instead of
/// accumulating into a wrong column count.
pub const CELL_PROBE_TEXT: &str = "0000000000";

/// Per-cell advance in CSS px. Fractional on purpose: rounding the advance
/// before the division loses a column across a full pane width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerminalCellBox {
    /// One column's advance.
    pub width: f64,
    /// One row's height.
    pub height: f64,
}

/// The padding a measured box's client size still includes.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BoxPadding {
    /// Left padding, in CSS px.
    pub left: f64,
    /// Right padding, in CSS px.
    pub right: f64,
    /// Top padding, in CSS px.
    pub top: f64,
    /// Bottom padding, in CSS px.
    pub bottom: f64,
}

/// One cell from the probe's measured box, or `None` when it has no layout.
pub fn cell_box_from_probe(probe_width: f64, probe_height: f64) -> Option<TerminalCellBox> {
    if probe_width == 0.0 || probe_height == 0.0 {
        return None;
    }
    Some(TerminalCellBox {
        width: probe_width / CELL_PROBE_TEXT.len() as f64,
        height: probe_height,
    })
}

/// Cols/rows a box can actually paint, or `None` when it is mid-layout or
/// narrower than one cell. `None` is the answer, not 1x1: a bogus geometry
/// reaches the keeper PTY on spawn and every other viewer's smallest common
/// geometry once claimed. Client sizes include padding, so it is subtracted.
pub fn terminal_geometry_for_box(
    client_width: f64,
    client_height: f64,
    padding: BoxPadding,
    cell: TerminalCellBox,
) -> Option<TerminalGeometry> {
    if cell.width <= 0.0 || cell.height <= 0.0 {
        return None;
    }
    let cols = ((client_width - padding.left - padding.right) / cell.width).floor();
    let rows = ((client_height - padding.top - padding.bottom) / cell.height).floor();
    (cols > 0.0 && rows > 0.0).then(|| TerminalGeometry {
        cols: cols.min(f64::from(u32::MAX)) as u32,
        rows: rows.min(f64::from(u32::MAX)) as u32,
    })
}

/// A computed `padding-*` value in px, the way `parseFloat(value) || 0` reads it.
pub fn css_px(value: &str) -> f64 {
    let trimmed = value.trim_start();
    (1..=trimmed.len())
        .rev()
        .find_map(|end| trimmed.get(..end)?.parse::<f64>().ok())
        .filter(|parsed| !parsed.is_nan())
        .unwrap_or(0.0)
}

/// Measure one cell INSIDE a mounted `.cell-grid` box: `.cell-grid .cell-row`
/// carries the row's line-height, so a probe parented anywhere else measures
/// the UA `line-height: normal` and under-counts rows.
#[cfg(target_arch = "wasm32")]
pub fn measure_terminal_cell_box(grid: &web_sys::Element) -> Option<TerminalCellBox> {
    use wasm_bindgen::JsCast;
    let probe = grid.owner_document()?.create_element("span").ok()?;
    probe.set_class_name("cell-row");
    if let Some(styled) = probe.dyn_ref::<web_sys::HtmlElement>() {
        let style = styled.style();
        for (name, value) in [
            ("position", "absolute"),
            ("visibility", "hidden"),
            ("white-space", "pre"),
        ] {
            if style.set_property(name, value).is_err() {
                tracing::warn!(target: "terminal_geometry", property = name, "the cell probe refused a style");
            }
        }
    }
    probe.set_text_content(Some(CELL_PROBE_TEXT));
    grid.append_child(&probe).ok()?;
    let rect = probe.get_bounding_client_rect();
    if grid.remove_child(&probe).is_err() {
        tracing::warn!(target: "terminal_geometry", "the cell probe could not be removed");
    }
    cell_box_from_probe(rect.width(), rect.height())
}

/// `terminal_geometry_for_box` over a mounted element's client box and its
/// computed padding.
#[cfg(target_arch = "wasm32")]
pub fn terminal_geometry_for_element(
    element: &web_sys::HtmlElement,
    cell: TerminalCellBox,
) -> Option<TerminalGeometry> {
    if cell.width <= 0.0 || cell.height <= 0.0 {
        return None;
    }
    let styles = web_sys::window()?.get_computed_style(element).ok()??;
    let padding_px = |name: &str| css_px(&styles.get_property_value(name).unwrap_or_default());
    let padding = BoxPadding {
        left: padding_px("padding-left"),
        right: padding_px("padding-right"),
        top: padding_px("padding-top"),
        bottom: padding_px("padding-bottom"),
    };
    terminal_geometry_for_box(
        f64::from(element.client_width()),
        f64::from(element.client_height()),
        padding,
        cell,
    )
}

/// The painted grid's geometry, in client (viewport) pixel space.
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
/// Clamping is not cosmetic: a pane letterboxes and the last row rarely ends
/// exactly at the container's bottom, so an unclamped report hands the
/// application a column past `cols` or a row past `rows` — which real TUIs
/// mishandle rather than ignore. A gesture in the margin belongs to the
/// nearest edge cell.
pub fn cell_from_point(geometry: TerminalCellGeometry, client_x: f64, client_y: f64) -> (u32, u32) {
    let column = 1 + ((client_x - geometry.left) / geometry.cell_width).floor() as i64;
    let row = 1 + ((client_y - geometry.top) / geometry.row_height).floor() as i64;
    (
        column.clamp(1, i64::from(geometry.cols.max(1))) as u32,
        row.clamp(1, i64::from(geometry.rows.max(1))) as u32,
    )
}

/// Derive the grid geometry from a measured viewport box, or `None` when the
/// measurement cannot support one: no row height, or a cell advance that is
/// not positive, would turn every pointer position into a division by zero.
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
