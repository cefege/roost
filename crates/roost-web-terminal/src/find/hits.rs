//! What one match list means for the PAINTED GRID: the column intervals a row
//! highlights, which match is active, which row must be visible for it, the
//! published state the painter reads, and the whole host conversation.
//!
//! `roost_client_core::search` owns the match and the epoch fence; `cell_row`
//! owns the `FindHit` column interval and the slicing arithmetic. This module
//! joins them. Nothing here reads the DOM or issues an RPC, so both the decision
//! and the ORDER it is demanded in are testable natively.
//!
//! `find::controller` drives searches and feeds this publication; `find::intent`
//! hands a global result to a pane that has not asked for one.

use std::collections::BTreeMap;

use roost_client_core::search::FindMatch;

use crate::cell_row::FindHit;
use crate::find::OlderMatchPage;
use crate::find::host::FindHost;

/// The highlight intervals of every match, keyed by the absolute row they sit
/// in. A `BTreeMap` because the row order IS the painting order: a viewport
/// painter walks rows, and a list ordered by match arrival would make the
/// painted order follow scan order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HitRows(BTreeMap<u32, Vec<FindHit>>);

impl HitRows {
    /// No rows, no intervals.
    pub const fn empty() -> Self {
        Self(BTreeMap::new())
    }
    /// Whether nothing is highlighted.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// The intervals in one row, or `None` for a row no match sits in.
    pub fn row(&self, row: u32) -> Option<&[FindHit]> {
        self.0.get(&row).map(Vec::as_slice)
    }
    /// The rows carrying at least one interval, ascending.
    pub fn rows(&self) -> impl Iterator<Item = u32> + '_ {
        self.0.keys().copied()
    }
    /// Every row's intervals, ascending by row then by column.
    pub fn iter(&self) -> impl Iterator<Item = (u32, &[FindHit])> {
        self.0.iter().map(|(row, hits)| (*row, hits.as_slice()))
    }
    /// Record one interval, keeping the row's intervals column-ascending.
    ///
    /// The order is maintained here rather than assumed of the caller, because
    /// `iter` promises it: a painter that walks a row's slices in arrival order
    /// would splice the highlight out of column order, and a match list that
    /// spans a resize arrives newest-first.
    fn push(&mut self, row: u32, hit: FindHit) {
        let intervals = self.0.entry(row).or_default();
        let at = intervals
            .iter()
            .position(|existing| existing.col > hit.col)
            .unwrap_or(intervals.len());
        intervals.insert(at, hit);
    }
}

/// The match a reader is on: the row to bring into view, and the column the
/// painter marks with the active class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveHit {
    /// The absolute row the active match sits in.
    pub row: u32,
    /// The column the active match starts at.
    pub col: u32,
}

/// A global-search result offered to pane-local find as the first match to
/// activate. A PREFERENCE, never an authority: pane-local fresh results and the
/// pane's live grid epoch decide the reveal, because a coordinator coordinate
/// may name a row the pane has since renumbered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreferredMatch {
    /// The grid numbering the coordinator coordinate was read against.
    pub grid_epoch: String,
    /// The absolute row it names.
    pub row: u32,
    /// The column it names.
    pub col: u32,
}

/// What a query asks for, beyond the text of it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FindQueryOptions {
    /// Reset regex state before searching an externally supplied literal. A
    /// needle from global content search IS a literal, and a pane left in regex
    /// mode would search for a pattern nobody typed.
    pub literal: bool,
    /// The case sensitivity to search with, when `literal`.
    pub case_sensitive: Option<bool>,
    /// The coordinator coordinate to activate if the pane still numbers rows that
    /// way.
    pub preferred_match: Option<PreferredMatch>,
}

/// A reveal a fetch is owed, fenced to the search that asked for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingReveal {
    /// The row the pull is for.
    pub row: u32,
    /// The grid numbering the match was found under.
    pub epoch: String,
    /// The publication token of the search that found it.
    pub token: u64,
    /// How many epoch renumberings this reveal may still spend.
    pub epoch_retry_budget: u32,
}

