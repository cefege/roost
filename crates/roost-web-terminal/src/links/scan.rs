//! Watches renderer row mutations and linkifies only the soft-wrap groups they
//! touch. The attachment owns user interaction; this scanner owns scheduling,
//! hidden-page recovery and bounded hot-tail rescans for streaming terminals,
//! over `LinkScanHost` (the browser in `links::dom`, a fake in tests). Every
//! owed callback is named, so a dropped animation frame cannot latch it shut.
//! Ports `apps/web/src/renderer/terminal-links.scan.ts`.

use super::TerminalLinkOptions;
use super::anchor::{LinkDom, js_parse_int, linkify_terminal_rows, terminal_row_columns};
use crate::cell_row::ROW_COLUMNS_ATTR;

/// Dirty rows past which a scan bounds itself to the hot tail.
pub const DIRTY_LIMIT: usize = 300;

/// `requestIdleCallback` timeout for a mutation scan, in milliseconds.
const IDLE_SCAN_TIMEOUT_MS: u32 = 250;

/// Which scanner callback an animation frame runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameCallback {
    /// The scheduled scan (the fallback when idle callbacks are unavailable).
    Scan,
    /// The post-paint frame after activation, which requests the tail scan.
    ActivationScan,
}

/// A set of rows by identity (v2 `Set<HTMLElement>`).
pub trait RowSet<R> {
    /// `add`.
    fn insert(&mut self, row: &R);
    /// `delete`.
    fn remove(&mut self, row: &R);
    /// `has`.
    fn contains(&self, row: &R) -> bool;
    /// `clear`.
    fn clear(&mut self);
    /// `size`.
    fn len(&self) -> usize;
    /// `size === 0`.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// `Array.from`, in insertion order.
    fn rows(&self) -> Vec<R>;
}

/// The page, frame and row-tree operations the scanner needs.
pub trait LinkScanHost: LinkDom {
    /// The dirty-row set this host stores rows in.
    type Rows: RowSet<Self::Element>;
    /// A new empty row set.
    fn new_row_set(&self) -> Self::Rows;
    /// v2 `isPageVisible()`.
    fn is_page_visible(&self) -> bool;
    /// The container's `--cell-cols` style property, verbatim.
    fn cell_cols_property(&self) -> String;
    /// Rows of the newest scrollback block, then of the viewport.
    fn hot_rows(&self) -> Vec<Self::Element>;
    /// The row before `row`, across cell blocks and the scrollback seam.
    fn previous_row(&self, row: &Self::Element) -> Option<Self::Element>;
    /// The row after `row`, across cell blocks and the scrollback seam.
    fn next_row(&self, row: &Self::Element) -> Option<Self::Element>;
    /// `isConnected`.
    fn is_connected(&self, row: &Self::Element) -> bool;
    /// `requestIdleCallback`, or `None` where the browser has none.
    fn request_idle_callback(&self, timeout_ms: u32) -> Option<u32>;
    /// `cancelIdleCallback`.
    fn cancel_idle_callback(&self, handle: u32);
    /// `requestAnimationFrame` for one scanner callback.
    fn request_animation_frame(&self, callback: FrameCallback) -> u32;
    /// `cancelAnimationFrame`.
    fn cancel_animation_frame(&self, handle: u32);
    /// Observe (`true`) or disconnect (`false`) the container's mutations.
    fn observe_mutations(&self, observe: bool);
    /// Add or remove the document `visibilitychange` listener.
    fn listen_for_visibility(&self, listen: bool);
}

/// The scanner's state; one per attachment.
pub struct LinkScanner<H: LinkScanHost> {
    active: bool,
    disposed: bool,
    observing: bool,
    listening_for_visibility: bool,
    scan_scheduled: bool,
    /// The owed scan callback and whether it is an idle callback.
    scan_handle: Option<(u32, bool)>,
    activation_frame: Option<u32>,
    hot_tail_scan_needed: bool,
    discard_dirty_until_activation_scan: bool,
    dirty_rows: H::Rows,
}

