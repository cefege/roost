//! Paging the coordinator-retained scrollback into a marker accumulator: the
//! request sequence, the strict per-page validation that keeps a moving
//! snapshot from passing, and the final `RetainedMarkerScan` report. Native;
//! `smoke::backdoor::retained_marker_scan` drives it over `CoordRpc`. Ports
//! `apps/web/src/smoke/smokeRetainedMarkerScan.ts`.

use std::collections::VecDeque;

use roost_client_core::client::rpc::calls::terminal_pane::{ScrollbackCells, ScrollbackCellsPage};
use roost_protocol::terminal_search::ScrollbackHistoryFloor;
use serde::Serialize;

use super::marker_scan::{MAX_SAFE_INTEGER, MarkerTally, inversions, prefixed_markers};

/// The page size when the caller names none.
pub const DEFAULT_PAGE_ROWS: u32 = 512;
/// The largest page a caller may ask for.
pub const MAX_PAGE_ROWS: u32 = 4_096;
/// Pages after which the scan gives up rather than loop on a growing history.
pub const MAX_PAGES: u32 = 128;

/// What `retainedMarkerScan()` reports (`RetainedMarkerScan` in `smokeTypes.ts`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetainedMarkerScan {
    pub grid_epoch: String,
    pub pages: u32,
    pub scrollback_total: u64,
    pub retained_floor: u64,
    pub retained_cap: i64,
    /// Why the scan stopped at `retained_floor`, as the worker reported it.
    pub retained_floor_reason: &'static str,
    pub row_indices: Vec<u64>,
    pub row_gap_count: u64,
    pub marker_ids: Vec<u64>,
    pub marker_min: u64,
    pub marker_max: u64,
    pub marker_missing: u64,
    pub marker_duplicated: Vec<u64>,
    pub marker_out_of_order: u64,
}

/// The pager: one request at a time, newest page first.
#[derive(Debug, Clone)]
pub struct RetainedScanPager {
    session_id: String,
    grid_epoch: String,
    page_rows: u32,
    end_row: u64,
    scrollback_total: Option<u64>,
    retained_floor: u64,
    floor_reason: &'static str,
    pages: u32,
    chunks: VecDeque<Vec<(u64, String)>>,
}

/// Validate the optional `pageRows` argument (any JS number).
pub fn retained_page_rows(requested: Option<f64>) -> Result<u32, String> {
    let Some(rows) = requested else {
        return Ok(DEFAULT_PAGE_ROWS);
    };
    if rows.fract() != 0.0 || !(1.0..=f64::from(MAX_PAGE_ROWS)).contains(&rows) {
        return Err(format!("invalid retained marker page size: {rows}"));
    }
    Ok(rows as u32)
}

impl RetainedScanPager {
    /// A scan of `session_id` fenced to `grid_epoch`; an empty epoch means the
    /// session has no cell grid to scan.
    pub fn new(session_id: &str, grid_epoch: &str, page_rows: u32) -> Result<Self, String> {
        if grid_epoch.is_empty() {
            return Err(format!("no cell grid epoch for {session_id}"));
        }
        Ok(Self {
            session_id: session_id.to_owned(),
            grid_epoch: grid_epoch.to_owned(),
            page_rows,
            end_row: MAX_SAFE_INTEGER,
            scrollback_total: None,
            retained_floor: 0,
            floor_reason: "none",
            pages: 0,
            chunks: VecDeque::new(),
        })
    }

    /// The next page to ask for, or an error once the page budget is spent.
    pub fn next_request(&self) -> Result<ScrollbackCells, String> {
        if self.pages >= MAX_PAGES {
            return Err(format!(
                "retained marker pagination exceeded {MAX_PAGES} pages for {}",
                self.session_id
            ));
        }
        Ok(ScrollbackCells {
            session_id: self.session_id.clone(),
            end_row: self.end_row,
            max_rows: self.page_rows,
            grid_epoch: self.grid_epoch.clone(),
        })
    }

    /// Admit one answer. `Ok(true)` when the scan reached its floor.
    pub fn accept(&mut self, page: &ScrollbackCellsPage) -> Result<bool, String> {
        self.pages += 1;
        let sid = &self.session_id;
        if page.grid_epoch != self.grid_epoch
            || page.start_row > MAX_SAFE_INTEGER
            || page.end_row > MAX_SAFE_INTEGER
            || page.scrollback_total > MAX_SAFE_INTEGER
            || page.end_row < page.start_row
            || page.scrollback_total < page.end_row
        {
            return Err(format!("invalid retained marker page for {sid}"));
        }
        self.floor_reason = match page.history_floor {
            ScrollbackHistoryFloor::Evicted => "evicted",
            ScrollbackHistoryFloor::ResizeReplay => "resize_replay",
            ScrollbackHistoryFloor::None | ScrollbackHistoryFloor::Other(_) => "none",
        };
        match self.scrollback_total {
            None => self.scrollback_total = Some(page.scrollback_total),
            Some(total) if total != page.scrollback_total => {
                return Err(format!(
                    "scrollback changed during retained marker scan for {sid}"
                ));
            }
            Some(_) => {}
        }
        let mut rows = Vec::with_capacity(page.rows.len());
        for (offset, row) in page.rows.iter().enumerate() {
            let expected = page.start_row + offset as u64;
            if u64::from(row.index) != expected {
                return Err(format!(
                    "non-contiguous retained page for {sid} at {expected}"
                ));
            }
            let text: String = row.spans.iter().map(|span| span.text.as_str()).collect();
            rows.push((expected, text));
        }
        let empty = rows.is_empty();
        if !empty {
            self.chunks.push_front(rows);
        }
        if page.start_row == 0 || empty {
            self.retained_floor = page.start_row;
            return Ok(true);
        }
        if page.start_row >= self.end_row {
            return Err(format!("retained marker page made no progress for {sid}"));
        }
        self.end_row = page.start_row;
        Ok(false)
    }

    /// The report over every admitted page.
    pub fn finish(self, prefix: &str) -> RetainedMarkerScan {
        let rows: Vec<(u64, String)> = self.chunks.into_iter().flatten().collect();
        let row_indices: Vec<u64> = rows.iter().map(|(index, _)| *index).collect();
        let row_gap_count = row_indices
            .windows(2)
            .filter(|pair| pair[1] != pair[0] + 1)
            .count() as u64;
        let marker_ids: Vec<u64> = rows
            .iter()
            .flat_map(|(_, text)| prefixed_markers(text, prefix))
            .collect();
        let tally = MarkerTally::of(&marker_ids);
        let total = self.scrollback_total.unwrap_or(0);
        RetainedMarkerScan {
            grid_epoch: self.grid_epoch,
            pages: self.pages,
            scrollback_total: total,
            retained_floor: self.retained_floor,
            retained_cap: total as i64 - self.retained_floor as i64,
            retained_floor_reason: self.floor_reason,
            row_indices,
            row_gap_count,
            marker_min: tally.min,
            marker_max: tally.max,
            marker_missing: tally.missing(),
            marker_duplicated: tally.duplicated(),
            marker_out_of_order: inversions(&marker_ids),
            marker_ids,
        }
    }
}
