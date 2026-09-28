//! The DOM boundary: a real `KeyboardEvent` becomes a `KeyChord`, a real
//! `Selection` becomes the `LiveSelection` the hold is derived from, and a
//! retained `Range` becomes the `RetainedRange` a capture is revalidated
//! against.
//!
//! Nothing here decides anything. Every judgement — whether a key is text, a
//! selection is the pane's, a capture is still restorable — was already made
//! natively; this file only reads and writes, and every read it reports is the
//! document's own state rather than a remembered one.

use std::cell::Cell;

use wasm_bindgen::JsCast;
use web_sys::{Document, Element, KeyboardEvent, Node, Range, Selection};

use crate::input::chord::{KeyChord, KeyKind, Modifiers, NamedKey};
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
    let key = event.key();
    let kind = match NamedKey::from_dom_key(&key) {
        Some(named) => KeyKind::Named(named),
        None if matches!(key.as_str(), "Dead" | "Process" | "Unidentified") => {
            KeyKind::BrowserOwned
        }
        None => match (key.chars().next(), key.chars().nth(1)) {
            (Some(character), None) => KeyKind::Printable(character),
            _ => KeyKind::BrowserOwned,
        },
    };
    KeyChord {
        kind,
        modifiers: Modifiers {
            shift: event.shift_key(),
            alt: event.alt_key(),
            ctrl: event.ctrl_key(),
            meta: event.meta_key(),
        },
        alt_graph: event.get_modifier_state("AltGraph"),
        is_composing: event.is_composing(),
    }
}

/// One pane's native selection, plus the range a capture retained.
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
    retained: Option<Range>,
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

    /// This pane's display element, whose identity every capture is scoped to.
    pub fn display_id(&self) -> DomNodeId {
        self.node_id(&self.display.clone().into())
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
        let node: Node = value.into();
        node.is_connected().then_some(node)
    }

    /// Drop every id whose node has detached, so a long-lived reader does not
    /// keep a discarded frame's elements alive. A detached node is never a
    /// valid range endpoint, so nothing resolvable is lost.
    fn forget_detached(&self) {
        let mut detached: Vec<DomNodeId> = Vec::new();
        self.nodes.for_each(&mut |value, key| {
            let node: Node = value.into();
            if !node.is_connected()
                && let Some(id) = key.as_f64()
            {
                detached.push(DomNodeId(id as u32));
            }
        });
        for id in detached {
            self.nodes.delete(&js_sys::Number::from(id.0));
        }
    }

    /// The `.cell-row` a node sits in, when it is inside this pane's display.
    fn owned_row(&self, node: &Node) -> Option<OwnedRow> {
        let Ok(element) = node.clone().dyn_into::<Element>() else {
            return None;
        };
        let row = element.closest(".cell-row").ok().flatten()?;
        if !self.contains(&row) {
            return None;
        }
        Some(OwnedRow {
            id: self.node_id(&row.clone().into()),
            text: row.text_content().unwrap_or_default(),
        })
    }

    /// Whether a node is inside this pane's display.
    fn contains(&self, node: &Element) -> bool {
        let display: &Node = self.display.as_ref();
        display.contains(Some(&node.clone().into()))
    }

    fn endpoint(&self, node: Option<Node>, offset: u32) -> Option<SelectionEndpoint> {
        let node = node?;
        Some(SelectionEndpoint {
            node: self.node_id(&node),
            offset,
        })
    }

    /// Read the document's selection: its shape, its endpoints, the rows those
    /// endpoints sit in, and the document's editing target.
    pub fn read(&self) -> LiveSelection {
        self.forget_detached();
        let Ok(Some(selection)) = self.document.get_selection() else {
            return LiveSelection::default();
        };
        let mut live = Self::live_selection(&selection);
        live.anchor = self.endpoint(selection.anchor_node(), selection.anchor_offset());
        live.focus = self.endpoint(selection.focus_node(), selection.focus_offset());
        for node in [selection.anchor_node(), selection.focus_node()]
            .into_iter()
            .flatten()
        {
            if let Some(row) = self.owned_row(&node)
                && !live.owned_rows.iter().any(|owned| owned.id == row.id)
            {
                live.owned_rows.push(row);
            }
        }
        live.focus_owner = self.focus_owner();
        live
    }

    fn live_selection(selection: &Selection) -> LiveSelection {
        LiveSelection {
            present: true,
            collapsed: selection.is_collapsed(),
            range_count: selection.range_count(),
            text: String::new(),
            ..LiveSelection::default()
        }
    }

    /// The document's editing target, or `None` when nothing is focused.
    ///
    /// `active_element` reads as the body or the document element when the
    /// page has no focused control, and neither is something a range can be
    /// yielded to.
    fn focus_owner(&self) -> Option<FocusOwner> {
        let active = self.document.active_element()?;
        let active_node: &Node = active.as_ref();
        // Node IDENTITY, not an `AsRef` comparison: web-sys types carry several
        // `AsRef` impls, so `==` on the references does not resolve, and the
        // question the DOM actually answers is "is this the same node".
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
            node: self.node_id(&active.clone().into()),
            connected: active.is_connected(),
        })
    }

    /// Retain the document's current range, so a restore has something to put
    /// back after the yield cleared the selection.
    pub fn retain_current_range(&mut self) -> Option<Range> {
        let range = self
            .document
            .get_selection()
            .ok()
            .flatten()?
            .get_range_at(0)
            .ok()?;
        self.retained = Some(range.clone());
        Some(range)
    }

    /// Re-read the retained range: its own text, whether its containers are
    /// still connected, and the rows it was captured over.
    pub fn read_retained(&self, display: DomNodeId) -> Option<RetainedRange> {
        let range = self.retained.clone()?;
        let anchor = self.endpoint(range.start_container().ok(), range.start_offset().ok()?)?;
        let focus = self.endpoint(range.end_container().ok(), range.end_offset().ok()?)?;
        let mut rows: Vec<OwnedRow> = Vec::new();
        for container in [range.start_container(), range.end_container()] {
            let Ok(container) = container else {
                continue;
            };
            let Ok(element) = container.dyn_into::<Element>() else {
                continue;
            };
            for ancestor in element.closest(".cell-row").ok().flatten().into_iter() {
                if self.contains(&ancestor) {
                    let row = OwnedRow {
                        id: self.node_id(&ancestor.clone().into()),
                        text: ancestor.text_content().unwrap_or_default(),
                    };
                    if !rows.contains(&row) {
                        rows.push(row);
                    }
                }
            }
        }
        Some(RetainedRange {
            display,
            anchor,
            focus,
            range_text: String::from(range.to_string()),
            containers_connected: range
                .start_container()
                .is_ok_and(|node| node.is_connected())
                && range.end_container().is_ok_and(|node| node.is_connected()),
            rows,
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