impl<H: LinkScanHost> LinkScanner<H> {
    /// A scanner, activated when `initial_active`.
    pub fn attach(host: &H, initial_active: bool) -> Self {
        let mut scanner = Self {
            active: false,
            disposed: false,
            observing: false,
            listening_for_visibility: false,
            scan_scheduled: false,
            scan_handle: None,
            activation_frame: None,
            hot_tail_scan_needed: false,
            discard_dirty_until_activation_scan: false,
            dirty_rows: host.new_row_set(),
        };
        if initial_active {
            scanner.set_active(host, true);
        }
        scanner
    }

    fn cancel_scan(&mut self, host: &H) {
        match self.scan_handle.take() {
            Some((handle, true)) => host.cancel_idle_callback(handle),
            Some((handle, false)) => host.cancel_animation_frame(handle),
            None => {}
        }
        self.scan_scheduled = false;
    }

    /// The scheduled scan callback.
    pub fn scan(&mut self, host: &H, options: &TerminalLinkOptions) {
        self.scan_scheduled = false;
        self.scan_handle = None;
        if !self.active || !host.is_page_visible() {
            return;
        }
        let cols = js_parse_int(&host.cell_cols_property()).unwrap_or(0);
        let owner_repo = options
            .github_owner_repo
            .as_ref()
            .and_then(|getter| getter());
        let linkify = |rows: &[H::Element]| {
            linkify_terminal_rows(
                host,
                rows,
                cols,
                options.file_resolver(),
                owner_repo.as_deref(),
                options.modifier_key,
            );
        };
        if self.hot_tail_scan_needed {
            self.hot_tail_scan_needed = false;
            let discard_dirty = self.discard_dirty_until_activation_scan;
            self.discard_dirty_until_activation_scan = false;
            let hot = host.hot_rows();
            if discard_dirty {
                self.dirty_rows.clear();
            } else {
                hot.iter().for_each(|row| self.dirty_rows.remove(row));
            }
            if !hot.is_empty() {
                linkify(&hot);
            }
            tracing::debug!(target: "terminal_links", hot_rows = hot.len(), discard_dirty, "hot-tail link scan");
            if !discard_dirty && !self.dirty_rows.is_empty() {
                self.schedule_scan(host);
            }
            return;
        }
        let dirty_overflow = self.dirty_rows.len() > DIRTY_LIMIT;
        let hot = if dirty_overflow {
            host.hot_rows()
        } else {
            Vec::new()
        };
        let mut hot_set = host.new_row_set();
        hot.iter().for_each(|row| hot_set.insert(row));
        let hot_stream_overflow = dirty_overflow
            && !hot.is_empty()
            && !self
                .dirty_rows
                .rows()
                .iter()
                .any(|row| host.is_connected(row) && !hot_set.contains(row));
        if hot_stream_overflow {
            self.dirty_rows.clear();
            linkify(&hot);
            return;
        }
        if self.dirty_rows.is_empty() {
            return;
        }
        let dirty = self.dirty_rows.rows();
        self.dirty_rows.clear();
        let mut visited = host.new_row_set();
        let row_columns = |row: &H::Element| {
            terminal_row_columns(host.attribute(row, ROW_COLUMNS_ATTR).as_deref())
        };
        for seed in dirty {
            if !host.is_connected(&seed) || visited.contains(&seed) {
                continue;
            }
            let mut first = seed;
            while cols > 0
                && let Some(previous) = host.previous_row(&first)
                && !visited.contains(&previous)
                && row_columns(&previous) == cols
            {
                first = previous;
            }
            let mut group = Vec::new();
            let mut current = Some(first);
            while let Some(row) = current {
                visited.insert(&row);
                let wraps = cols > 0 && row_columns(&row) == cols;
                group.push(row);
                if !wraps {
                    break;
                }
                current = group
                    .last()
                    .and_then(|row| host.next_row(row))
                    .filter(|next| !visited.contains(next));
            }
            linkify(&group);
        }
    }

