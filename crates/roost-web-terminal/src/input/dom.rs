//! The DOM boundary: a real `KeyboardEvent` becomes a `KeyChord`, a real
//! `Selection` becomes the `LiveSelection` the hold is derived from, and a
//! retained capture becomes the `RetainedRange` it is revalidated against.
//!
//! Nothing here decides anything. Every judgement — whether a key is text, a
//! selection is the pane's, a capture is still restorable — is made natively;
//! this file only reads and writes, and every read it reports is the
//! document's own state rather than a remembered one. Ports the DOM reads of
//! v2's `apps/web/src/renderer/terminalSelectionGuard.ts` (`captureTerminalSelection`,
//! `validCapture`, `paneOwnsSelectionEndpoint`, `focusedYieldOwner`).

use std::cell::Cell;

use wasm_bindgen::JsCast;
use web_sys::{Document, Element, KeyboardEvent, Node, Range};

use crate::input::chord::{KeyChord, KeyKind, Modifiers};
use crate::input::selection::{
    DomNodeId, FocusOwner, LiveSelection, OwnedRow, RetainedRange, SelectionEndpoint,
};

/// Translate one `keydown` into the DOM-free chord the encoder takes.
///
/// `alt_graph` comes from `get_modifier_state` rather than a property: Windows
/// reports AltGraph only there, and reading it as text input is what keeps one
/// character's AltGraph layout from reaching the shell as a control byte plus
/// an escape prefix.
pub fn key_chord_from_event(event: &KeyboardEvent) -> KeyChord {
    KeyChord {
        kind: KeyKind::from_dom_key(&event.key()),
        modifiers: Modifiers {
            shift: event.shift_key(),
            alt: event.alt_key(),
            ctrl: event.ctrl_key(),
            meta: false,
            super_key: event.meta_key() || event.get_modifier_state("OS"),
            hyper: event.get_modifier_state("Hyper"),
            caps_lock: event.get_modifier_state("CapsLock"),
            num_lock: event.get_modifier_state("NumLock"),
        },
        alt_graph: event.get_modifier_state("AltGraph"),
        is_composing: event.is_composing(),
    }
}

/// What a capture retained: v2 clones the range AND keeps the selection's own
/// anchor and focus, because a backward selection's anchor is the range's END.
struct RetainedCapture {
    range: Range,
    anchor: (Node, u32),
    focus: (Node, u32),
}

/// One pane's native selection, plus the capture it retained.
///
/// The guard holds no DOM node, so every node it later compares is named by a
/// `DomNodeId` this reader minted. That is what makes a canonical repair
/// detectable: the replacement element is a different object and so gets a
/// different id, while the same element across a re-read keeps the one it had.
pub struct DomSelectionReader {
    document: Document,
    display: Element,
    node_ids: js_sys::WeakMap,
    nodes: js_sys::Map,
    retained: Option<RetainedCapture>,
    /// Minted ids come from a `Cell`, not a `&mut`: every read path is `&self`
    /// because a read must never be able to disturb the pane, and an id counter
    /// is the one piece of state a read legitimately advances.
    next_id: Cell<u32>,
}

/// No DOM handle is printed: neither `web_sys` nor `js_sys` carries a `Debug`,
/// and a reader's useful state is what it has retained and what it will mint
/// next. Deliberately does not call `node_id`, which would mint an id as a side
/// effect of being printed.
impl std::fmt::Debug for DomSelectionReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DomSelectionReader")
            .field("retained", &self.retained.is_some())
            .field("next_id", &self.next_id.get())
            .finish_non_exhaustive()
    }
}

impl DomSelectionReader {
    /// A reader over one pane's display element.
    pub fn new(document: &Document, display: &Element) -> Self {
        Self {
            document: document.clone(),
            display: display.clone(),
            node_ids: js_sys::WeakMap::new(),
            nodes: js_sys::Map::new(),
            retained: None,
            next_id: Cell::new(1),
        }
    }

    /// The document this reader reads.
    pub fn document(&self) -> &Document {
        &self.document
    }

    /// This pane's display element, whose identity every capture is scoped to.
    pub fn display_id(&self) -> DomNodeId {
        self.node_id(self.display.as_ref())
    }

