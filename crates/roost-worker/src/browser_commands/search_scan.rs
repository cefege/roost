//! The production [`ScrollbackSearch`]: a bounded newest-first cursor scan over
//! one session's grid, and a page-budget's worth of them. `runtime::deps`
//! installs one over the worker's `SessionTable`; `browser_commands::search`
//! owns admission, cancellation and the two identities.
//!
//! This is v2's `apps/worker/src/terminal/search/terminal-search.ts` and
//! `terminal-search-batch.ts` with the awaits replaced. v2 scanned
//! asynchronously so a slice could let every OTHER session's PTY run; here the
//! same bound is reached by taking and releasing the session record once per
//! slice, which is the same thing without an `await` held across a lock.
//!
//! FOUR THINGS ARE NOT NEGOTIABLE, and each is a specific bug when dropped.
//!
//! NEWEST FIRST, `before_row` EXCLUSIVE, RANGES HALF-OPEN. This is the same
//! absolute row numbering the cell frames use, so a client paging a search and
//! a client paging scrollback agree about which row "row 900" is.
//!
//! A MATCH CAP THAT STOPS SHORT OF THE FLOOR HANDS BACK THE SAME CURSOR A ROW
//! CAP DOES. Otherwise the matches older than the cap are unreachable: the page
//! says "truncated" and names no way to continue.
//!
//! THE EPOCH IS CHECKED BEFORE THE QUERY IS COMPILED AND BEFORE A ROW IS READ.
//! A caller holding a stale epoch gets `epoch_changed` and nothing else — not a
//! scan over a grid it can no longer address, and not a pattern it pays to
//! compile first.
//!
//! THE FLOOR IS RE-READ AT EVERY SLICE BOUNDARY. A match the ring evicted
//! underneath the scan is an error rather than a row to skip, because the page
//! reports a half-open range and a skipped row is a hole in it the caller
//! cannot see.

use std::sync::Arc;
use std::time::{Duration, Instant};

use roost_protocol::terminal_search::{
    GLOBAL_TERMINAL_SEARCH_MAX_MATCHES, GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
    TERMINAL_SEARCH_RPC_DEADLINE_MS,
};
use serde_json::{Value, json};

use super::scrollback_page::GridDescription;
use super::search::{BatchSearch, ScrollbackSearch, SingleSearch};
use super::search_match::{Matcher, RowScan, Scan, preview_of, scan_row};
use super::search_page::{Page, fleet_error};
use super::{Boxed, Refusal};
use roost_protocol::wire::brand::SessionId;

use crate::session::lifecycle::SessionTable;
use crate::session::retained_grid::describe_grid;

/// Rows one slice may read before the session record lock is handed back.
///
/// Every other session's PTY output is stalled for the duration, so this stays
/// well under a frame — the same reason `crate::scrollback_read` has one, and
/// the same number v2 used.
pub const SEARCH_SLICE_ROWS: u32 = 500;

/// The work budget one single-session scan gets.
///
/// v2 reserved the last half second of the eight-second RPC deadline for
/// building the result and getting it back; the scan is what the other seven
/// and a half seconds are for.
const RETURN_RESERVE_MS: u32 = 500;

/// The command a refusal is attributed to. A scan answers its own result with a
/// stop reason, so the only refusals are the ones that never reach a scan.
const COMMAND: &str = "search-scrollback";

/// A scan over the grids this worker holds.
#[derive(Debug)]
pub struct GridScanner {
    table: Arc<SessionTable>,
}

impl GridScanner {
    /// A scanner over every session this worker holds.
    pub fn new(table: Arc<SessionTable>) -> Self {
        Self { table }
    }
}

impl ScrollbackSearch for GridScanner {
    fn search(&self, request: SingleSearch) -> Boxed<Result<Value, Refusal>> {
        let budget = Duration::from_millis(u64::from(
            TERMINAL_SEARCH_RPC_DEADLINE_MS.saturating_sub(RETURN_RESERVE_MS),
        ));
        let page = self.scan(
            &request.session_id,
            &request.grid_epoch,
            Scan::Literal {
                query: &request.query,
                regex: request.regex,
                case_sensitive: request.case_sensitive,
            },
            Bounds {
                before_row: request.before_row,
                max_rows: request.max_rows,
                max_matches: request.max_matches,
                budget,
            },
        );
        Box::pin(std::future::ready(page))
    }

