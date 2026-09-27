//! Reading painted terminal links back out of the attributes the row painter
//! stamped, and the scheduling decision for when a look is worth taking.
//!
//! The input is DATA — `PaintedRow`, which `links::dom` reads off the live DOM —
//! so every rule here is testable without a browser: which links a row holds,
//! which one a grid position lands on, and which soft-wrapped halves are one
//! link rather than two.
//!
//! A link is re-identified by `LINK_KEY_ATTR`, never by its text: the core
//! authors one run key per link and stamps it on every painted half, so the two
//! rows a wrapped link straddles merge back into one. Text cannot do this — two
//! links may share their visible text exactly.
//!
//! Ported from v2's `terminal-links.scan.ts` and the painted-link read in
//! `terminal-links.dom.ts`.

use super::PaintedRow;

/// How many dirty rows a mutation may touch before the scan bounds itself to the
/// live tail instead of replaying them.
///
/// A streaming terminal dirties its newest scrollback block every frame, so an
/// unbounded dirty set is a per-frame rescan of history that never ends.
pub const DIRTY_ROW_LIMIT: u32 = 300;

/// One painted half of a link: the row it landed on and the columns it covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkHalf {
    /// The 1-based grid row this half is on.
    pub row: u32,
    /// The 1-based first column of the half.
    pub first_col: u32,
    /// The 1-based column just past the half, so the interval is half-open.
    pub end_col: u32,
}

impl LinkHalf {
    /// Whether this half paints `col` on `row`.
    pub fn covers(&self, row: u32, col: u32) -> bool {
        self.row == row && col >= self.first_col && col < self.end_col
    }
}

/// One link found in painted rows, with every painted half it has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedLink {
    /// `LINK_KEY_ATTR`, verbatim. The core's per-run identity, and the only
    /// thing that ties a soft-wrapped second row back to the first.
    pub key: String,
    /// The terminal-authored target, exactly as the renderer stamped it. A
    /// re-serialized URI would retarget the click.
    pub raw_target: String,
    /// The painted halves in row order. A link that did not wrap has exactly one.
    pub halves: Vec<LinkHalf>,
}

/// Every link painted in one row, in document order: one entry per anchor.
///
/// `grid_row` is the 1-based grid row the element sits on, which is a fact
/// about where the row was READ rather than about the row itself. A link that
/// soft-wrapped is not joined here — its halves are in different rows, and
/// [`region_links`] is what merges them.
///
/// A row without `ROW_HAS_LINKS_ATTR` contributes nothing: the marker is the
/// one attribute read that skips virtually every held row of a scan.
pub fn row_links(row: &PaintedRow, grid_row: u32) -> Vec<ScannedLink> {
    if !row.has_links {
        return Vec::new();
    }
    let mut column = 1u32;
    let mut links: Vec<ScannedLink> = Vec::new();
    for child in &row.children {
        let first_col = column;
        column = column.saturating_add(child.columns);
        let Some(attributes) = child.link.as_ref() else {
            continue;
        };
        // A run key with no target names a link nothing can open, so it is not a
        // link at all. An unkeyed one still is: an inferred match carries a
        // target and no run key.
        if !attributes.is_terminal_link {
            continue;
        }
        let Some(raw_target) = attributes.target.clone() else {
            continue;
        };
        links.push(ScannedLink {
            key: attributes.key.clone().unwrap_or_default(),
            raw_target,
            halves: vec![LinkHalf {
                row: grid_row,
                first_col,
                // A child the adapter could not measure claims no columns, so it
                // is never a hit. Inventing one would put a link under a cell
                // the pointer may not be anywhere near.
                end_col: column,
            }],
        });
    }
    links
}

/// Every link in a run of rows, in first-appearance order.
///
/// Two painted halves of one soft-wrapped link arrive in two rows with the same
/// run key; this merges them into one `ScannedLink` with two halves, so a click
/// on either half activates the same link. `base_row` is the 1-based grid row
/// `rows[0]` sits on, which the caller knows from where it read the rows.
pub fn region_links(rows: &[PaintedRow], base_row: u32) -> Vec<ScannedLink> {
    let mut links: Vec<ScannedLink> = Vec::new();
    for (offset, row) in rows.iter().enumerate() {
        for link in row_links(row, base_row.saturating_add(offset as u32)) {
            match keyed_index(&links, &link.key) {
                Some(index) => {
                    let existing = &mut links[index];
                    existing.raw_target = link.raw_target;
                    existing.halves.extend(link.halves);
                }
                None => links.push(link),
            }
        }
    }
    links
}

/// The index of the link already carrying `key`, or `None`.
///
/// An unkeyed link never matches: two of them are two links, because nothing
/// about them says they are halves of one.
fn keyed_index(links: &[ScannedLink], key: &str) -> Option<usize> {
    if key.is_empty() {
        return None;
    }
    links.iter().position(|link| link.key == key)
}

/// The link a grid position lands on, or `None` when the cell holds no link.
///
/// A position outside every half is a plain cell: a link owns columns, not a
/// bounding box, so the cell to the right of the last one is not a click away
/// from opening it.
pub fn link_at_cell<'a>(links: &'a [ScannedLink], row: u32, col: u32) -> Option<&'a ScannedLink> {
    links
        .iter()
        .find(|link| link.halves.iter().any(|half| half.covers(row, col)))
}

