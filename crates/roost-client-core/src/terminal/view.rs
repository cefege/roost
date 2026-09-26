//! One pane's lease over a session replica: what it wants the authority to show,
//! and whether the authority has agreed.
//!
//! A view is the client half of the view authority the spec describes
//! (`protocol/spec/terminal-stream.md:24-26`). It publishes its effective size,
//! renews at the heartbeat interval, and requires a generation-matched
//! acknowledgement before its lease expires. Explicit hide, authorization loss,
//! and durable close remove it immediately — no lease wait.
//!
//! Ported from `apps/web/src/store/terminal-stream-view.ts` and
//! `apps/web/src/store/terminal-stream-view-commands.ts`.

use roost_protocol::viewport::{TERMINAL_VIEW_HEARTBEAT_MS, TERMINAL_VIEW_LEASE_MS};

/// What a view wants the authority to do for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewIntent {
    /// Hold the view at this effective size. The authority computes the
    /// independent-axis minimum across every active view, and a size change
    /// mints a new stream id for everyone.
    Publish {
        /// The pane's effective columns.
        cols: u32,
        /// The pane's effective rows.
        rows: u32,
    },
    /// Keep the view but stop constraining geometry — a hidden pane still holds
    /// its place in the authority's membership, it just stops counting.
    Park,
    /// Remove the view immediately. This is what an explicit hide and a pane
    /// close both send, and it is the only path that does not wait for a lease.
    Unpublish,
}

/// What the authority said about a view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewStateResult {
    /// The session.
    pub session_id: String,
    /// The view the command named.
    pub view_id: String,
    /// The generation the acknowledgement belongs to. A result for any other
    /// generation is stale and changes nothing.
    pub generation: u64,
    /// Whether the authority holds the view.
    pub accepted: bool,
    /// The stream id the authority is now minting, when it accepted.
    pub stream_id: Option<String>,
}

/// One pane's view record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalView {
    /// The view's identity. Stable for the life of the pane, which is what lets
    /// a renderer keep its DOM across a transport change.
    pub view_id: String,
    /// The columns the pane last published.
    pub cols: u32,
    /// The rows the pane last published.
    pub rows: u32,
    /// The intent the pane currently wants sent.
    pub intent: ViewIntent,
    /// The generation whose command is still unacknowledged, if any.
    pub unacknowledged: Option<u64>,
    /// When the view last published, for the heartbeat.
    pub published_at_ms: u64,
    /// When the authority last acknowledged, for the lease.
    pub acknowledged_at_ms: u64,
    /// Whether the view is counted by the authority's geometry right now.
    pub counted: bool,
}

impl TerminalView {
    /// A newly opened view at a size. It has published nothing yet, so
    /// `published_at_ms` is zero and the first heartbeat is due immediately.
    pub fn opened(view_id: impl Into<String>, cols: u32, rows: u32, _now_ms: u64) -> Self {
        Self {
            view_id: view_id.into(),
            cols,
            rows,
            intent: ViewIntent::Publish { cols, rows },
            unacknowledged: None,
            published_at_ms: 0,
            acknowledged_at_ms: 0,
            counted: false,
        }
    }

    /// Record a new effective size.
    ///
    /// A size change is not just a new `Publish`: the authority mints a NEW
    /// stream id for it, so the replica's expectation is invalidated and the
    /// pane has to wait for a fresh baseline. Returning true is the caller's
    /// signal to clear liveness and require a baseline.
    pub fn resize(&mut self, cols: u32, rows: u32) -> bool {
        if self.cols == cols && self.rows == rows {
            return false;
        }
        self.cols = cols;
        self.rows = rows;
        self.intent = ViewIntent::Publish { cols, rows };
        true
    }

    /// Hide the pane: it keeps its place but stops constraining geometry.
    pub fn park(&mut self) {
        self.intent = ViewIntent::Park;
        self.counted = false;
    }

    /// Whether the heartbeat interval has elapsed and this view must republish.
    ///
    /// The heartbeat is not politeness. A dropped transport parks a view, and a
    /// parked view stops constraining geometry after the park grace — so a pane
    /// that has gone quiet for three heartbeats releases the session's minimum
    /// size without anyone noticing it had left.
    pub fn heartbeat_due(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.published_at_ms) >= TERMINAL_VIEW_HEARTBEAT_MS
    }

    /// Whether the lease has expired without a matching acknowledgement.
    pub fn lease_expired(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.acknowledged_at_ms) >= TERMINAL_VIEW_LEASE_MS
    }

    /// Record that this view's command was sent on `generation`.
    pub fn mark_published(&mut self, generation: u64, now_ms: u64) {
        self.unacknowledged = Some(generation);
        self.published_at_ms = now_ms;
    }

    /// Record a generation-matched acknowledgement.
    ///
    /// A result for a generation this view is not waiting on is stale — it is
    /// the answer to a command from a socket that has since been replaced — and
    /// it must not satisfy the current lease. Returns false for that case, so
    /// the caller can log it and change nothing.
    ///
    /// The stream id the authority answered with belongs to the session's
    /// expectation, not to the view, so the caller reads it off the
    /// `ViewStateResult` and installs it there.
    pub fn acknowledge(&mut self, generation: u64, now_ms: u64) -> bool {
        if self.unacknowledged != Some(generation) {
            return false;
        }
        self.unacknowledged = None;
        self.acknowledged_at_ms = now_ms;
        self.counted = true;
        true
    }
}
