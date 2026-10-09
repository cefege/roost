//! The one terminal grid renderer: immutable worker-width cell rows, painted
//! without client-side reflow. ONE type because its per-frame state (painted rows,
//! scrollback layout, reader park, reconcile watermark) is shared, and a second
//! owner is how painted history disagrees with its frame; the `impl` blocks split
//! across `cell_renderer/` by concern, all painting through `RenderElement`.
//! Ports `apps/web/src/renderer/cellRenderer.ts`.

mod eviction;
mod history_page;
mod images;
mod ingest;
mod paint;
mod probe;
mod reader;
mod reconcile;
mod scroll_events;
mod scrollback;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::rc::Rc;

use roost_protocol::cell::CellGridFrame;

use crate::cell_geometry::TerminalCellGeometry;
use crate::cell_renderer_dom::{
    DomSetupError, GhostCursor, create_cell_renderer_elements, create_ghost_elements, is_placed_in,
    measure_cell_row_height,
};
use crate::cell_row::FindHit;
use crate::cell_row::style_cache::StyleCache;
use crate::painted_history::PaintedHistory;
use crate::presentation::{RendererIncidentObserver, RendererTerminalModeSnapshot};
use crate::reader_intent::{ReaderAnchor, ReaderState};
use crate::render_element::RenderElement;

/// The painted terminal grid for one pane.
///
/// Construct it over a container element that is already in the document; the
/// renderer appends `div.cell-sb-spacer`, `div.cell-scrollback` and
/// `div.cell-viewport` to it, and stamps the container with BOTH `wterm` and
/// `cell-grid` — the first carries the overflow rules that make history
/// scrollable at all, the second scopes every cell-grid rule.
pub struct CellGridRenderer<E = web_sys::Element> {
    container: E,
    /// Inline styles memoised across frames; a `RefCell` because history rows
    /// paint through `&self`.
    styles: RefCell<StyleCache>,
    spacer: E,
    scrollback: E,
    viewport: E,
    image_below: E,
    images: E,
    cursor: E,
    ghosts: E,
    image_urls: HashMap<u64, String>,
    image_failed: HashSet<u64>,
    /// Shared by refcount, so a render pass that holds it across `&mut self`
    /// borrows a pointer rather than copying the grid.
    frame: Option<Rc<CellGridFrame>>,
    /// The newest frame, held back while a reader is parked. It is the CANONICAL
    /// frame: the pane keeps advancing while the DOM is immutable.
    reader_pending_frame: Option<Rc<CellGridFrame>>,
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
    cur_block: Option<E>,
    cur_block_rows: u32,
    /// The gap that ends at the layout end, when the tail is unpainted.
    tail_gap: Option<E>,

    row_elements: Vec<E>,
    row_hashes: Vec<u32>,
    /// The measured row pitch, zero until measured. Measured LAZILY on read, so
    /// every reader of the pitch sees the one a paint would use — a cleared
    /// cache re-measures at its next use instead of answering zero.
    row_height: Cell<f64>,
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

impl<E> fmt::Debug for CellGridRenderer<E> {
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

impl<E: RenderElement> CellGridRenderer<E> {
    /// Build a renderer over `container`.
    pub fn new(container: &E) -> Result<Self, DomSetupError> {
        Self::with_callbacks(container, None, None, None)
    }

