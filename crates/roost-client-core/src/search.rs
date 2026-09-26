//! Find paging: validating one coordinator scrollback-search page before any of
//! its rows reach a reader.
//!
//! Three guards, each of which exists because the alternative is a row that
//! points at the wrong line: the scanned window must be contiguous, inside the
//! row ceiling, and continue exactly from the row the reader was on; every match
//! must name a row inside that window; and the continuation must move strictly
//! in the direction the reader is paging.
//!
//! Ported from `apps/web/src/client/search/terminalFindPaging.ts`. The limits
//! are `roost_protocol::terminal_search` and are not restated here.

use roost_protocol::terminal_search::{TERMINAL_SEARCH_MAX_MATCHES, TERMINAL_SEARCH_MAX_ROWS};

/// One match, fenced to the grid epoch that owns its row.
///
/// The epoch travels WITH the row rather than being read from the replica at
/// click time: a search that spans a resize has results from two grids, and a row
/// whose epoch is looked up later points at whatever the grid has become.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindMatch {
    /// The absolute scrollback row.
    pub row: u32,
    /// The column the match starts at.
    pub col: u32,
    /// How many columns the match spans.
    pub len: u32,
    /// A short excerpt for the results list. An excerpt, never the row: a result
    /// row is shown in a list the reader did not ask to be scrolled to.
    pub preview: String,
    /// The grid numbering that owns `row`.
    pub epoch: String,
}

/// One match as it arrived, before it is fenced to an epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawMatch {
    /// The absolute scrollback row.
    pub row: u32,
    /// The column the match starts at.
    pub col: u32,
    /// How many columns the match spans.
    pub len: u32,
    /// A short excerpt.
    pub preview: String,
}

/// One page, as the coordinator reported it: the window it read and where it
/// would resume. The matches travel beside it, not inside it, so the window can
/// be validated before any match is trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchPage {
    /// The first row scanned, inclusive.
    pub scanned_start_row: u32,
    /// The last row scanned, exclusive.
    pub scanned_end_row: u32,
    /// The row the NEXT page starts at, when there is one.
    pub next_before_row: Option<u32>,
}

/// Why a page was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageRefusal {
    /// The scanned window runs backwards, runs past the row ceiling, carries more
    /// matches than the ceiling allows, or does not continue from the row the
    /// reader was on.
    BadWindow,
    /// A match names a row outside the scanned window. This is the one that
    /// matters: the row is absolute, and a match outside the window the
    /// coordinator actually read belongs to a different scan.
    MatchOutsideWindow,
    /// The continuation does not move strictly in the paging direction, so
    /// following it would loop or walk back over rows already fetched.
    BadContinuation,
}

impl PageRefusal {
    /// A short name for the incident log.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BadWindow => "bad_window",
            Self::MatchOutsideWindow => "match_outside_window",
            Self::BadContinuation => "bad_continuation",
        }
    }
}

/// Whether the page's scanned window is the one the reader asked for.
///
/// `before_row` is where the reader's previous page ended, and the new page must
/// END exactly there: pages tile downward from a cursor, so a window that merely
/// OVERLAPS the previous one has silently skipped rows in between.
pub fn window_is_valid(page: &SearchPage, matches: &[RawMatch], before_row: Option<u32>) -> bool {
    if page.scanned_start_row > page.scanned_end_row {
        return false;
    }
    if page.scanned_end_row - page.scanned_start_row > TERMINAL_SEARCH_MAX_ROWS {
        return false;
    }
    if matches.len() as u32 > TERMINAL_SEARCH_MAX_MATCHES {
        return false;
    }
    if before_row.is_some_and(|expected| page.scanned_end_row != expected) {
        return false;
    }
    matches
        .iter()
        .all(|m| m.row >= page.scanned_start_row && m.row < page.scanned_end_row)
}

/// Whether the page offers a usable next page, moving strictly in the paging
/// direction.
pub fn continuation_is_valid(page: &SearchPage, before_row: Option<u32>) -> bool {
    let Some(next) = page.next_before_row else {
        return false;
    };
    if next != page.scanned_start_row || next >= page.scanned_end_row {
        return false;
    }
    before_row.is_none_or(|previous| next < previous)
}

/// Fence every match in a page to one grid epoch.
///
/// `None` when the window is not the one the reader asked for, so a caller cannot
/// fence a page it should have refused. The epoch is applied to every row
/// TOGETHER, because a page whose rows come from two grids cannot be made
/// coherent by tagging them one at a time.
pub fn fence_page(
    matches: &[RawMatch],
    page: &SearchPage,
    before_row: Option<u32>,
    epoch: &str,
) -> Option<Vec<FindMatch>> {
    if !window_is_valid(page, matches, before_row) {
        return None;
    }
    Some(
        matches
            .iter()
            .map(|raw| FindMatch {
                row: raw.row,
                col: raw.col,
                len: raw.len,
                preview: raw.preview.clone(),
                epoch: epoch.to_string(),
            })
            .collect(),
    )
}