    /// The stable id for a node, minting one the first time it is seen.
    fn node_id(&self, node: &Node) -> DomNodeId {
        let key: &js_sys::Object = node.as_ref();
        if let Some(number) = self.node_ids.get(key).as_f64() {
            return DomNodeId(number as u32);
        }
        let id = self.next_id.get();
        self.next_id.set(id.saturating_add(1));
        self.node_ids.set(key, &js_sys::Number::from(id));
        self.nodes.set(&js_sys::Number::from(id), node.as_ref());
        DomNodeId(id)
    }

    /// The node a minted id names, or `None` when the id was never minted here
    /// or its element has been detached.
    fn resolve(&self, id: DomNodeId) -> Option<Node> {
        let value = self.nodes.get(&js_sys::Number::from(id.0));
        if value.is_undefined() {
            return None;
        }
        let node: Node = value.unchecked_into();
        node.is_connected().then_some(node)
    }

    /// Drop every id whose node has detached, so a long-lived reader does not
    /// keep a discarded frame's elements alive. A detached node is never a
    /// valid range endpoint, so nothing resolvable is lost.
    fn forget_detached(&self) {
        let mut detached: Vec<f64> = Vec::new();
        self.nodes.for_each(&mut |value, key| {
            let node: Node = value.unchecked_into();
            if !node.is_connected()
                && let Some(id) = key.as_f64()
            {
                detached.push(id);
            }
        });
        for id in detached {
            self.nodes.delete(&js_sys::Number::from(id));
        }
    }

    /// Whether a node is inside this pane's display (the display included).
    fn contains(&self, node: &Node) -> bool {
        let display: &Node = self.display.as_ref();
        display.contains(Some(node))
    }

    /// The `.cell-row` a node sits in, when that row is inside this display.
    /// A text node — the usual endpoint — is resolved through its parent.
    fn owned_row(&self, node: &Node) -> Option<OwnedRow> {
        let element = match node.dyn_ref::<Element>() {
            Some(element) => element.clone(),
            None => node.parent_element()?,
        };
        let row = element.closest(".cell-row").ok().flatten()?;
        if !self.contains(row.as_ref()) {
            return None;
        }
        Some(OwnedRow {
            id: self.node_id(row.as_ref()),
            text: row.text_content().unwrap_or_default(),
        })
    }

    /// The distinct rows two endpoints resolve to, anchor first.
    fn rows_of(&self, anchor: Option<&Node>, focus: Option<&Node>) -> Vec<OwnedRow> {
        let mut rows: Vec<OwnedRow> = Vec::new();
        for node in [anchor, focus].into_iter().flatten() {
            if let Some(row) = self.owned_row(node)
                && !rows.iter().any(|owned| owned.id == row.id)
            {
                rows.push(row);
            }
        }
        rows
    }

    fn endpoint(&self, node: Option<&Node>, offset: u32) -> Option<SelectionEndpoint> {
        Some(SelectionEndpoint {
            node: self.node_id(node?),
            offset,
        })
    }

    /// Read the document's selection: its shape, its text, its endpoints, the
    /// rows those endpoints sit in, and the document's editing target.
    pub fn read(&self) -> LiveSelection {
        self.forget_detached();
        let focus_owner = self.focus_owner();
        let Ok(Some(selection)) = self.document.get_selection() else {
            return LiveSelection {
                focus_owner,
                ..LiveSelection::default()
            };
        };
        let anchor = selection.anchor_node();
        let focus = selection.focus_node();
        LiveSelection {
            present: true,
            collapsed: selection.is_collapsed(),
            range_count: selection.range_count(),
            anchor: self.endpoint(anchor.as_ref(), selection.anchor_offset()),
            focus: self.endpoint(focus.as_ref(), selection.focus_offset()),
            text: String::from(selection.to_string()),
            owned_rows: self.rows_of(anchor.as_ref(), focus.as_ref()),
            endpoint_in_display: [anchor.as_ref(), focus.as_ref()]
                .into_iter()
                .flatten()
                .any(|node| self.contains(node)),
            focus_owner,
        }
    }

