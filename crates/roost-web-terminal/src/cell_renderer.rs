//! The one terminal grid renderer: immutable worker-width cell rows, painted
//! without any client-side reflow.
//!
//! It is ONE type because its per-frame state is private and shared: the
//! painted row list, the scrollback layout, the reader's park and the reconcile
//! watermark all read and write each other, and a second owner of any one of
//! them is how painted history ends up disagreeing with the frame that
//! described it. Its `impl` blocks are split across the `cell_renderer/`
//! modules by concern — that is a file split, not a type split.
//!
//! Scrollback is append-only while ordinary deltas patch only dirty viewport
//! rows. Nothing here re-parses VT, and nothing here reflows text: the worker
//! ships pre-rendered cells and the renderer draws them.
//!
//! Depended on by the pane component (which owns the events), the backfill
//! pager and the smoke API. Depends on `roost-protocol` for the cell model and
//! `roost-client-core` for the absolute history arithmetic.

mod eviction;
mod history_page;
mod ingest;
mod paint;
mod probe;
mod reader;
mod reconcile;
mod scroll_events;
mod scrollback;

use std::collections::BTreeMap;
use std::fmt;

use roost_protocol::cell::CellGridFrame;
use wasm_bindgen::prelude::wasm_bindgen;
use web_sys::{Document, Element, HtmlElement};

use crate::cell_geometry::TerminalCellGeometry;
use crate::cell_renderer_dom::{
    DomSetupError, GhostCursor, create_cell_renderer_elements, create_ghost_elements, detach,
};
use crate::cell_row::FindHit;
use crate::painted_history::PaintedHistory;
use crate::presentation::{RendererIncidentObserver, RendererTerminalModeSnapshot};
use crate::reader_intent::{ReaderAnchor, ReaderIntent, ReaderState};

/// The painted terminal grid for one pane.
///
/// Construct it over a container element that is already in the document; the
/// renderer appends `div.cell-sb-spacer`, `div.cell-scrollback` and
/// `div.cell-viewport` to it, and stamps the container with BOTH `wterm` and
/// `cell-grid` — the first carries the overflow rules that make history
/// scrollable at all, the second scopes every cell-grid rule.
#[wasm_bindgen]
pub struct CellGridRenderer {
    container: Element,
    doc: Document,
    spacer: HtmlElement,
    scrollback: HtmlElement,
    viewport: HtmlElement,
    cursor: HtmlElement,
    ghosts: HtmlElement,

    /// The frame the painted DOM was built from.
    frame: Option<CellGridFrame>,
    /// The newest frame, held back while a reader is parked. It is the CANONICAL
    /// frame: the pane keeps advancing while the DOM is immutable.
    reader_pending_frame: Option<CellGridFrame>,
    /// Whether the pending frame is compatible with the painted history. A
    /// viewport-only checkpoint is not, and a backfill page addressed to the
    /// painted anchor must be refused while it waits.
    reader_pending_frame_retains_history: bool,
    /// A frame was accepted but not yet painted.
    pending_render: bool,

    reader: ReaderState,
    /// A pending selection-release scroll is consumed before native reader
    /// intent, so a selection that ends above the bottom is not read as a
    /// reader gesture.
    live_selection_release_pending: bool,
    /// Monotonic arm counter for the bottom-park settle; a stale callback whose
    /// epoch no longer matches is inert.
    bottom_park_settle_epoch: u64,
    reader_anchor: Option<ReaderAnchor>,

    /// The next epoch this renderer will stamp on a scroll write it owns.
    next_owned_scroll_epoch: u64,
    /// The epoch of the last owned write, or zero when none is outstanding.
    owned_scroll_epoch: u64,
    /// The position that owned write landed on.
    owned_scroll_top: f64,
    /// The last scroll maximum observed WITH a position. A smaller one means a
    /// clamp no gesture aimed at.
    last_scroll_max: f64,
    /// The last box height this renderer saw. It is consumed even on a refusal,
    /// so a resize observer cannot retry the same transition forever.
    last_box_height: f64,

    /// The OPEN tail block, which still accepts rows and therefore opts out of
    /// content-visibility until it seals.
    cur_block: Option<Element>,
    cur_block_rows: u32,
    /// The gap that ends at the layout end, when the tail is unpainted.
    tail_gap: Option<Element>,

    row_elements: Vec<Element>,
    row_hashes: Vec<u32>,
    row_height: f64,
    painted_gap_row_height: f64,

    find_hits: BTreeMap<u32, Vec<FindHit>>,
    active_hit: Option<(u32, u32)>,

