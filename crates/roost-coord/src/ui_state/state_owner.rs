//! Bounded, TTL-scoped retention for one coordinator's browser tab reports.
//!
//! Ported from `apps/coord/src/ui-state/ui-state-owner.ts`. The report is
//! volatile by design: a browser tab that stops reporting is dropped after the
//! TTL rather than left in the Sync feed for every other client to see.
//! Read by the four UI handlers, and by the Sync seed once that slice lands.
//!
//! THE REAP IS LAZY, NOT ON A TIMER. v2 arms a 60-second interval as well as
//! reaping on access; the interval only exists to release memory on a
//! coordinator nobody is reading, and what it bounds is already capped at
//! `UI_STATE_MAX_TABS_TOTAL` entries. Every entry point here reaps first, so a
//! report is never observable past its TTL.

use std::sync::{Arc, Mutex};

use roost_proto::UiReportStateRequest;

use crate::ui_state::identity_rate::{AllowEveryIdentityRateLimiter, IdentityRateLimiter};
use crate::ui_state::limits::{
    UI_STATE_IDENTITY_WINDOW_MS, UI_STATE_MAX_TABS_PER_FINGERPRINT, UI_STATE_MAX_TABS_TOTAL,
    UI_STATE_NEW_IDENTITIES_PER_WINDOW,
};

/// How long a report survives without a heartbeat from the same tab.
///
/// Five minutes. A tab heartbeats far more often than that, so the TTL is a
/// bound on how long a dead browser stays visible in another tab's deck, not a
/// scheduling requirement.
pub const UI_STATE_TTL_MS: i64 = 5 * 60_000;

/// The budget group new tab identities are admitted under.
pub const UI_STATE_RATE_GROUP: &str = "ui-state-new-identity";

/// One retained report, as `UiListStates` and the Sync seed read it.
#[derive(Debug, Clone, PartialEq)]
pub struct UiTabEntry {
    /// The browser key fingerprint that reported it.
    pub fingerprint: String,
    /// The tab id the report was filed under.
    pub tab_id: String,
    /// When the tab last reported, in epoch milliseconds.
    pub last_ms: i64,
    /// The canonical report, rebuilt from the request's known fields.
    pub state: UiReportStateRequest,
}

/// Why a report was refused. Both variants answer the browser with 429.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiStateReportError {
    /// The aggregate or per-fingerprint tab cap is reached.
    Capacity,
    /// The new-identity budget for this fingerprint is spent.
    IdentityRate,
}

impl UiStateReportError {
    /// The message a browser reads, naming the bound it hit.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::Capacity => "ui state tab capacity exhausted",
            Self::IdentityRate => "ui state new tab identity rate exhausted",
        }
    }
}

/// The retained reports, keyed by `(fingerprint, tab_id)`.
///
/// A `BTreeMap` rather than a `HashMap` so `list` is ordered by identity and two
/// coordinators fed the same reports produce the same list.
type ReportsByTab = std::collections::BTreeMap<(String, String), UiTabEntry>;

/// One coordinator's retained tab reports.
#[derive(Clone)]
pub struct UiStateOwner {
    reports: Arc<Mutex<ReportsByTab>>,
    now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
    identity_rate_limiter: Arc<dyn IdentityRateLimiter>,
    max_tabs_per_fingerprint: usize,
    max_tabs_total: usize,
    new_identities_per_window: usize,
    identity_window_ms: i64,
}

impl std::fmt::Debug for UiStateOwner {
    /// The owner holds a clock and a limiter, and a log line wants the retained
    /// depth rather than either of their values.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UiStateOwner")
            .field("retained", &self.retained_count())
            .finish()
    }
}

impl Default for UiStateOwner {
    fn default() -> Self {
        Self::new()
    }
}

impl UiStateOwner {
    /// An owner over the real clock and the allow-all identity limiter.
    #[must_use]
    pub fn new() -> Self {
        Self::with_clock(Arc::new(crate::serve::now_ms))
    }

