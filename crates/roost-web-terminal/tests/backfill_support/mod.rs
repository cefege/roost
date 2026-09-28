//! Shared fixture for the scrollback demand-paging suites, ported from
//! `apps/web/tests/scrollbackBackfill-test-harness.ts`: a renderer stand-in
//! (`BackfillHost`) modelling ABSOLUTE painted rows and exact missing intervals,
//! the page builder, and a driver that performs the pager's actions — reads,
//! frames, timers on a fake clock — while capturing its `tracing` diag lines.

#![allow(dead_code)]

pub mod capture;

use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;

use roost_client_core::terminal::history::{HistoryRange, HistoryScrollTarget};
use roost_protocol::cell::{CellRow, CellSpan};
use roost_protocol::terminal_search::ScrollbackHistoryFloor;
use roost_web_terminal::BackfillAnchor;
use roost_web_terminal::backfill::{
    BackfillAction, BackfillHost, ScrollbackBackfill, ScrollbackPage, ScrollbackPageRequest,
};

use capture::CapturedEvent;

pub const GRID_EPOCH: &str = "test-grid:0";

/// Painted head and tail around one interior hole `[100, 200)`.
pub fn interior_painted() -> Vec<u32> {
    (0..100).chain(200..300).collect()
}

/// A page answering `[start, end)` under `total`, on the harness grid.
pub fn response(start: u32, end: u32, total: u64) -> ScrollbackPage {
    ScrollbackPage {
        rows: (start..end).map(history_row).collect(),
        start_row: u64::from(start),
        end_row: u64::from(end),
        cols: 80,
        scrollback_total: total,
        grid_epoch: GRID_EPOCH.to_string(),
        history_floor: ScrollbackHistoryFloor::None,
    }
}

fn history_row(index: u32) -> CellRow {
    CellRow {
        index,
        spans: Arc::from([CellSpan {
            text: format!("row-{index}"),
            fg: 256,
            bg: 256,
            flags: 0,
            fg_rgb: None,
            bg_rgb: None,
            columns: 5,
            link_uri: None,
            link_key: None,
        }]),
    }
}

/// The renderer stand-in: no DOM, no layout, no `scrollTop`.
#[derive(Debug)]
pub struct FakeRenderer {
    pub anchor: BackfillAnchor,
    pub painted: BTreeSet<u32>,
    pub insertions: Vec<Vec<u32>>,
    pub floor_rows: Vec<u32>,
    pub focus: Option<u32>,
    pub bottom: bool,
}

impl FakeRenderer {
    fn total(&self) -> u32 {
        u32::try_from(self.anchor.total).expect("a harness total fits a row")
    }

    fn missing(&self, row: u32) -> Option<HistoryRange> {
        if row >= self.total() || self.painted.contains(&row) {
            return None;
        }
        let mut start = row;
        let mut end = row + 1;
        while start > 0 && !self.painted.contains(&(start - 1)) {
            start -= 1;
        }
        while end < self.total() && !self.painted.contains(&end) {
            end += 1;
        }
        Some(HistoryRange { start, end })
    }
}

impl BackfillHost for FakeRenderer {
    fn backfill_anchor(&self) -> Option<BackfillAnchor> {
        Some(self.anchor.clone())
    }
    fn follows_bottom(&self) -> bool {
        self.bottom
    }
    fn has_painted_scrollback_range(&self, start: u32, end: u32) -> bool {
        start < end && end <= self.total() && (start..end).all(|row| self.painted.contains(&row))
    }
    fn missing_scrollback_range(&self, row: u32) -> Option<HistoryRange> {
        self.missing(row)
    }
    /// One-row viewport at the focus row, widened upward exactly like the real
    /// helper: the bottom-most missing interval inside the window wins.
    fn missing_scrollback_range_at_scroll(&self, ahead_rows: u32) -> Option<HistoryScrollTarget> {
        let focus = self.focus?;
        let top = focus.saturating_sub(ahead_rows);
        (top..=focus).rev().find_map(|row| {
            let gap = self.missing(row)?;
            let focus_row = gap.start.max(top);
            Some(HistoryScrollTarget {
                missing: gap,
                in_window: HistoryRange { start: focus_row, end: row + 1 },
                focus_row,
            })
        })
    }
    fn set_history_floor(&mut self, row: u32) {
        self.floor_rows.push(row);
    }
    /// One placeholder per page, exactly like the renderer: the head spacer
    /// stands over `[0, sb_base)`, a gap element over each missing interval
    /// above it, and no insert spans two of them.
    fn insert_history_page(&mut self, rows: &[CellRow], _follow_tail: bool) -> bool {
        let Some(start) = rows.first().map(|row| row.index) else {
            return false;
        };
        let end = start + rows.len() as u32;
        let fits = self.missing(start).is_some_and(|gap| end <= gap.end);
        let spans_base = start < self.anchor.sb_base && end > self.anchor.sb_base;
        let contiguous = rows.iter().zip(start..).all(|(row, index)| row.index == index);
        if !fits || spans_base || !contiguous {
            return false;
        }
        self.insertions.push(rows.iter().map(|row| row.index).collect());
        self.painted.extend(start..end);
        self.anchor.sb_base = self.anchor.sb_base.min(start);
        true
    }
}

/// The shape of one harness, v2's `BackfillHarnessOptions`.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub total: Option<u64>,
    pub painted: Vec<u32>,
    pub painted_base: Option<u32>,
    pub focus: Option<u32>,
    pub session_id: Option<&'static str>,
    pub bottom: bool,
}