    /// Build a renderer with the pane's three hooks: first reconcile, every
    /// reconcile, and the follow-band settle request.
    pub fn with_callbacks(
        container: &E,
        on_first_reconcile: Option<Box<dyn FnOnce()>>,
        on_reconcile: Option<Box<dyn Fn()>>,
        request_follow_band_settle: Option<Box<dyn Fn()>>,
    ) -> Result<Self, DomSetupError> {
        let elements = create_cell_renderer_elements(container)?;
        elements.cursor.set_attribute("data-blink", "false");
        let last_box_height = container.client_height();
        Ok(Self {
            container: container.clone(),
            styles: RefCell::new(StyleCache::default()),
            spacer: elements.spacer,
            scrollback: elements.scrollback,
            viewport: elements.viewport,
            image_below: elements.image_below,
            images: elements.images,
            cursor: elements.cursor,
            ghosts: elements.ghosts,
            image_urls: HashMap::new(),
            image_failed: HashSet::new(),
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
            row_height: Cell::new(0.0),
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
    pub fn container(&self) -> &E {
        &self.container
    }

    /// The immutable history sheet, whose first child is the eviction unit.
    pub fn scrollback_element(&self) -> &E {
        &self.scrollback
    }

    /// The live grid host, which the echo predictor also paints into.
    pub fn prediction_host(&self) -> &E {
        &self.viewport
    }

    /// The head spacer, a SIBLING of the history sheet.
    pub fn spacer_element(&self) -> &E {
        &self.spacer
    }

    /// The cursor element the viewport overlay owns.
    pub fn cursor_element(&self) -> &E {
        &self.cursor
    }

    /// The frame the painted DOM was built from.
    pub fn current_frame(&self) -> Option<&CellGridFrame> {
        self.frame.as_deref()
    }

    /// Install the incident recorder. `None` is the production state: the
    /// renderer's boundaries are then free.
    pub fn set_incident_observer(&mut self, observer: Option<Box<dyn RendererIncidentObserver>>) {
        self.incident_observer = observer;
    }

    /// Enable or disable the focused-pane cursor blink presentation policy.
    pub fn set_cursor_blink_enabled(&mut self, enabled: bool) {
        let value = if enabled { "true" } else { "false" };
        if self.cursor.attribute("data-blink").as_deref() == Some(value) {
            return;
        }
        self.cursor.set_attribute("data-blink", value);
    }

    /// Attach remote cursor overlays to the viewport.
    pub fn set_ghosts(&mut self, ghosts: &[GhostCursor]) {
        let boxes = create_ghost_elements(&self.container, ghosts);
        self.ghosts.clear_children();
        for element in &boxes {
            self.ghosts.append_child(element);
        }
        if !is_placed_in(&self.ghosts, &self.viewport) {
            self.viewport.append_child(&self.ghosts);
        }
    }

    /// Set the pane's accessible name on the scroll container.
    pub fn set_accessible_label(&mut self, label: &str) {
        self.container.set_attribute("aria-label", label);
    }

    /// The row pitch, measured with a probe the first time it is asked for
    /// after a paint or a font swap cleared it; zero while the pane cannot
    /// measure one.
    pub fn row_height(&self) -> f64 {
        let cached = self.row_height.get();
        if cached > 0.0 {
            return cached;
        }
        let measured = measure_cell_row_height(&self.viewport);
        if measured > 0.0 {
            self.row_height.set(measured);
        }
        self.row_height.get()
    }

    /// The painted grid's geometry, derived from the canonical frame's box.
    pub fn viewport_cell_geometry(&self) -> Option<TerminalCellGeometry> {
        crate::cell_renderer_dom::viewport_cell_geometry(
            self.canonical_frame(),
            &self.viewport,
            self.row_height(),
        )
    }

    /// Forget the measured row height, the reserved spacer height and the
    /// placeholder pitch, so the next paint re-measures.
    pub fn invalidate_row_height(&mut self) {
        self.row_height.set(0.0);
        self.painted_spacer_height.clear();
        self.painted_gap_row_height = 0.0;
    }

    /// Repair the exact-height history placeholders and the bottom placement
    /// once the pane's font has settled; the pane calls it when the document's
    /// `fonts.ready` resolves. Returns false, changing nothing, when no row can
    /// be measured yet.
    ///
    /// A font swap invalidates every measured placeholder, so a bottom reader
    /// would otherwise keep the pixel position it had under the fallback face
    /// and land mid-history. The placement is preserved, not recomputed.
    pub fn on_fonts_settled(&mut self) -> bool {
        let was_at_bottom = self.at_bottom();
        self.row_height.set(0.0);
        let row_height = self.row_height();
        if row_height <= 0.0 {
            return false;
        }
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
    }
}