    painted_cols: Option<u32>,
    painted_alt_screen: Option<bool>,
    painted_cursor_visible: Option<bool>,
    painted_cursor_row: i64,
    painted_cursor_col: i64,
    predicted_col: Option<u32>,
    painted_spacer_height: String,

    painted: PaintedHistory,
    painted_sb_base: u32,
    scrollback_layout_end: u64,
    gap_rows: u64,
    history_floor_row: u32,

    reconciled_grid_epoch: Option<String>,
    reconciled_seq: Option<u64>,
    reconciled_mode: Option<RendererTerminalModeSnapshot>,

    /// Fired once, when the DOM is reconciled for the first time.
    on_first_reconcile: Option<Box<dyn FnOnce()>>,
    /// Fired on every reconcile, so the pane can schedule its next repaint.
    on_reconcile: Option<Box<dyn Fn()>>,
    /// Asks the pane to open the scroll-idle window a band rest needs before it
    /// may resume. The renderer only ASKS: writing `scrollTop` mid-gesture
    /// cancels the scroll the reader is still performing.
    request_follow_band_settle: Option<Box<dyn Fn()>>,
    /// Installed only by an armed terminal incident recorder. `None` in
    /// production, and an unarmed recorder allocates nothing.
    incident_observer: Option<Box<dyn RendererIncidentObserver>>,
}

impl fmt::Debug for CellGridRenderer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CellGridRenderer")
            .field("intent", &self.reader.intent())
            .field("hold_mask", &self.reader.hold_mask())
            .field("dom_rows", &self.row_elements.len())
            .field("painted_rows", &self.painted.len())
            .field("painted_sb_base", &self.painted_sb_base)
            .field("layout_end", &self.scrollback_layout_end)
            .finish_non_exhaustive()
    }
}

impl CellGridRenderer {
    /// Build a renderer over `container`.
    pub fn new(container: &Element) -> Result<Self, DomSetupError> {
        Self::with_callbacks(container, None, None, None)
    }

    /// Build a renderer with the pane's three hooks: first reconcile, every
    /// reconcile, and the follow-band settle request.
    pub fn with_callbacks(
        container: &Element,
        on_first_reconcile: Option<Box<dyn FnOnce()>>,
        on_reconcile: Option<Box<dyn Fn()>>,
        request_follow_band_settle: Option<Box<dyn Fn()>>,
    ) -> Result<Self, DomSetupError> {
        let elements = create_cell_renderer_elements(container)?;
        elements.cursor.set_attribute("data-blink", "false").ok();
        let last_box_height = f64::from(container.client_height());
        Ok(Self {
            container: container.clone(),
            doc: elements.doc,
            spacer: elements.spacer,
            scrollback: elements.scrollback,
            viewport: elements.viewport,
            cursor: elements.cursor,
            ghosts: elements.ghosts,
            frame: None,
            reader_pending_frame: None,
            reader_pending_frame_retains_history: true,
            pending_render: false,
            reader: ReaderState::new(),
            live_selection_release_pending: false,
            bottom_park_settle_epoch: 0,
            reader_anchor: None,
            next_owned_scroll_epoch: 0,
            owned_scroll_epoch: 0,
            owned_scroll_top: 0.0,
            last_scroll_max: 0.0,
            last_box_height,
            cur_block: None,
            cur_block_rows: 0,
            tail_gap: None,
            row_elements: Vec::new(),
            row_hashes: Vec::new(),
            row_height: 0.0,
            painted_gap_row_height: 0.0,
            find_hits: BTreeMap::new(),
            active_hit: None,
            painted_cols: None,
            painted_alt_screen: None,
            painted_cursor_visible: None,
            painted_cursor_row: -1,
            painted_cursor_col: -1,
            predicted_col: None,
            painted_spacer_height: String::new(),
            painted: PaintedHistory::new(),
            painted_sb_base: 0,
            scrollback_layout_end: 0,
            gap_rows: 0,
            history_floor_row: 0,
            reconciled_grid_epoch: None,
            reconciled_seq: None,
            reconciled_mode: None,
            on_first_reconcile,
            on_reconcile,
            request_follow_band_settle,
            incident_observer: None,
        })
    }

    /// The scroll container this renderer paints into.
    pub fn container(&self) -> &Element {
        &self.container
    }

    /// The immutable history sheet, whose first child is the eviction unit.
    pub fn scrollback_element(&self) -> &HtmlElement {
        &self.scrollback
    }

    /// The live grid host, which the echo predictor also paints into.
    pub fn prediction_host(&self) -> &Element {
        self.viewport.as_ref()
    }

    /// The head spacer, a SIBLING of the history sheet.
    pub fn spacer_element(&self) -> &HtmlElement {
        &self.spacer
    }

    /// The cursor element the viewport overlay owns.
    pub fn cursor_element(&self) -> &Element {
        self.cursor.as_ref()
    }