/// The active match at a 1-based index, or `None` when there is none.
///
/// Index 0 is the state a cleared find bar is in, and it is not a position: the
/// painted active class and the row a reveal scrolls to both come from here, so
/// a zero index has to mean "no active match", never "the first one".
#[must_use]
pub fn active_hit(matches: &[FindMatch], index: u32) -> Option<ActiveHit> {
    if index == 0 {
        return None;
    }
    matches.get(index as usize - 1).map(|found| ActiveHit {
        row: found.row,
        col: found.col,
    })
}

/// Whether a match list belongs to the pane's live grid numbering.
///
/// A list from a retired grid numbers rows the pane has since renumbered, so
/// painting any of them highlights the wrong line. The first match decides: a
/// chain fences a whole page to one epoch, and a list mixing two could not be
/// repaired row by row.
#[must_use]
pub fn matches_belong_to_pane(matches: &[FindMatch], pane_epoch: &str) -> bool {
    matches
        .first()
        .is_none_or(|found| found.epoch == pane_epoch)
}

/// The highlight intervals of a match list, grouped into the rows they sit in.
///
/// Grouping rather than flattening is what lets a painter walk the grid in row
/// order and look up only the rows that carry a match: a flat match list would
/// make painted order follow scan order, and a match list spanning a resize
/// arrives newest-first.
#[must_use]
pub fn hit_rows(matches: &[FindMatch]) -> HitRows {
    let mut rows = HitRows::empty();
    for found in matches {
        rows.push(
            found.row,
            FindHit {
                col: found.col,
                len: found.len,
            },
        );
    }
    rows
}

/// The 1-based index the next publication activates.
///
/// The preferred coordinate wins only when the pane still numbers rows the way
/// the coordinator read them; otherwise the newest fresh pane match is active,
/// which is where a reader who just typed expects to be.
#[must_use]
pub fn preferred_find_index(
    matches: &[FindMatch],
    pane_epoch: &str,
    preferred: Option<&PreferredMatch>,
) -> u32 {
    if let Some(preferred) = preferred
        && pane_epoch == preferred.grid_epoch
        && let Some(position) = matches.iter().position(|found| {
            found.epoch == preferred.grid_epoch
                && found.row == preferred.row
                && found.col == preferred.col
        })
    {
        return position as u32 + 1;
    }
    matches.len() as u32
}

/// Everything the painted grid is currently told about find.
///
/// Separate from the controller because these are DISPLAY STATE: they change when
/// a search concludes or the reader steps, and the painter reads them rather than
/// the search. A cleared bar is a real state, not an absent one — the painter has
/// to be told to stop highlighting.
#[derive(Debug, Clone, Default)]
pub struct FindPublication {
    matches: Vec<FindMatch>,
    /// 1-based position of the active match, 0 when there is none.
    index: u32,
    truncated: bool,
    failed: bool,
    preferred_match: Option<PreferredMatch>,
    older_page: Option<OlderMatchPage>,
    pending_reveal: Option<PendingReveal>,
}

