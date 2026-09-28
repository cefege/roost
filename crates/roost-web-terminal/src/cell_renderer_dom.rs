//! The DOM the renderer owns: the element tree it builds inside a container
//! (stamped BOTH `wterm`, which makes history scrollable, and `cell-grid`), the
//! exact-height placeholders on scrollback blocks, and the probe and box reads
//! the layout needs. `CellGridRenderer` composes these through `RenderElement`.
//! Ports `apps/web/src/renderer/cellRendererDom.ts`.

use crate::block_placeholder::{DEFAULT_CELL_ROW_PX, block_placeholder};
use crate::cell_geometry::{TerminalCellGeometry, grid_geometry_from_box};
use crate::render_element::RenderElement;
use roost_protocol::cell::{CellGridFrame, spans_text};

#[cfg(target_arch = "wasm32")]
pub use web_nodes::{as_node, create_div, create_span, detach, is_child_of};

/// The only two ways element creation fails: a tag name the document rejects,
/// and a `div` that comes back as something the renderer cannot stamp inline
/// styles on. Every tag here is a constant, so these name unreachable states
/// rather than runtime conditions a caller must handle. The renderer leaves the
/// DOM untouched when one happens, and its reconcile watermark repairs on the
/// next frame.
#[derive(Debug, thiserror::Error)]
pub enum DomSetupError {
    /// The document refused to create one of the renderer's fixed elements.
    #[error("the document refused to create a {tag} element")]
    RefusedTag {
        /// The tag the renderer asked for.
        tag: String,
    },
    /// The document produced a tag the renderer stamps inline styles on, but
    /// not as an HTML element — an XML document's `div`, where `style` has
    /// nowhere to land.
    #[error("the document produced a {tag} that is not an HTML element")]
    NotHtmlElement {
        /// The tag the renderer asked for.
        tag: String,
    },
}

/// A DOM result.
pub type DomResult<T> = Result<T, DomSetupError>;

/// The elements a renderer owns inside its container.
#[derive(Debug)]
pub struct CellRendererElements<E> {
    /// Reserved height of the unpainted history HEAD, a SIBLING of the
    /// scrollback sheet. Sibling placement is load-bearing: the sheet's first
    /// child is the eviction unit, and an absolute row's pixel offset is the
    /// spacer's offset plus its row number.
    pub spacer: E,
    /// The immutable painted history: blocks and exact-height gaps.
    pub scrollback: E,
    /// The live grid rows, plus the cursor and ghost overlays.
    pub viewport: E,
    /// The local cursor block.
    pub cursor: E,
    /// Remote cursor overlays, sharing the viewport as their host.
    pub ghosts: E,
}

/// One remote operator's cursor, in grid cells from the pane's top-left.
#[derive(Debug, Clone, PartialEq)]
pub struct GhostCursor {
    /// Stable identity of the operator whose cursor this is.
    pub operator_id: String,
    /// Column offset, in cells.
    pub x: f64,
    /// Row offset, in cells.
    pub y: f64,
    /// Hover label, defaulting to the operator id.
    pub label: Option<String>,
}

/// Whether `child` is currently a direct child of `parent`, by node identity.
pub fn is_placed_in<E: RenderElement>(child: &E, parent: &E) -> bool {
    child.parent().is_some_and(|owner| owner == *parent)
}

/// Create the renderer's element tree inside `container`.
///
/// The container's class list ends up holding BOTH `wterm` and `cell-grid`,
/// and it takes `role=log` so assistive technology reads the terminal as a log
/// rather than as layout.
pub fn create_cell_renderer_elements<E: RenderElement>(
    container: &E,
) -> DomResult<CellRendererElements<E>> {
    container.add_class("wterm");
    container.add_class("cell-grid");
    container.set_attribute("role", "log");
    let spacer = classed_div(container, "cell-sb-spacer")?;
    spacer.set_style("height", "0px");
    let scrollback = classed_div(container, "cell-scrollback")?;
    let viewport = classed_div(container, "cell-viewport")?;
    viewport.set_style("position", "relative");
    let cursor = classed_div(container, "cell-cursor")?;
    let ghosts = classed_div(container, "cell-ghosts")?;
    container.append_child(&spacer);
    container.append_child(&scrollback);
    container.append_child(&viewport);
    Ok(CellRendererElements {
        spacer,
        scrollback,
        viewport,
        cursor,
        ghosts,
    })
}

fn classed_div<E: RenderElement>(factory: &E, class_name: &str) -> DomResult<E> {
    let element = factory.create_element("div")?;
    element.set_class_name(class_name);
    Ok(element)
}

/// Stamp the exact reserved height of a block or gap of rows onto one element.
///
/// The value is a BARE length. The self-correcting `auto <len>` form makes a
/// browser reuse a block's LAST RENDERED size, so a block that grew while
/// skipped understates `scrollHeight` until it materializes — and every scroll
/// position in the pane is derived from that number.
pub fn size_scrollback_block<E: RenderElement>(block: &E, rows: u32, row_height: f64) {
    block.set_style(
        "contain-intrinsic-size",
        &block_placeholder(rows, row_height),
    );
}

