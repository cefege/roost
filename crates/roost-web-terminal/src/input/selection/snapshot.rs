//! The two reads the selection guard is decided from, as plain data: the
//! DOCUMENT's selection right now (`LiveSelection`) and the CAPTURED range as
//! it stands now (`RetainedRange`). `input::dom` fills both from a real
//! document; `selection` judges them. Ports the capture record and the
//! ownership predicates of v2's `apps/web/src/renderer/terminalSelectionGuard.ts`.

/// The adapter's identity for one DOM node. The guard compares these instead
/// of holding a DOM reference, so a capture survives only while the node it
/// named is genuinely still there.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DomNodeId(pub u32);

/// One painted row a capture depends on. A repair that replaced the nodes
/// changes the text or the identity, and either one invalidates the capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedRow {
    /// The row element's identity.
    pub id: DomNodeId,
    /// The row's text as read.
    pub text: String,
}

/// One endpoint of a native selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SelectionEndpoint {
    /// The node's identity.
    pub node: DomNodeId,
    /// The offset within that node.
    pub offset: u32,
}

/// The document's editing target, which is what a suspended range yields to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusOwner {
    /// The focused element's identity.
    pub node: DomNodeId,
    /// Whether it is still connected to the document.
    pub connected: bool,
}

/// What the document's selection looks like right now, as the adapter reads
/// it. Every field is a fact about the LIVE document, which is what lets the
/// hold be derived rather than latched on an edge that may never arrive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveSelection {
    /// Whether the document reports a selection at all.
    pub present: bool,
    /// Whether the selection is collapsed to a caret.
    pub collapsed: bool,
    /// How many ranges the selection holds.
    pub range_count: u32,
    /// The anchor endpoint, or `None` when the document reports none.
    pub anchor: Option<SelectionEndpoint>,
    /// The focus endpoint.
    pub focus: Option<SelectionEndpoint>,
    /// The selected text, which is what a copy reads.
    pub text: String,
    /// The rows the endpoints resolve to, in `cell-row` order. Empty when
    /// neither endpoint is inside a row of the pane's display.
    pub owned_rows: Vec<OwnedRow>,
    /// Whether either endpoint is inside the pane's display at all — a row,
    /// the viewport, or the display element itself.
    pub endpoint_in_display: bool,
    /// The document's editing target when there is a real one. `None` means
    /// nothing is focused, which is nothing to yield a range to.
    pub focus_owner: Option<FocusOwner>,
}

impl LiveSelection {
    /// Whether the pane owns an endpoint of this selection: either endpoint
    /// inside its display. A row the pane painted is inside it by definition.
    pub fn pane_owns_endpoint(&self) -> bool {
        self.endpoint_in_display || !self.owned_rows.is_empty()
    }

    /// A non-collapsed selection with a range. This is the whole shape the
    /// paint hold asks for: the user may still be dragging it out, and its
    /// text is not what makes it worth holding.
    pub fn is_range(&self) -> bool {
        self.present && !self.collapsed && self.range_count > 0
    }

    /// A non-collapsed range carrying text, which is the only selection a user
    /// can act on and the only one a capture retains.
    pub fn is_live_range(&self) -> bool {
        self.is_range() && !self.text.is_empty()
    }
}

/// What the adapter reads about the range a capture retained.
///
/// This is the CAPTURED range, not the document's: a yield clears the
/// document's ranges and leaves the retained one alone, which is the whole
/// reason a restore can put the user's selection back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetainedRange {
    /// The display the capture was taken in.
    pub display: DomNodeId,
    /// The anchor endpoint as captured.
    pub anchor: SelectionEndpoint,
    /// The focus endpoint as captured.
    pub focus: SelectionEndpoint,
    /// The retained range's own text, which a repair would change.
    pub range_text: String,
    /// Whether the captured endpoints and the range's containers are all still
    /// connected, in this document and this display, with offsets that still
    /// fit their nodes.
    pub containers_connected: bool,
    /// The captured rows, re-read: identity and text as they stand now.
    pub rows: Vec<OwnedRow>,
}