impl FindPublication {
    /// Nothing published, no active match, nothing owed a fetch.
    pub fn new() -> Self {
        Self::default()
    }
    /// The published matches, ascending by row then column.
    pub fn matches(&self) -> &[FindMatch] {
        &self.matches
    }
    /// The 1-based position of the active match, 0 when there is none.
    pub fn index(&self) -> u32 {
        self.index
    }
    /// Whether older matches are known to exist beyond the published ones.
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }
    /// Whether the search stopped on something invalid or incomplete. Shown on
    /// the input, never as a toast.
    pub fn has_failed(&self) -> bool {
        self.failed
    }
    /// Whether a match cap left older rows this reader may still page into.
    pub fn has_older_page(&self) -> bool {
        self.older_page.is_some()
    }

    /// Offer a coordinator coordinate to activate, replacing any earlier one.
    pub fn prefer(&mut self, preferred: Option<PreferredMatch>) {
        self.preferred_match = preferred;
    }

    /// Drop every published match, the older-rows cursor, and the failure a
    /// previous search reported. The paint is re-issued, because a cleared bar is
    /// a state the painter has to be told about.
    pub fn clear(&mut self, host: &mut dyn FindHost) {
        self.matches.clear();
        self.index = 0;
        self.truncated = false;
        self.failed = false;
        self.older_page = None;
        self.pending_reveal = None;
        self.paint(host);
    }

    /// Report that a search gave up with nothing to blame. Separate from `clear`
    /// because the input still has to say WHY it is empty: a bar showing no
    /// results reads as "no matches", one showing a failure reads as "this did
    /// not work".
    pub fn mark_failed(&mut self) {
        self.failed = true;
    }

    /// Take the older-rows cursor a match cap handed back, so two keypresses
    /// cannot spend the same page.
    pub fn take_older_page(&mut self) -> Option<OlderMatchPage> {
        self.older_page.take()
    }

    /// Install a concluded search: reading order, the active match, and the row
    /// its reveal is owed.
    ///
    /// The worker's traversal is newest-first, which is the order a scan reads in
    /// and not the order a reader steps through, so the list is re-sorted here.
    pub fn install(
        &mut self,
        mut list: Vec<FindMatch>,
        truncated: bool,
        failed: bool,
        older: Option<OlderMatchPage>,
        pane_epoch: &str,
        host: &mut dyn FindHost,
    ) -> Option<(u32, String)> {
        list.sort_by_key(|found| (found.row, found.col));
        self.matches = list;
        self.truncated = truncated;
        self.failed = failed;
        self.older_page = older;
        let preferred = self.preferred_match.take();
        self.index = preferred_find_index(&self.matches, pane_epoch, preferred.as_ref());
        self.paint(host);
        self.active_match()
    }

    /// Move the active match by `delta`, wrapping at both ends.
    pub fn step(&mut self, delta: i64) -> Option<(u32, String)> {
        if self.matches.is_empty() {
            return None;
        }
        let length = self.matches.len() as i64;
        let next = ((self.index as i64 - 1 + delta) % length + length) % length;
        self.index = next as u32 + 1;
        self.active_match()
    }

    /// The active match's row and grid numbering, which is what a reveal acts on.
    pub fn active_match(&self) -> Option<(u32, String)> {
        self.matches
            .get(self.index.checked_sub(1)? as usize)
            .map(|found| (found.row, found.epoch.clone()))
    }

    /// Arm the reveal of one match, which is owed a fetch when it is history.
    pub fn arm_reveal(&mut self, row: u32, epoch: &str, token: u64, budget: u32) {
        self.pending_reveal = Some(PendingReveal {
            row,
            epoch: epoch.to_string(),
            token,
            epoch_retry_budget: budget,
        });
    }

    /// Take the reveal owed a fetch of `row`, if that is the one outstanding.
    pub fn take_pending_reveal(&mut self, row: u32) -> Option<PendingReveal> {
        let pending = self
            .pending_reveal
            .clone()
            .filter(|reveal| reveal.row == row)?;
        self.pending_reveal = None;
        Some(pending)
    }

    /// Choose the match the next publication activates: the newest of a freshly
    /// slid page, or the one the reader was parked on when it added none.
    pub fn prefer_newest_of(&mut self, slid: &[FindMatch], parked: &[FindMatch]) {
        let choice = slid
            .iter()
            .max_by_key(|found| found.row)
            .or_else(|| parked.first());
        self.preferred_match = choice.map(|found| PreferredMatch {
            grid_epoch: found.epoch.clone(),
            row: found.row,
            col: found.col,
        });
    }

    /// Paint only hits owned by the pane's current grid numbering.
    fn paint(&mut self, host: &mut dyn FindHost) {
        if matches_belong_to_pane(&self.matches, &host.pane_epoch()) {
            host.publish_hits(
                &hit_rows(&self.matches),
                active_hit(&self.matches, self.index),
            );
        } else {
            host.publish_hits(&HitRows::empty(), None);
        }
    }
}
