//! The smoke backdoor's DOM reads: a pane's slot, grid, rows, focus and scroll
//! box, computed-style visibility, rectangles, a marker's text range, the
//! painted cursor, and the terminal deck's size. wasm32 only; every decision
//! over these values lives in the native `smoke::{probes,paint_proof}`. Ports
//! the DOM half of `apps/web/src/smoke/smokeTerminalRenderProbes.ts` and
//! `apps/web/src/smoke/smokeHarness.ts:85-304`.

use wasm_bindgen::JsCast as _;
use web_sys::{Document, Element, HtmlElement, Node, Window};

use super::paint_proof::{RectSnapshot, marker_text_range, style_hides};
use super::probes::{
    GridScrollBox, PaneFocus, SmokeRenderProbe, TerminalDimensions, collapse_whitespace,
    parse_cell_cols, terminal_slot_selector,
};

pub(super) fn window() -> Option<Window> {
    web_sys::window()
}

pub(super) fn document() -> Option<Document> {
    window()?.document()
}

/// `[data-testid="terminal-slot-<session>"]`.
pub(super) fn slot(session_id: &str) -> Option<Element> {
    document()?
        .query_selector(&terminal_slot_selector(session_id))
        .ok()?
}

/// The `.cell-grid` a slot mounts.
pub(super) fn grid_in(slot: &Element) -> Option<HtmlElement> {
    slot.query_selector(".cell-grid")
        .ok()??
        .dyn_into::<HtmlElement>()
        .ok()
}

/// Every element under `root` matching `selector`, in document order.
pub(super) fn all(root: &Element, selector: &str) -> Vec<Element> {
    let Ok(list) = root.query_selector_all(selector) else {
        return Vec::new();
    };
    (0..list.length())
        .filter_map(|index| list.item(index)?.dyn_into::<Element>().ok())
        .collect()
}

pub(super) fn text_of(node: &Node) -> String {
    node.text_content().unwrap_or_default()
}