/// How the fake carrier answers one read.
pub enum Reply {
    Page(ScrollbackPage),
    Pending,
    Fail,
}

enum Timer {
    FetchRetry(u64),
    DeferredRearm(u64),
}

/// A pager over a fake renderer, with the pane's side of every action.
pub struct Harness {
    pub pager: ScrollbackBackfill,
    pub host: FakeRenderer,
    pub calls: Vec<ScrollbackPageRequest>,
    pub pending: Vec<u64>,
    pub find_results: Vec<(u32, bool)>,
    responder: Box<dyn FnMut(&ScrollbackPageRequest) -> Reply>,
    now_ms: u64,
    timers: Vec<(u64, Timer)>,
    events: capture::Capture,
}

impl Harness {
    /// v2's default read: whatever was asked for, under a 760-row total.
    pub fn new(options: Options) -> Self {
        let total = options.total.unwrap_or(760);
        let default_base = options.painted.first().copied().unwrap_or(total as u32);
        let host = FakeRenderer {
            anchor: BackfillAnchor {
                sb_base: options.painted_base.unwrap_or(default_base),
                cols: 80,
                total,
                grid_epoch: GRID_EPOCH.to_string(),
            },
            painted: options.painted.iter().copied().collect(),
            insertions: Vec::new(),
            floor_rows: Vec::new(),
            focus: options.focus,
            bottom: options.bottom,
        };
        Self {
            pager: ScrollbackBackfill::new(options.session_id.unwrap_or("session-1")),
            host,
            calls: Vec::new(),
            pending: Vec::new(),
            find_results: Vec::new(),
            responder: Box::new(|request| {
                Reply::Page(response(request.end_row - request.max_rows, request.end_row, 760))
            }),
            now_ms: 0,
            timers: Vec::new(),
            events: capture::Capture::install(),
        }
    }

    pub fn respond_with(&mut self, responder: impl FnMut(&ScrollbackPageRequest) -> Reply + 'static) {
        self.responder = Box::new(responder);
    }
    pub fn scroll(&mut self) {
        let actions = self.pager.on_user_scroll(&mut self.host);
        self.perform(actions);
    }
    pub fn full_frame(&mut self) {
        let actions = self.pager.on_full_frame(&mut self.host);
        self.perform(actions);
    }
    pub fn ensure_row_painted(&mut self, row: u32) {
        let actions = self.pager.ensure_row_painted(row, &mut self.host);
        self.perform(actions);
    }
    pub fn suspend(&mut self) {
        let actions = self.pager.suspend();
        self.perform(actions);
    }
    pub fn dispose(&mut self) {
        let actions = self.pager.dispose();
        self.perform(actions);
    }
    /// Answer the oldest unanswered read with `page`.
    pub fn resolve_oldest(&mut self, page: ScrollbackPage) {
        let wave = self.pending.remove(0);
        let actions = self.pager.on_page(wave, page, &mut self.host);
        self.perform(actions);
    }
    /// Advance the fake clock, firing every timer that comes due on the way.
    pub fn advance(&mut self, delta_ms: u64) {
        let target = self.now_ms + delta_ms;
        while let Some(next) = (0..self.timers.len())
            .filter(|index| self.timers[*index].0 <= target)
            .min_by_key(|index| self.timers[*index].0)
        {
            let (due, timer) = self.timers.remove(next);
            self.now_ms = due;
            let actions = match timer {
                Timer::FetchRetry(wave) => self.pager.on_fetch_retry_due(wave, &mut self.host),
                Timer::DeferredRearm(token) => self.pager.on_deferred_rearm_due(token, &mut self.host),
            };
            self.perform(actions);
        }
        self.now_ms = target;
    }
    pub fn requested(&self) -> Vec<(u32, u32)> {
        self.calls.iter().map(|call| (call.end_row, call.max_rows)).collect()
    }
    pub fn events(&self) -> Vec<CapturedEvent> {
        self.events.events()
    }
    pub fn events_named(&self, message: &str) -> Vec<CapturedEvent> {
        self.events().into_iter().filter(|event| event.message == message).collect()
    }

    fn perform(&mut self, actions: Vec<BackfillAction>) {
        let mut queue: VecDeque<BackfillAction> = actions.into();
        while let Some(action) = queue.pop_front() {
            let next = match action {
                BackfillAction::Fetch { wave, request } => {
                    self.calls.push(request.clone());
                    match (self.responder)(&request) {
                        Reply::Page(page) => self.pager.on_page(wave, page, &mut self.host),
                        Reply::Pending => {
                            self.pending.push(wave);
                            Vec::new()
                        }
                        Reply::Fail => self.pager.on_fetch_failed(wave, &mut self.host),
                    }
                }
                BackfillAction::AwaitAnimationFrame { wave } => {
                    self.pager.on_animation_frame(wave, &mut self.host)
                }
                BackfillAction::ArmFetchRetry { wave, delay_ms } => {
                    self.timers.push((self.now_ms + delay_ms, Timer::FetchRetry(wave)));
                    Vec::new()
                }
                BackfillAction::ArmDeferredRearm { timer, delay_ms } => {
                    self.timers.push((self.now_ms + delay_ms, Timer::DeferredRearm(timer)));
                    Vec::new()
                }
                BackfillAction::FindSettled { row, painted } => {
                    self.find_results.push((row, painted));
                    Vec::new()
                }
            };
            queue.extend(next);
        }
    }
}
