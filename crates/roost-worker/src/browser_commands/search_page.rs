//! One scrollback-search page as the wire spells it. `search_scan` fills it;
//! this file turns it into the reply, and it is the only place that knows the
//! field names a browser parses.
//!
//! It exists apart from the scan because the ANSWER has obligations the SCAN
//! does not: `truncated` must agree with `stop_reason`, `next_before_row` must
//! appear only when a page can actually continue, and the history floor must be
//! the same answer a scrollback page gives. All three are cross-field rules, and
//! a reader checking them should not have to hold a scan loop in their head.
//! Ports v2 `apps/worker/src/terminal/search/terminal-search-result.ts`.

use roost_protocol::terminal_search::ScrollbackHistoryFloor;
use serde_json::{Map, Value};

use super::Refusal;
use super::scrollback_page::{GridDescription, history_floor_for};

/// The accumulated page, and the numbers every answer is built from.
pub(super) struct Page {
    pub(super) scanned_start_row: u32,
    pub(super) scanned_end_row: u32,
    scrollback_total: u32,
    cols: u32,
    /// The epoch the ANSWER carries: the live one, never the one that was asked
    /// with, because the caller's next page has to name this one.
    pub(super) epoch: String,
    description: GridDescription,
}

impl Page {
    /// A page over `description`, ending at `scanned_end_row`.
    pub(super) fn new(description: &GridDescription, scanned_end_row: u32) -> Self {
        Self {
            scanned_start_row: scanned_end_row,
            scanned_end_row,
            scrollback_total: description.total,
            cols: u32::from(description.cols),
            epoch: description.binding.current().to_owned(),
            description: description.clone(),
        }
    }

    /// The page, in the wire's own field names and the browser's own literals.
    ///
    /// `next_before_row` is omitted rather than sent as null when a page cannot
    /// continue, because a client that reads the key's presence is asking
    /// "may I continue?", and `"next_before_row": null` answers a different
    /// question.
    pub(super) fn into_value(
        self,
        reason: &'static str,
        next_before_row: Option<u32>,
        matches: Vec<Value>,
    ) -> Value {
        let mut result = Map::with_capacity(10);
        result.insert("matches".to_owned(), Value::Array(matches));
        // `truncated` IS `match_limit` or `deadline`, in both directions: a
        // page that stopped short without saying so is a client that stops
        // paging, and one that says so without stopping short is a client that
        // pages forever.
        result.insert(
            "truncated".to_owned(),
            Value::from(reason == "match_limit" || reason == "deadline"),
        );
        result.insert(
            "scrollback_total".to_owned(),
            Value::from(self.scrollback_total),
        );
        result.insert("cols".to_owned(), Value::from(self.cols));
        result.insert("grid_epoch".to_owned(), Value::from(self.epoch));
        result.insert(
            "scanned_start_row".to_owned(),
            Value::from(self.scanned_start_row),
        );
        result.insert(
            "scanned_end_row".to_owned(),
            Value::from(self.scanned_end_row),
        );
        result.insert(
            "history_floor".to_owned(),
            Value::from(floor_literal(&self.description, self.scanned_start_row)),
        );
        if let Some(next) = next_before_row {
            result.insert("next_before_row".to_owned(), Value::from(next));
        }
        result.insert("stop_reason".to_owned(), Value::from(reason));
        Value::Object(result)
    }
}

/// The one history-floor decision, in the wire's own literals.
///
/// [`history_floor_for`] owns the DECISION because a scrollback page needs the
/// same answer for the same reason; a scanner that re-derived it would be a
/// second answer to "why is there nothing older than this", kept in step by
/// nothing.
fn floor_literal(description: &GridDescription, scanned_start_row: u32) -> &'static str {
    match history_floor_for(description, scanned_start_row) {
        ScrollbackHistoryFloor::None => "none",
        ScrollbackHistoryFloor::Evicted => "evicted",
        ScrollbackHistoryFloor::ResizeReplay => "resize_replay",
        // A value a NEWER peer added. Spelled `none` rather than refused: the
        // page already decided the floor, and an older build that cannot name
        // the reason must not turn a readable page into an error.
        ScrollbackHistoryFloor::Other(_) => "none",
    }
}

/// One refused session, named in a fleet-wide page.
///
/// A single search REFUSES, because the caller's one request failed and it is
/// waiting for that one answer. A fleet page is a navigation across many
/// sessions where one closing is ordinary, so the same cause becomes a
/// per-entry `error` and the rest still answer. The vocabulary is closed
/// because the coordinator narrows a worker's answer against the same literals.
pub(super) fn fleet_error(refusal: &Refusal) -> &'static str {
    match refusal {
        Refusal::Failed { message, .. } if message == "session closed" => "session_closed",
        Refusal::Failed { message, .. } if message == "session has no terminal" => "no_terminal",
        _ => "internal",
    }
}