/// The ghost overlay boxes for a set of remote cursors.
///
/// A ghost that cannot be created is dropped: an overlay is decoration, and
/// refusing to paint the whole grid because one remote box failed would trade a
/// missing cursor for a frozen pane.
pub fn create_ghost_elements<E: RenderElement>(factory: &E, ghosts: &[GhostCursor]) -> Vec<E> {
    ghosts
        .iter()
        .filter_map(|ghost| {
            let box_element = classed_div(factory, "cell-ghost").ok()?;
            box_element.set_attribute("data-operator-id", &ghost.operator_id);
            box_element.set_attribute(
                "title",
                ghost.label.as_deref().unwrap_or(&ghost.operator_id),
            );
            box_element.set_style(
                "transform",
                &format!("translate({}ch, {}lh)", ghost.x, ghost.y),
            );
            Some(box_element)
        })
        .collect()
}

/// The painted grid as text, one line per row, for a diagnostics dump.
pub fn cell_grid_text(frame: Option<&CellGridFrame>) -> String {
    frame.map_or(String::new(), |frame| {
        frame
            .viewport_rows
            .iter()
            .map(|row| spans_text(&row.spans))
            .collect::<Vec<String>>()
            .join("\n")
    })
}

/// The tail of the applied frame's history as text, newest `max_rows` rows.
pub fn cell_scrollback_text(frame: Option<&CellGridFrame>, max_rows: usize) -> String {
    frame.map_or(String::new(), |frame| {
        let skip = frame.scrollback_rows.len().saturating_sub(max_rows);
        frame
            .scrollback_rows
            .iter()
            .skip(skip)
            .map(|row| spans_text(&row.spans))
            .collect::<Vec<String>>()
            .join("\n")
    })
}

/// Measure one row's height with a throwaway probe element.
///
/// The probe is detached before returning, so a measurement never becomes a
/// painted row: the viewport diff owns the row list, and a stray probe would
/// make the diff's row count disagree with the grid.
pub fn measure_cell_row_height<E: RenderElement>(viewport: &E) -> f64 {
    let Ok(probe) = classed_div(viewport, "cell-row") else {
        return 0.0;
    };
    probe.set_style("position", "absolute");
    probe.set_style("visibility", "hidden");
    probe.set_text(" ");
    viewport.append_child(&probe);
    let height = probe.bounding_rect().height;
    probe.remove();
    height
}

/// The grid geometry a measured viewport box implies, or `None` before the pane
/// has measured a row or laid out a box.
pub fn viewport_cell_geometry<E: RenderElement>(
    frame: Option<&CellGridFrame>,
    viewport: &E,
    row_height: f64,
) -> Option<TerminalCellGeometry> {
    let frame = frame?;
    if row_height <= 0.0 {
        return None;
    }
    let rect = viewport.bounding_rect();
    grid_geometry_from_box(
        frame.cols, frame.rows, rect.left, rect.top, rect.width, row_height,
    )
}

/// Force the alternate-screen class to the frame's state, returning the state
/// now painted. The class is FORCED, not flipped: a first paint of a normal
/// screen must leave it off, not turn it on.
pub fn sync_alternate_screen<E: RenderElement>(
    container: &E,
    frame: Option<&CellGridFrame>,
    painted: Option<bool>,
) -> bool {
    let active = frame.is_some_and(|frame| frame.alt_screen);
    if Some(active) != painted {
        container.toggle_class("alt-active", active);
    }
    active
}

/// Publish the grid's column count as a custom property, returning the count
/// now painted. The CSS paints exactly `cols` x `1ch`, so this value is what
/// stops a narrower fallback font from painting columns outside the clip.
pub fn paint_cell_grid_width<E: RenderElement>(
    container: &E,
    frame: Option<&CellGridFrame>,
    painted: Option<u32>,
) -> Option<u32> {
    let Some(frame) = frame else {
        return painted;
    };
    if Some(frame.cols) == painted {
        return painted;
    }
    container.set_style("--cell-cols", &frame.cols.to_string());
    Some(frame.cols)
}

/// The row height a derived scroll offset should multiply, falling back to the
/// default pitch so a scroll position is never computed against zero.
pub fn effective_row_height(row_height: f64) -> f64 {
    if row_height > 0.0 {
        row_height
    } else {
        DEFAULT_CELL_ROW_PX
    }
}

/// The raw `web_sys` node helpers the predictive-echo overlay paints with. The
/// renderer itself goes through `RenderElement`; these stay for the overlay,
/// which owns its own `wasm32` adapter.
#[cfg(target_arch = "wasm32")]
mod web_nodes {
    use web_sys::{Document, Element, Node};

    use super::{DomResult, DomSetupError};

    /// One element as the node every structural `Node` call takes.
    pub fn as_node(element: &Element) -> &Node {
        AsRef::<Node>::as_ref(element)
    }

    /// Create one `div`.
    pub fn create_div(doc: &Document) -> DomResult<Element> {
        created(doc, "div")
    }

    /// Create one inline `span`.
    pub fn create_span(doc: &Document) -> DomResult<Element> {
        created(doc, "span")
    }

    fn created(doc: &Document, tag: &str) -> DomResult<Element> {
        doc.create_element(tag)
            .map_err(|_| DomSetupError::RefusedTag {
                tag: tag.to_string(),
            })
    }

    /// Whether `child` is currently a direct child of `parent`, by identity.
    pub fn is_child_of(child: &Element, parent: &Element) -> bool {
        child.parent_element().is_some_and(|owner| owner == *parent)
    }

    /// Remove an element from wherever it currently sits; a detached element
    /// is a no-op instead of a throw.
    pub fn detach(element: &Element) {
        if let Some(parent) = as_node(element).parent_node() {
            let _ = parent.remove_child(as_node(element));
        }
    }
}