/// The raw text of every painted `.cell-row` of a session, in render order.
pub(super) fn painted_row_texts(session_id: &str) -> Vec<String> {
    slot(session_id)
        .and_then(|slot| grid_in(&slot))
        .map(|grid| {
            all(&grid, ".cell-row")
                .iter()
                .map(|row| text_of(row))
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn pane_focus(session_id: &str) -> PaneFocus {
    let slot = slot(session_id);
    let textarea = slot
        .as_ref()
        .and_then(|slot| slot.query_selector("textarea").ok().flatten());
    let active = document().and_then(|document| document.active_element());
    PaneFocus {
        has_slot: slot.is_some(),
        has_textarea: textarea.is_some(),
        focused: matches!((&textarea, &active), (Some(field), Some(active)) if field.is_same_node(Some(active))),
    }
}

pub(super) fn viewport_text(session_id: &str) -> String {
    slot(session_id).map_or_else(String::new, |slot| collapse_whitespace(&text_of(&slot)))
}

pub(super) fn render_probe(session_id: &str) -> SmokeRenderProbe {
    let Some(grid) = slot(session_id).and_then(|slot| grid_in(&slot)) else {
        return SmokeRenderProbe::absent();
    };
    let rows: Vec<String> = all(&grid, ".cell-row")
        .iter()
        .map(|row| text_of(row))
        .collect();
    let scroll = GridScrollBox {
        scroll_top: f64::from(grid.scroll_top()),
        scroll_height: f64::from(grid.scroll_height()),
        client_height: f64::from(grid.client_height()),
    };
    SmokeRenderProbe::of_grid(scroll, &rows)
}

pub(super) fn terminal_dimensions(session_id: &str) -> TerminalDimensions {
    let grid = slot(session_id).and_then(|slot| grid_in(&slot));
    let cols = grid
        .as_ref()
        .and_then(|grid| grid.style().get_property_value("--cell-cols").ok())
        .map_or(0, |value| parse_cell_cols(&value));
    let rows = grid
        .as_ref()
        .and_then(|grid| grid.query_selector(".cell-viewport").ok().flatten())
        .map_or(0, |viewport| {
            let children = viewport.children();
            (0..children.length())
                .filter_map(|index| children.item(index))
                .filter(|child| child.class_list().contains("cell-row"))
                .count()
        });
    TerminalDimensions { cols, rows }
}

pub(super) fn rect_of(element: &Element) -> RectSnapshot {
    let rect = element.get_bounding_client_rect();
    RectSnapshot::from_origin(rect.left(), rect.top(), rect.width(), rect.height())
}

/// The visual viewport, or the layout viewport where there is none.
pub(super) fn visual_viewport_rect() -> RectSnapshot {
    let Some(window) = window() else {
        return RectSnapshot::from_origin(0.0, 0.0, 0.0, 0.0);
    };
    if let Some(viewport) = window.visual_viewport() {
        return RectSnapshot::from_origin(
            viewport.offset_left(),
            viewport.offset_top(),
            viewport.width(),
            viewport.height(),
        );
    }
    let root = window
        .document()
        .and_then(|document| document.document_element());
    let (width, height) = root.map_or((0, 0), |root| (root.client_width(), root.client_height()));
    RectSnapshot::from_origin(0.0, 0.0, f64::from(width), f64::from(height))
}

/// `(display, visibility, opacity, content-visibility, background-color)`.
fn computed(element: &Element) -> Option<[String; 5]> {
    let style = window()?.get_computed_style(element).ok()??;
    let read = |name: &str| style.get_property_value(name).unwrap_or_default();
    Some([
        read("display"),
        read("visibility"),
        read("opacity"),
        read("content-visibility"),
        read("background-color"),
    ])
}

/// No ancestor-or-self hides `element` (`hasVisibleComputedStyle`).
pub(super) fn visibly_styled(element: &Element) -> bool {
    let mut current = Some(element.clone());
    while let Some(node) = current {
        let Some([display, visibility, opacity, content, _]) = computed(&node) else {
            return false;
        };
        if style_hides(&display, &visibility, &opacity, &content, true) {
            return false;
        }
        current = node.parent_element();
    }
    true
}

/// The cursor and its ancestors up to `terminal` paint: the cursor's own blink
/// opacity is tolerated, its background must not be transparent.
pub(super) fn cursor_paintable(cursor: &Element, terminal: &Element) -> bool {
    let mut current = Some(cursor.clone());
    while let Some(node) = current {
        let own = node.is_same_node(Some(cursor));
        let Some([display, visibility, opacity, content, background]) = computed(&node) else {
            return false;
        };
        if style_hides(&display, &visibility, &opacity, &content, !own) {
            return false;
        }
        if own && super::paint_proof::background_is_transparent(&background) {
            return false;
        }
        if node.is_same_node(Some(terminal)) {
            return true;
        }
        current = node.parent_element();
    }
    false
}

/// Non-empty text nodes under `root`, in document order (a `SHOW_TEXT` walk).
fn text_nodes(root: &Node, into: &mut Vec<Node>) {
    let mut child = root.first_child();
    while let Some(node) = child {
        if node.node_type() == Node::TEXT_NODE {
            if node.node_value().is_some_and(|value| !value.is_empty()) {
                into.push(node.clone());
            }
        } else {
            text_nodes(&node, into);
        }
        child = node.next_sibling();
    }
}

/// The client rect of `marker` inside `row`, and the element its range
/// starts in, or `None` when the row's text nodes do not hold it.
pub(super) fn marker_rect(row: &Element, marker: &str) -> Option<(RectSnapshot, Element)> {
    let mut nodes = Vec::new();
    text_nodes(row, &mut nodes);
    let texts: Vec<String> = nodes
        .iter()
        .map(|node| node.node_value().unwrap_or_default())
        .collect();
    let span = marker_text_range(&texts, marker)?;
    let range = document()?.create_range().ok()?;
    range
        .set_start(&nodes[span.start_node], span.start_offset)
        .ok()?;
    range.set_end(&nodes[span.end_node], span.end_offset).ok()?;
    let rect = range.get_bounding_client_rect();
    let start_element = range.start_container().ok()?.parent_element()?;
    Some((
        RectSnapshot::from_origin(rect.left(), rect.top(), rect.width(), rect.height()),
        start_element,
    ))
}

/// The terminal deck (`[data-testid="terminal-deck"]`).
pub(super) fn terminal_deck() -> Option<HtmlElement> {
    document()?
        .query_selector("[data-testid=\"terminal-deck\"]")
        .ok()??
        .dyn_into::<HtmlElement>()
        .ok()
}
