//! The DOM the renderer owns: the element tree it builds inside a container,
//! the exact-height placeholders it stamps on scrollback blocks, and the two
//! node operations the layout needs.
//!
//! `CellGridRenderer` composes these; nothing else constructs or destroys a
//! terminal element. The container is stamped with BOTH `wterm` and
//! `cell-grid` because the first carries the overflow rules that make history
//! scrollable at all and the second scopes every cell-grid rule.
//!
//! Only the head spacer is an `HtmlElement`, and only because `offset_top` is
//! the one measured offset the renderer reads off it. Everything else is an
//! `Element`, which keeps the structural calls unambiguous: a web-sys element
//! implements `AsRef` for its whole IDL chain, so an `as_ref()` on one is
//! ambiguous between `Node`, `Element` and the document.

use wasm_bindgen::JsCast;
use web_sys::{Document, Element, HtmlElement, Node};

use crate::block_placeholder::{DEFAULT_CELL_ROW_PX, block_placeholder};
use crate::cell_geometry::{TerminalCellGeometry, grid_geometry_from_box};
use crate::element_style::set_style_property;
use roost_protocol::cell::{CellGridFrame, spans_text};

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
    /// nowhere to land. The renderer's geometry would be derived from reserved
    /// pixels the element never reserves.
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
pub struct CellRendererElements {
    /// The document the container lives in; every created node comes from it.
    pub doc: Document,
    /// Reserved height of the unpainted history HEAD, a SIBLING of the
    /// scrollback sheet. Sibling placement is load-bearing: the sheet's first
    /// child is the eviction unit, and an absolute row's pixel offset is the
    /// spacer's offset plus its row number.
    pub spacer: HtmlElement,
    /// The immutable painted history: blocks and exact-height gaps.
    pub scrollback: Element,
    /// The live grid rows, plus the cursor and ghost overlays.
    pub viewport: Element,
    /// The local cursor block.
    pub cursor: Element,
    /// Remote cursor overlays, sharing the viewport as their host.
    pub ghosts: Element,
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

/// One element as the node every structural `Node` call takes.
pub fn as_node(element: &Element) -> &Node {
    AsRef::<Node>::as_ref(element)
}

/// One HTML element as the node every structural `Node` call takes.
pub fn html_as_node(element: &HtmlElement) -> &Node {
    AsRef::<Node>::as_ref(element)
}

/// Create one of the renderer's fixed `div` elements.
pub fn create_div(doc: &Document) -> DomResult<Element> {
    doc.create_element("div")
        .map_err(|_| DomSetupError::RefusedTag {
            tag: "div".to_string(),
        })
}

/// Create one inline `span`, the unit a painted row is built from.
pub fn create_span(doc: &Document) -> DomResult<Element> {
    doc.create_element("span")
        .map_err(|_| DomSetupError::RefusedTag {
            tag: "span".to_string(),
        })
}

/// Whether `child` is currently a direct child of `parent`.
///
/// Identity, not equality: a web-sys element has no `PartialEq`, and the
/// overlays have to be re-appended exactly when they are no longer children of
/// the viewport rather than merely when their contents differ.
pub fn is_child_of(child: &Element, parent: &Element) -> bool {
    child
        .parent_node()
        .is_some_and(|owner| std::ptr::eq(&owner, as_node(parent)))
}

/// Create the renderer's element tree inside `container`.
///
/// The container's class list ends up holding BOTH `wterm` and `cell-grid`,
/// and it takes `role=log` so assistive technology reads the terminal as a log
/// rather than as layout.
pub fn create_cell_renderer_elements(container: &Element) -> DomResult<CellRendererElements> {
    let doc = container
        .owner_document()
        .ok_or_else(|| DomSetupError::RefusedTag {
            tag: "document".to_string(),
        })?;
    let _ = container.class_list().add_2("wterm", "cell-grid");
    let _ = container.set_attribute("role", "log");
    let spacer = classed_div(&doc, "cell-sb-spacer")?;
    set_style_property(&spacer, "height", "0px");
    let scrollback = classed_div(&doc, "cell-scrollback")?;
    let viewport = classed_div(&doc, "cell-viewport")?;
    set_style_property(&viewport, "position", "relative");
    let cursor = classed_div(&doc, "cell-cursor")?;
    let ghosts = classed_div(&doc, "cell-ghosts")?;
    let spacer: HtmlElement = spacer
        .dyn_into()
        .map_err(|_| DomSetupError::NotHtmlElement {
            tag: "div".to_string(),
        })?;
    let _ = container.append_child(html_as_node(&spacer));
    let _ = container.append_child(as_node(&scrollback));
    let _ = container.append_child(as_node(&viewport));
    Ok(CellRendererElements {
        doc,
        spacer,
        scrollback,
        viewport,
        cursor,
        ghosts,
    })
}

fn classed_div(doc: &Document, class_name: &str) -> DomResult<Element> {
    let element = create_div(doc)?;
    element.set_class_name(class_name);
    Ok(element)
}

/// Remove an element from wherever it currently sits.
///
/// Going through the parent rather than the `ChildNode.remove` mixin keeps
/// every structural change on the inherent `Node` surface, and makes a detached
/// element a no-op instead of a throw.
pub fn detach(element: &Element) {
    if let Some(parent) = element.parent_node() {
        let _ = parent.remove_child(element);
    }
}

/// Swap one element for another in place, keeping its position among siblings.
pub fn replace_element(old: &Element, new: &Element) {
    if let Some(parent) = old.parent_node() {
        let _ = parent.replace_child(new, old);
    }
}

/// Stamp the exact reserved height of a block or gap of rows onto one element.
///
/// The value is a BARE length. The self-correcting `auto <len>` form makes a
/// browser reuse a block's LAST RENDERED size, so a block that grew while
/// skipped understates `scrollHeight` until it materializes — and every scroll
/// position in the pane is derived from that number.
pub fn size_scrollback_block(block: &Element, rows: u32, row_height: f64) {
    set_style_property(
        block,
        "contain-intrinsic-size",
        &block_placeholder(rows, row_height),
    );
}

/// The ghost overlay boxes for a set of remote cursors.
///
/// A ghost that cannot be created is dropped: an overlay is decoration, and
/// refusing to paint the whole grid because one remote box failed would trade a
/// missing cursor for a frozen pane.
pub fn create_ghost_elements(doc: &Document, ghosts: &[GhostCursor]) -> Vec<Element> {
    ghosts
        .iter()
        .filter_map(|ghost| {
            let box_element = create_div(doc).ok()?;
            box_element.set_class_name("cell-ghost");
            let _ = box_element.set_attribute("data-operator-id", &ghost.operator_id);
            let _ = box_element.set_attribute(
                "title",
                ghost.label.as_deref().unwrap_or(&ghost.operator_id),
            );
            set_style_property(
                &box_element,
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
pub fn measure_cell_row_height(doc: &Document, viewport: &Element) -> f64 {
    let Ok(probe) = doc.create_element("div") else {
        return 0.0;
    };
    probe.set_class_name("cell-row");
    set_style_property(&probe, "position", "absolute");
    set_style_property(&probe, "visibility", "hidden");
    probe.set_text_content(Some(" "));
    if viewport.append_child(&probe).is_err() {
        return 0.0;
    }
    let height = probe.get_bounding_client_rect().height();
    detach(&probe);
    height
}

/// The grid geometry a measured viewport box implies, or `None` before the pane
/// has laid out.
pub fn viewport_cell_geometry(
    cols: u32,
    rows: u32,
    viewport: &Element,
    row_height: f64,
) -> Option<TerminalCellGeometry> {
    let rect = viewport.get_bounding_client_rect();
    grid_geometry_from_box(
        cols,
        rows,
        rect.left(),
        rect.top(),
        rect.width(),
        row_height,
    )
}

/// Toggle the alternate-screen class, returning the state now painted.
pub fn sync_alternate_screen(
    container: &Element,
    frame: Option<&CellGridFrame>,
    painted: Option<bool>,
) -> bool {
    let active = frame.is_some_and(|frame| frame.alt_screen);
    if Some(active) != painted {
        // `classList` exposes only a TOGGLING `toggle`, so the state this call
        // lands on is the guard's own watermark rather than a re-read: the call
        // happens exactly when the class is not yet `active`.
        let _ = container.class_list().toggle("alt-active");
    }
    active
}

/// Publish the grid's column count as a custom property, returning the count
/// now painted. The CSS paints exactly `cols` x `1ch`, so this value is what
/// stops a narrower fallback font from painting columns outside the clip.
pub fn paint_cell_grid_width(
    container: &Element,
    frame: Option<&CellGridFrame>,
    painted: Option<u32>,
) -> Option<u32> {
    let Some(frame) = frame else {
        return painted;
    };
    if Some(frame.cols) == painted {
        return painted;
    }
    set_style_property(container, "--cell-cols", &frame.cols.to_string());
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