/// What the scanner should do next, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanRequest {
    /// The callback scanned nothing: either the post-activation paint landed and
    /// only ARMED the current-tail scan, or there was nothing left to scan.
    Idle,
    /// Scan the current tail: the newest scrollback block plus the viewport.
    /// Retained history is deliberately never revisited.
    CurrentTail,
    /// Scan exactly the rows a mutation touched.
    Dirty {
        /// How many rows the mutation dirtied.
        rows: u32,
    },
}

/// The scanner's scheduling state: what the browser still owes it, and what that
/// callback must scan.
///
/// The owed callback is the reason this is a type. v2 kept a `scanScheduled`
/// latch in a closure that only a scheduled scan or an explicit cancel could
/// clear, so a browser that DROPPED the queued animation frame left it stuck
/// forever: later mutations could not queue work and rebuilt anchors stayed
/// unlinked. Everything that owes a callback names it, and visibility recovery
/// cancels the stale one before queueing its replacement — so "a scan is still
/// coming" and "the tail scan ran" are never the same answer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanSchedule {
    /// The attachment is live.
    active: bool,
    /// A callback is owed. Never more than one: a second request coalesces into
    /// the owed one rather than queueing behind it.
    queued: bool,
    /// The owed callback must wait for a paint before it scans.
    awaiting_paint: bool,
    /// The owed callback scans the current tail rather than the dirty set.
    tail_owed: bool,
    /// Retained history arrived as mutations, and the activation scan has not run
    /// yet to read it: replaying it as dirty rows is what this latch prevents.
    discard_dirty: bool,
    /// How many rows a mutation touched. The identities are the adapter's; the
    /// only question the schedule answers about them is how many there are.
    dirty_rows: u32,
}

impl ScanSchedule {
    /// A dormant scanner that owes nothing.
    pub const fn new() -> Self {
        Self {
            active: false,
            queued: false,
            awaiting_paint: false,
            tail_owed: false,
            discard_dirty: false,
            dirty_rows: 0,
        }
    }

    /// Whether the attachment is live.
    pub const fn is_active(&self) -> bool {
        self.active
    }

    /// Whether the browser still owes this scanner a callback.
    pub const fn needs_frame(&self) -> bool {
        self.queued
    }

    /// What the owed callback must scan, or `None` when it owes no scan — which
    /// is the post-activation paint frame, the one that scans nothing.
    pub const fn pending_scan(&self) -> Option<ScanRequest> {
        if self.tail_owed {
            Some(ScanRequest::CurrentTail)
        } else if self.dirty_rows > 0 {
            Some(ScanRequest::Dirty {
                rows: self.dirty_rows,
            })
        } else {
            None
        }
    }

    /// Attach. The browser now owes one post-paint callback and no scan yet: a
    /// canonical repaint's mutations can include retained history, and a pane
    /// that has not painted has nothing to scan.
    pub fn activate(&mut self) {
        self.active = true;
        self.discard_dirty = true;
        if self.queued {
            return;
        }
        self.queued = true;
        self.awaiting_paint = true;
    }

    /// Release everything: the owed callback, the dirty set, and the latch. A
    /// hidden pane must leave nothing a later visibility flip mistakes for its
    /// own.
    pub fn deactivate(&mut self) {
        self.active = false;
        self.queued = false;
        self.awaiting_paint = false;
        self.tail_owed = false;
        self.discard_dirty = false;
        self.dirty_rows = 0;
    }

    /// A mutation touched `rows` rows.
    pub fn note_mutation(&mut self, rows: u32) {
        if !self.active {
            return;
        }
        if self.discard_dirty {
            // The activation scan has not run yet, so this history is about to be
            // read by the tail scan anyway.
            self.arm_tail();
            return;
        }
        self.dirty_rows = self.dirty_rows.saturating_add(rows);
        if self.dirty_rows > DIRTY_ROW_LIMIT {
            // A history-sized dirty set is what a streaming frame produces every
            // tick. The tail is already the rows in question, so bound to it and
            // drop the set rather than replaying history each frame.
            self.arm_tail();
            return;
        }
        self.queued = true;
    }

    /// The page went hidden: arm the tail scan, because the browser will not run
    /// any callback until it comes back.
    pub fn page_hidden(&mut self) {
        if self.active {
            self.arm_tail();
        }
    }

    /// The page came back: cancel whatever was owed and re-arm, so a frame the
    /// browser dropped cannot deadlock the scanner.
    pub fn page_visible(&mut self) {
        if !self.active {
            return;
        }
        self.queued = false;
        self.awaiting_paint = false;
        self.arm_tail();
    }

    /// The owed callback fired. Returns the work it did; whether another callback
    /// is still owed is [`Self::needs_frame`].
    pub fn fire(&mut self) -> ScanRequest {
        self.queued = false;
        if self.awaiting_paint {
            // The paint landed. Only now is the current-tail scan asked for,
            // which is why activation never scans before the first frame.
            self.awaiting_paint = false;
            self.arm_tail();
            return ScanRequest::Idle;
        }
        if self.tail_owed {
            self.tail_owed = false;
            self.discard_dirty = false;
            self.dirty_rows = 0;
            return ScanRequest::CurrentTail;
        }
        let rows = self.dirty_rows;
        self.dirty_rows = 0;
        match rows {
            0 => ScanRequest::Idle,
            rows => ScanRequest::Dirty { rows },
        }
    }

    /// Arm the current-tail scan. A callback already owed IS that callback: a
    /// post-activation paint frame is promoted rather than queued behind.
    fn arm_tail(&mut self) {
        self.tail_owed = true;
        self.queued = true;
    }
}