    fn search_batch(&self, request: BatchSearch) -> Boxed<Result<Value, Refusal>> {
        // ONE PAGE BUDGET ACROSS THE SESSIONS, not per session. A fleet search
        // that gave every session the full match limit would answer with a page
        // proportional to the fleet, which is the thing the cap exists to stop.
        let per_session = GLOBAL_TERMINAL_SEARCH_MAX_MATCHES
            .div_ceil(request.sessions.len().max(1) as u32)
            .min(request.max_matches);
        let deadline = Instant::now() + Duration::from_millis(u64::from(request.deadline_ms));
        let mut entries: Vec<Value> = Vec::with_capacity(request.sessions.len());
        for (session_id, grid_epoch, before_row) in request
            .sessions
            .iter()
            .take(GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS)
        {
            // The budget is SHARED: the frame's deadline is for the page, so
            // each session gets what is left rather than a fresh window. A
            // session that finds none left answers `deadline`, which is the
            // honest name for a page the caller's own deadline ended.
            let entry = match self.scan(
                session_id,
                grid_epoch,
                Scan::Literal {
                    query: &request.query,
                    regex: false,
                    case_sensitive: request.case_sensitive,
                },
                Bounds {
                    before_row: *before_row,
                    max_rows: request.max_rows_per_session,
                    max_matches: per_session,
                    budget: deadline.saturating_duration_since(Instant::now()),
                },
            ) {
                Ok(result) => json!({
                    "status": "ok",
                    "session_id": session_id,
                    "result": result,
                }),
                Err(error) => json!({
                    "status": "error",
                    "session_id": session_id,
                    "error": fleet_error(&error),
                }),
            };
            entries.push(entry);
        }
        Box::pin(std::future::ready(Ok(json!({ "entries": entries }))))
    }
}

/// How much of one session's grid a scan may read.
///
/// These four were four parameters, which is what `clippy::too_many_arguments`
/// was objecting to — and it was right that they belong together, because they
/// are one decision: the caller saying how much it wants. Folding them makes
/// that a type rather than a convention, and a scan can no longer be given a
/// row budget with no match budget by forgetting an argument at a call site.
#[derive(Debug, Clone, Copy)]
struct Bounds {
    /// The newest row a caller may name, or `None` for the whole history.
    before_row: Option<u32>,
    max_rows: u32,
    max_matches: u32,
    budget: Duration,
}