    /// The document every painted node is created from.
    pub fn document(&self) -> &Document {
        &self.doc
    }

    /// The frame the painted DOM was built from.
    pub fn current_frame(&self) -> Option<&CellGridFrame> {
        self.frame.as_ref()
    }

    /// Install the incident recorder. `None` is the production state: the
    /// renderer's boundaries are then free.
    pub fn set_incident_observer(
        &mut self,
        observer: Option<Box<dyn RendererIncidentObserver>>,
    ) {
        self.incident_observer = observer;
    }

    /// Enable or disable the focused-pane cursor blink presentation policy.
    pub fn set_cursor_blink_enabled(&mut self, enabled: bool) {
        let value = if enabled { "true" } else { "false" };
        if self.cursor.get_attribute("data-blink").as_deref() == Some(value) {
            return;
        }
        let _ = self.cursor.set_attribute("data-blink", value);
    }

    /// Attach remote cursor overlays to the viewport.
    pub fn set_ghosts(&mut self, ghosts: &[GhostCursor]) {
        let boxes = create_ghost_elements(&self.doc, ghosts);
        self.ghosts.set_inner_html("");
        for element in boxes {
            self.ghosts.append_child(&element).ok();
        }
        if self.ghosts.parent_element().as_ref() != Some(self.viewport.as_ref()) {
            self.viewport.append_child(&self.ghosts).ok();
        }
    }

    /// Set the pane's accessible name on the scroll container.
    pub fn set_accessible_label(&mut self, label: &str) {
        let _ = self.container.set_attribute("aria-label", label);
    }

    /// The measured row height, or zero before the pane has measured one.
    pub fn row_height(&self) -> f64 {
        self.row_height
    }

    /// The painted grid's geometry, derived from the canonical frame's box.
    pub fn viewport_cell_geometry(&self) -> Option<TerminalCellGeometry> {
        let frame = self.canonical_frame()?;
        crate::cell_renderer_dom::viewport_cell_geometry(
            frame.cols,
            frame.rows,
            self.viewport.as_ref(),
            self.row_height(),
        )
    }

    /// Forget the measured row height, the reserved spacer height and the
    /// placeholder pitch, so the next paint re-measures. A font swap changes
    /// the cell box, and every scroll offset in the pane is derived from it.
    pub fn invalidate_row_height(&mut self) {
        self.row_height = 0.0;
        self.painted_spacer_height.clear();
        self.painted_gap_row_height = 0.0;
    }

    /// Repair the exact-height history placeholders and the bottom placement
    /// after the pane's font settles.
    ///
    /// A font swap invalidates every measured placeholder, so a bottom reader
    /// would otherwise keep the pixel position it had under the fallback face
    /// and land mid-history. The placement is preserved, not recomputed.
    pub fn on_fonts_settled(&mut self) -> bool {
        let was_at_bottom = self.at_bottom();
        self.invalidate_row_height();
        self.measure_row_height();
        let row_height = self.row_height();
        self.resize_history_placeholders(row_height);
        self.sync_spacer();
        self.pin_to_bottom(was_at_bottom);
        true
    }

    /// Release every DOM node the renderer owns and reset its state.
    pub fn dispose(&mut self) {
        self.incident_observer = None;
        detach(&self.spacer);
        detach(&self.scrollback);
        detach(&self.viewport);
        self.resize_history_placeholders(row_height);
        self.sync_spacer();
        self.pin_to_bottom(was_at_bottom);
        true
    }

    /// Release every DOM node the renderer owns and reset its state.
    pub fn dispose(&mut self) {
        self.incident_observer = None;
        self.spacer.remove();
        self.scrollback.remove();
        self.viewport.remove();
        self.frame = None;
        self.reader_pending_frame = None;
        self.reader_pending_frame_retains_history = true;
        self.reconciled_grid_epoch = None;
        self.reconciled_seq = None;
        self.reconciled_mode = None;
        self.reader = ReaderState::new();
        self.reader_anchor = None;
        self.owned_scroll_epoch = 0;
        self.owned_scroll_top = 0.0;
        self.bottom_park_settle_epoch = self.bottom_park_settle_epoch.wrapping_add(1);
        self.live_selection_release_pending = false;
        self.pending_render = false;
        self.painted_sb_base = 0;
        self.history_floor_row = 0;
        self.painted.clear();
        self.scrollback_layout_end = 0;
        self.gap_rows = 0;
        self.tail_gap = None;
        self.row_elements.clear();
        self.row_hashes.clear();
        self.cur_block = None;
        self.cur_block_rows = 0;
        self.find_hits.clear();
        self.active_hit = None;
    }
}
