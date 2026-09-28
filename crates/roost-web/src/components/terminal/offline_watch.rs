//! When a VIEWED pane whose view cannot deliver output has stayed that way
//! long enough to be "not responding": silent re-claims first, the offline
//! notice only once they all failed. Output silence is never evidence; only a
//! detached view the operator is looking at is. Target-independent; the wasm
//! pane mount feeds it and fires its deadline. Ports
//! `apps/web/src/browser/offlineWatch.ts` (used only by the terminal pane).

/// A viewed, detached pane waits this long before each silent re-claim.
pub const OFFLINE_GRACE_MS: u64 = 3_000;

/// Silent re-claims spent before the notice shows.
pub const OFFLINE_RETRIES: u32 = 2;

/// What one fired deadline asks of the pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfflineFire {
    /// Re-claim the view silently; another grace is armed.
    Retry,
    /// The budget is spent: show the offline notice.
    Offline,
}

/// One pane's accusation state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OfflineWatch {
    due_ms: Option<u64>,
    offline: bool,
    attempts: u32,
}

impl OfflineWatch {
    /// Not armed, not offline.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the notice shows.
    pub fn offline(&self) -> bool {
        self.offline
    }

    /// When the host must call `on_deadline`.
    pub fn next_deadline_ms(&self) -> Option<u64> {
        self.due_ms
    }

    /// Feed the pane facts. A fresh frame, a deliverable view, or nobody
    /// looking clears everything and refreshes the retry budget; a viewed,
    /// detached, quiet pane arms the grace once. Answers true when `offline`
    /// changed.
    pub fn update(
        &mut self,
        viewed: bool,
        detached: bool,
        painted_recently: bool,
        now_ms: u64,
    ) -> bool {
        if painted_recently || !viewed || !detached {
            self.due_ms = None;
            self.attempts = 0;
            return self.set(false);
        }
        if !self.offline && self.due_ms.is_none() {
            self.due_ms = Some(now_ms + OFFLINE_GRACE_MS);
        }
        false
    }

    /// The grace ran out with the view still undeliverable.
    pub fn on_deadline(&mut self, now_ms: u64) -> Option<OfflineFire> {
        if !self.due_ms.is_some_and(|due| due <= now_ms) {
            return None;
        }
        if self.attempts < OFFLINE_RETRIES {
            self.attempts += 1;
            self.due_ms = Some(now_ms + OFFLINE_GRACE_MS);
            tracing::info!(target: "terminal", attempt = self.attempts, "cell.offline_retry");
            return Some(OfflineFire::Retry);
        }
        self.due_ms = None;
        self.set(true);
        Some(OfflineFire::Offline)
    }

    /// Cancel the pending grace (teardown).
    pub fn dispose(&mut self) {
        self.due_ms = None;
    }

    fn set(&mut self, offline: bool) -> bool {
        if self.offline == offline {
            return false;
        }
        self.offline = offline;
        tracing::info!(target: "terminal", offline, "terminal pane offline changed");
        true
    }
}