    /// The document's editing target, or `None` when nothing is focused.
    ///
    /// `active_element` reads as the body or the document element when the
    /// page has no focused control, and neither is something a range can be
    /// yielded to. Recording an owner and re-deriving it both read focus here.
    fn focus_owner(&self) -> Option<FocusOwner> {
        let active = self.document.active_element()?;
        let active_node: &Node = active.as_ref();
        // Node IDENTITY: the question the DOM actually answers is "is this the
        // same node", and web-sys's many `AsRef` impls make `==` ambiguous.
        let is_page_root = self.document.body().is_some_and(|body| {
            let body_node: &Node = body.as_ref();
            active_node.is_same_node(Some(body_node))
        }) || self.document.document_element().is_some_and(|root| {
            let root_node: &Node = root.as_ref();
            active_node.is_same_node(Some(root_node))
        });
        if is_page_root {
            return None;
        }
        Some(FocusOwner {
            node: self.node_id(active_node),
            connected: active.is_connected(),
        })
    }

    /// Retain the document's current range (cloned, so a later selection
    /// change cannot move it) and the selection's own anchor and focus, so a
    /// restore has something to put back after the yield cleared the document.
    pub fn retain_current_range(&mut self) -> bool {
        let Ok(Some(selection)) = self.document.get_selection() else {
            return false;
        };
        let (Some(anchor), Some(focus)) = (selection.anchor_node(), selection.focus_node()) else {
            return false;
        };
        let Ok(range) = selection.get_range_at(0) else {
            return false;
        };
        self.retained = Some(RetainedCapture {
            range: range.clone_range(),
            anchor: (anchor, selection.anchor_offset()),
            focus: (focus, selection.focus_offset()),
        });
        true
    }

    /// Forget the retained capture.
    pub fn forget_retained(&mut self) {
        self.retained = None;
    }

    /// Re-read the retained capture: its endpoints, its range's own text,
    /// whether every node it names is still connected, in this document and
    /// this display with offsets that still fit, and the rows it sits in now.
    pub fn read_retained(&self, display: DomNodeId) -> Option<RetainedRange> {
        let retained = self.retained.as_ref()?;
        let (anchor_node, anchor_offset) = &retained.anchor;
        let (focus_node, focus_offset) = &retained.focus;
        let document: &Node = self.document.as_ref();
        let endpoint_live = |node: &Node, offset: u32| {
            node.is_connected()
                && node.get_root_node().is_same_node(Some(document))
                && offset <= node_length(node)
        };
        let containers_live = retained
            .range
            .start_container()
            .is_ok_and(|node| node.is_connected())
            && retained
                .range
                .end_container()
                .is_ok_and(|node| node.is_connected());
        let display_node: &Node = self.display.as_ref();
        Some(RetainedRange {
            display,
            anchor: self.endpoint(Some(anchor_node), *anchor_offset)?,
            focus: self.endpoint(Some(focus_node), *focus_offset)?,
            range_text: String::from(retained.range.to_string()),
            containers_connected: display_node.is_connected()
                && endpoint_live(anchor_node, *anchor_offset)
                && endpoint_live(focus_node, *focus_offset)
                && (self.contains(anchor_node) || self.contains(focus_node))
                && containers_live,
            rows: self.rows_of(Some(anchor_node), Some(focus_node)),
        })
    }

    /// Clear the document's ranges, which is the only call through which
    /// Chromium resets its native editing target.
    pub fn clear_ranges(&self) {
        if let Ok(Some(selection)) = self.document.get_selection() {
            let _ = selection.remove_all_ranges();
        }
    }

    /// Re-establish the retained range at its anchor and focus endpoints.
    ///
    /// False means the document refused — the nodes were detached, so the
    /// range is gone and the guard ends the capture rather than retrying it.
    pub fn restore_retained(&self, retained: &RetainedRange) -> bool {
        let Ok(Some(selection)) = self.document.get_selection() else {
            return false;
        };
        let (Some(anchor), Some(focus)) = (
            self.resolve(retained.anchor.node),
            self.resolve(retained.focus.node),
        ) else {
            return false;
        };
        selection
            .set_base_and_extent(
                &anchor,
                retained.anchor.offset,
                &focus,
                retained.focus.offset,
            )
            .is_ok()
    }
}

/// A node's length as a range offset counts it: characters for character
/// data, children for everything else.
fn node_length(node: &Node) -> u32 {
    match node.node_value() {
        Some(value) => value.encode_utf16().count() as u32,
        None => node.child_nodes().length(),
    }
}