    fn schedule_scan(&mut self, host: &H) {
        if !self.active || self.scan_scheduled {
            return;
        }
        self.scan_scheduled = true;
        self.scan_handle = Some(match host.request_idle_callback(IDLE_SCAN_TIMEOUT_MS) {
            Some(handle) => (handle, true),
            None => (host.request_animation_frame(FrameCallback::Scan), false),
        });
    }

    /// Ask for one scan of the current tail (newest block + viewport).
    pub fn request_current_scan(&mut self, host: &H) {
        if !self.active {
            return;
        }
        self.hot_tail_scan_needed = true;
        if self.activation_frame.is_none() {
            self.schedule_scan(host);
        }
    }

    fn cancel_current_scan_after_paint(&mut self, host: &H) {
        if let Some(frame) = self.activation_frame.take() {
            host.cancel_animation_frame(frame);
        }
    }

    fn schedule_current_scan_after_paint(&mut self, host: &H) {
        if self.activation_frame.is_none() {
            self.activation_frame =
                Some(host.request_animation_frame(FrameCallback::ActivationScan));
        }
    }

    /// The post-activation paint frame fired.
    pub fn activation_frame_fired(&mut self, host: &H) {
        self.activation_frame = None;
        self.request_current_scan(host);
    }

    /// A mutation batch arrived; `touched_rows` names the rows it touched and
    /// is read only when the batch counts.
    pub fn observe_mutations(&mut self, host: &H, touched_rows: impl FnOnce() -> Vec<H::Element>) {
        if !self.active || self.discard_dirty_until_activation_scan {
            return;
        }
        if !host.is_page_visible() {
            self.request_current_scan(host);
            return;
        }
        for row in touched_rows() {
            self.dirty_rows.insert(&row);
        }
        self.schedule_scan(host);
    }

    /// Browsers may drop a hidden tab's queued frame: recovery resets both the
    /// handle and the latch so later mutation scans cannot deadlock.
    pub fn visibility_changed(&mut self, host: &H) {
        if !self.active || !host.is_page_visible() {
            return;
        }
        tracing::debug!(target: "terminal_links", "visibility recovery rearms the tail scan");
        self.cancel_current_scan_after_paint(host);
        self.cancel_scan(host);
        self.request_current_scan(host);
    }

    /// Foreground (`true`) or withdraw (`false`) the scanner.
    pub fn set_active(&mut self, host: &H, next_active: bool) {
        if self.disposed || next_active == self.active {
            return;
        }
        self.active = next_active;
        tracing::debug!(target: "terminal_links", active = next_active, "link scanner activity");
        if !next_active {
            self.cancel_scan(host);
            self.cancel_current_scan_after_paint(host);
            self.dirty_rows.clear();
            self.hot_tail_scan_needed = false;
            self.discard_dirty_until_activation_scan = false;
            if self.observing {
                host.observe_mutations(false);
                self.observing = false;
            }
            if self.listening_for_visibility {
                host.listen_for_visibility(false);
                self.listening_for_visibility = false;
            }
            return;
        }
        // Canonical repaint mutations may include retained history; activation
        // owns one current-tail scan instead of replaying it as dirty rows.
        self.discard_dirty_until_activation_scan = true;
        if !self.observing {
            host.observe_mutations(true);
            self.observing = true;
        }
        if !self.listening_for_visibility {
            host.listen_for_visibility(true);
            self.listening_for_visibility = true;
        }
        self.schedule_current_scan_after_paint(host);
    }

    /// Tear down for good.
    pub fn dispose(&mut self, host: &H) {
        if self.disposed {
            return;
        }
        self.set_active(host, false);
        self.disposed = true;
    }
}

impl<H: LinkScanHost> std::fmt::Debug for LinkScanner<H> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LinkScanner")
            .field("active", &self.active)
            .field("disposed", &self.disposed)
            .field("scan_handle", &self.scan_handle)
            .field("activation_frame", &self.activation_frame)
            .field("hot_tail_scan_needed", &self.hot_tail_scan_needed)
            .field("dirty_rows", &self.dirty_rows.len())
            .finish()
    }
}