impl GridScanner {
    /// One session's page, newest first, bounded by rows, matches and a budget.
    fn scan(
        &self,
        session_id: &str,
        grid_epoch: &str,
        request: Scan<'_>,
        bounds: Bounds,
    ) -> Result<Value, Refusal> {
        let reading = self.describe(session_id)?;
        let (grid, live_epoch) = (reading.description, reading.epoch);
        // The newest row a caller may name is the last history row plus the
        // viewport, and `before_row` is clamped to it rather than refused: a
        // caller may hold a cursor from before an emit, and a well-formed
        // request naming a row the grid has not reached yet is a request for
        // everything, not a failure.
        let newest_exclusive = grid.total.saturating_add(u32::from(grid.viewport_rows));
        let scanned_end = bounds
            .before_row
            .unwrap_or(newest_exclusive)
            .min(newest_exclusive);
        let mut page = Page::new(&grid, scanned_end);

        if !grid_epoch.is_empty() && grid_epoch != live_epoch {
            page.epoch.clone_from(&live_epoch);
            return Ok(page.into_value("epoch_changed", None, Vec::new()));
        }
        if request.query().is_empty() {
            page.scanned_start_row = 0;
            page.scanned_end_row = 0;
            return Ok(page.into_value("complete", None, Vec::new()));
        }
        let matcher = Matcher::compile(request)?;

        let deadline = Instant::now() + bounds.budget;
        let mut matches: Vec<Value> = Vec::new();
        let mut suppressed = false;
        let mut next_row = page.scanned_end_row;
        let mut stop: Option<&'static str> = None;
        while let Some(row) = next_row.checked_sub(1) {
            if row < grid.retained_floor {
                stop = Some("complete");
                break;
            }
            if Instant::now() >= deadline {
                stop = Some("deadline");
                break;
            }
            let start = next_row
                .saturating_sub(bounds.max_rows.min(SEARCH_SLICE_ROWS))
                .max(grid.retained_floor);
            for (row, scanned) in self.read_slice(session_id, &matcher, start, next_row)? {
                if matches.len() as u32 >= bounds.max_matches {
                    // One row past the cap is what tells a full page from a
                    // short one: without it a scan that reached the floor with
                    // exactly `max_matches` hits would claim to be truncated
                    // while holding everything there was.
                    suppressed = true;
                    break;
                }
                let preview = preview_of(&scanned.text);
                for hit in scanned.hits {
                    matches.push(json!({
                        "row": row,
                        "col": hit.col,
                        "len": hit.len,
                        "preview": preview,
                    }));
                }
                page.scanned_start_row = row;
            }
            if matches.len() as u32 >= bounds.max_matches
                && (!suppressed || start > grid.retained_floor)
            {
                stop = Some("match_limit");
                break;
            }
            next_row = start;
            if next_row <= grid.retained_floor {
                stop = Some("complete");
                break;
            }
            if page.scanned_end_row.saturating_sub(page.scanned_start_row) >= bounds.max_rows {
                stop = Some("row_limit");
                break;
            }
        }
        let reason = stop.unwrap_or("complete");
        // Read BEFORE the page is consumed: the cursor is the one field a
        // caller needs and `into_value` takes the page whole.
        let cursor = page.scanned_start_row;
        let continues =
            reason == "row_limit" || (reason == "match_limit" && cursor > grid.retained_floor);
        Ok(page.into_value(reason, continues.then_some(cursor), matches))
    }

    /// The grid and its live epoch, read together, or the refusal for a
    /// session this worker does not hold.
    ///
    /// The epoch lives beside the geometry rather than in a second lookup: a
    /// grid and the epoch that names it are one observation, and reading them
    /// separately is how a page gets scanned against a grid that has already
    /// been reframed.
    fn describe(&self, session_id: &str) -> Result<Reading, Refusal> {
        let branded = SessionId::try_from(session_id.to_owned())
            .map_err(|_| Refusal::failed(COMMAND, "session closed"))?;
        let (described, epoch) = self
            .table
            .with_record(&branded, |record| {
                (describe_grid(record), record.cell_emit.grid_epoch())
            })
            .ok_or_else(|| Refusal::failed(COMMAND, "session closed"))?;
        Ok(Reading {
            description: described?,
            epoch,
        })
    }

    /// Read `[start, end)` newest-first, holding the record once per slice.
    ///
    /// The lock is taken for the WHOLE slice rather than once per row: a
    /// row-at-a-time lock would let the ring evict between two rows of one page
    /// and splice them.
    fn read_slice(
        &self,
        session_id: &str,
        matcher: &Matcher,
        start: u32,
        end: u32,
    ) -> Result<Vec<(u32, RowScan)>, Refusal> {
        let branded = SessionId::try_from(session_id.to_owned())
            .map_err(|_| Refusal::failed(COMMAND, "session closed"))?;
        let mut rows = Vec::new();
        let mut row = end;
        while row > start {
            row -= 1;
            match self
                .table
                .with_record(&branded, |record| scan_row(record, matcher, row))
            {
                Some(Some(scanned)) => rows.push((row, scanned)),
                Some(None) => {
                    return Err(Refusal::failed(
                        COMMAND,
                        "scrollback search row unavailable",
                    ));
                }
                None => return Err(Refusal::failed(COMMAND, "session closed")),
            }
        }
        Ok(rows)
    }
}

/// The grid as it was when the page was admitted, plus its live epoch.
struct Reading {
    description: GridDescription,
    epoch: String,
}