    /// An owner whose clock the caller supplies, so a test never waits for this
    /// one to tick.
    #[must_use]
    pub fn with_clock(now_ms: Arc<dyn Fn() -> i64 + Send + Sync>) -> Self {
        Self {
            reports: Arc::new(Mutex::new(ReportsByTab::new())),
            now_ms,
            identity_rate_limiter: Arc::new(AllowEveryIdentityRateLimiter),
            max_tabs_per_fingerprint: UI_STATE_MAX_TABS_PER_FINGERPRINT,
            max_tabs_total: UI_STATE_MAX_TABS_TOTAL,
            new_identities_per_window: UI_STATE_NEW_IDENTITIES_PER_WINDOW,
            identity_window_ms: UI_STATE_IDENTITY_WINDOW_MS,
        }
    }

    /// The same owner, admitting new identities through `limiter`.
    #[must_use]
    pub fn with_identity_rate_limiter(mut self, limiter: Arc<dyn IdentityRateLimiter>) -> Self {
        self.identity_rate_limiter = limiter;
        self
    }

    /// Retain or refresh one tab's report.
    ///
    /// An existing `(fingerprint, tab_id)` refreshes in place and consumes no
    /// admission budget, which is what keeps a live tab's heartbeat working when
    /// a device opens a burst of new ones.
    pub fn report(
        &self,
        fingerprint: &str,
        tab_id: &str,
        state: UiReportStateRequest,
    ) -> Result<(), UiStateReportError> {
        let now = (self.now_ms)();
        let key = (fingerprint.to_owned(), tab_id.to_owned());
        let mut reports = self.lock();
        self.reap_expired(&mut reports, now);
        if let Some(existing) = reports.get_mut(&key) {
            existing.last_ms = now;
            existing.state = state;
            return Ok(());
        }
        if self.at_capacity(&reports, fingerprint) {
            return Err(UiStateReportError::Capacity);
        }
        if !self.identity_rate_limiter.consume(
            fingerprint,
            UI_STATE_RATE_GROUP,
            self.new_identities_per_window,
            self.identity_window_ms,
            now,
        ) {
            tracing::warn!(
                event = "ui-state",
                action = "identity_rate_limited",
                identity_scope = fingerprint,
                limiter_capacity = self.new_identities_per_window,
                "a device spent its new tab identity budget"
            );
            return Err(UiStateReportError::IdentityRate);
        }
        reports.insert(
            key,
            UiTabEntry {
                fingerprint: fingerprint.to_owned(),
                tab_id: tab_id.to_owned(),
                last_ms: now,
                state,
            },
        );
        Ok(())
    }

    /// Every retained report, oldest heartbeat first, after the TTL reap.
    #[must_use]
    pub fn list(&self) -> Vec<UiTabEntry> {
        let now = (self.now_ms)();
        let mut reports = self.lock();
        self.reap_expired(&mut reports, now);
        reports.values().cloned().collect()
    }

    /// How many reports are retained, for a log line or a test.
    #[must_use]
    pub fn retained_count(&self) -> usize {
        self.lock().len()
    }

    /// Drop every report and admit nothing until the next report.
    pub fn dispose(&self) {
        self.lock().clear();
    }

    /// Whether this fingerprint is already at its own cap, or the whole
    /// coordinator is at the aggregate one.
    fn at_capacity(&self, reports: &ReportsByTab, fingerprint: &str) -> bool {
        if reports.len() >= self.max_tabs_total {
            return true;
        }
        reports
            .keys()
            .filter(|(held, _)| held == fingerprint)
            .count()
            >= self.max_tabs_per_fingerprint
    }

    fn reap_expired(&self, reports: &mut ReportsByTab, now_ms: i64) {
        reports.retain(|_, entry| now_ms - entry.last_ms <= UI_STATE_TTL_MS);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ReportsByTab> {
        // A poisoned lock means a reporting thread panicked mid-insert, and the
        // map is still structurally valid: every mutation here is a whole entry
        // swap, so recovering the guard loses nothing and keeps one bad report
        // from blinding the UI surface for the life of the process.
        self.reports
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
