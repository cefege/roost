//! The shared cursor-poll ticker: one 500ms interval for EVERY mounted pane,
//! armed by the first registration and stopped by the last.
//!
//! The deck keeps every open session mounted, so a ticker per pane would mean
//! a timer per open session. This module owns only the DECISION — who is
//! registered, whether an interval is wanted, and whether one poll has anything
//! to report — and every clock value arrives as a parameter. The interval
//! itself is the caller's.

use std::collections::BTreeMap;

/// The shared cursor-poll interval. Short enough that another viewer's ghost
/// cursor is not visibly behind, long enough that a pane nobody is looking at
/// costs a comparison rather than a write.
pub const CURSOR_POLL_INTERVAL_MS: u64 = 500;

/// One mounted pane's identity in the shared poll.
///
/// The ticker never interprets it — it only needs identities that compare and
/// de-duplicate, because unregistering the same pane twice must not stop an
/// interval a sibling pane is still riding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CursorPollPane(u64);

impl CursorPollPane {
    /// Name a pane by the caller's own identity for it.
    pub const fn new(id: u64) -> Self {
        Self(id)
    }
}

/// A pane's cursor as its own last delivery read it, read on every poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorPollReading {
    /// Whether this pane may do foreground work at all: its view is active and
    /// the document is visible. A pane that may not is not polled, and its
    /// remembered position is left alone, so the first poll after it returns
    /// reports the position it is actually at.
    pub foreground_work_allowed: bool,
    pub cursor_row: u32,
    pub cursor_col: u32,
}

/// The position a poll must publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorPosition {
    pub row: u32,
    pub col: u32,
}

/// What one pane's last reported position was.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct CursorPollPaneState {
    /// `None` until the pane's first poll. It is the "never reported" sentinel,
    /// and it is what makes the first poll always report — including for a
    /// pane whose cursor happens to sit at the origin.
    last_reported: Option<CursorPosition>,
}

impl CursorPollPaneState {
    /// Consume this reading and answer the position to publish, or `None` when
    /// the poll has nothing to say. A pane whose cursor has not moved is the
    /// reason a mounted-but-idle pane costs nothing.
    fn take_due(&mut self, reading: CursorPollReading) -> Option<CursorPosition> {
        if !reading.foreground_work_allowed {
            return None;
        }
        let current = CursorPosition {
            row: reading.cursor_row,
            col: reading.cursor_col,
        };
        if self.last_reported == Some(current) {
            return None;
        }
        self.last_reported = Some(current);
        Some(current)
    }
}

/// The document's one cursor-poll interval, and the panes riding it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CursorPollTicker {
    panes: BTreeMap<CursorPollPane, CursorPollPaneState>,
    /// When the armed interval comes round, or `None` when none is armed.
    next_due_ms: Option<u64>,
}

impl CursorPollTicker {
    /// A ticker with no panes and no interval.
    pub const fn new() -> Self {
        Self {
            panes: BTreeMap::new(),
            next_due_ms: None,
        }
    }

    /// Register a pane and answer the deadline the caller must arm, or `None`
    /// when an interval is already armed and this pane simply rides it.
    ///
    /// Registering the same pane twice is one registration, not two: a
    /// re-mounted pane that kept its handle must not be polled twice, and must
    /// not hold the interval open after it really is gone.
    pub fn register(&mut self, pane: CursorPollPane, now_ms: u64) -> Option<u64> {
        self.panes.entry(pane).or_default();
        if self.next_due_ms.is_some() {
            return None;
        }
        let due = now_ms.saturating_add(CURSOR_POLL_INTERVAL_MS);
        self.next_due_ms = Some(due);
        Some(due)
    }

    /// Remove a pane and answer whether the caller must stop the interval.
    ///
    /// It answers true only for the LAST pane, so a double unregister can never
    /// stop an interval a sibling pane is still riding.
    pub fn unregister(&mut self, pane: CursorPollPane) -> bool {
        self.panes.remove(&pane);
        if !self.panes.is_empty() {
            return false;
        }
        self.next_due_ms.take().is_some()
    }

    /// Remove every mounted pane at a credential boundary and answer whether
    /// the caller must stop the interval. A credential boundary leaves no
    /// mounted pane whose cursor position may still be published.
    pub fn reset(&mut self) -> bool {
        self.panes.clear();
        self.next_due_ms.take().is_some()
    }

    /// Whether the shared interval has come round at `now_ms`, consuming it.
    ///
    /// It answers the NEXT deadline from `now_ms` rather than from the one it
    /// slept through, so a caller whose timer was throttled by a background tab
    /// re-arms once instead of firing once per interval it missed.
    pub fn take_due(&mut self, now_ms: u64) -> bool {
        let Some(due) = self.next_due_ms else {
            return false;
        };
        if now_ms < due {
            return false;
        }
        self.next_due_ms = Some(now_ms.saturating_add(CURSOR_POLL_INTERVAL_MS));
        true
    }

    /// The deadline the caller must arm for, or `None` when nothing is armed.
    pub fn due_at_ms(&self) -> Option<u64> {
        self.next_due_ms
    }

    /// Whether an interval is armed.
    pub fn is_armed(&self) -> bool {
        self.next_due_ms.is_some()
    }

    /// How many panes are riding the interval.
    pub fn registered_panes(&self) -> usize {
        self.panes.len()
    }

    /// The position this pane must publish on this poll, consuming the change.
    /// `None` for a pane that is not registered, may not do foreground work, or
    /// whose cursor has not moved.
    pub fn poll(&mut self, pane: CursorPollPane, reading: CursorPollReading) -> Option<CursorPosition> {
        self.panes.get_mut(&pane)?.take_due(reading)
    }
}
