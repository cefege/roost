//! When the next Sync dial happens: capped backoff, hidden-page parking, the
//! stale-link watchdog decision, and the smoke transport pause.
//!
//! Owned by `SyncState`; driven by `handle_sync::lifecycle` from link closes,
//! frames, visibility changes and the sweep. Ported from
//! `apps/web/src/store/sync-redial.ts` and the pure decisions of
//! `apps/web/src/store/sync-watchdog.ts`; the immediate-redial reasons are
//! `apps/web/src/client/sync/sync-flow.ts:72-78`.

/// The first redial delay, and the delay after any success.
pub const SYNC_REDIAL_BASE_MS: u64 = 1_000;
/// The redial delay ceiling.
pub const SYNC_REDIAL_MAX_MS: u64 = 30_000;
/// Consecutive failures at which the delay saturates: 1s 2s 4s 8s 16s 30s.
pub const SYNC_REDIAL_SATURATION_FAILURES: u32 = 6;
/// Consecutive failures a HIDDEN document tolerates before it sleeps until a
/// page-lifecycle resume instead of dialing on a throttled timer.
pub const SYNC_HIDDEN_PARK_FAILURES: u32 = 8;
/// An open socket silent this long (three missed 30 s keepalives) is stale.
pub const SYNC_STALE_TIMEOUT_MS: u64 = 90_000;
/// A refocused tab tolerates this much idle before replacing the socket.
pub const SYNC_REFOCUS_STALE_MS: u64 = 45_000;
/// Two lifecycle wakes closer than this are one wake.
pub const SYNC_RESUME_COALESCE_MS: u64 = 500;

/// Capped backoff for the Nth consecutive failed dial.
pub fn next_redial_delay_ms(failures: u32) -> u64 {
    let steps = failures
        .saturating_sub(1)
        .min(SYNC_REDIAL_SATURATION_FAILURES);
    (SYNC_REDIAL_BASE_MS << steps).min(SYNC_REDIAL_MAX_MS)
}

/// Whether the loop may sleep instead of dialing: only a hidden document past
/// the failure budget. A visible document never parks.
pub fn should_park_redial(failures: u32, visible: bool) -> bool {
    !visible && failures >= SYNC_HIDDEN_PARK_FAILURES
}

/// The close reasons that redial at once instead of counting as a failure.
pub fn is_immediate_sync_redial(reason: Option<&str>) -> bool {
    matches!(
        reason,
        Some("visibility" | "manual" | "stale" | "flow" | "terminal-liveness")
    )
}

/// Is there a Sync socket, and is it carrying traffic?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncLinkLiveness {
    /// No socket and no dial.
    None,
    /// A dial is in flight.
    Dialing,
    /// A socket is open.
    Open,
}

impl SyncLinkLiveness {
    /// The wire spelling v2's diagnostics use.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Dialing => "dialing",
            Self::Open => "open",
        }
    }
}

/// Resume decision: replace only an OPEN socket that went silent past the
/// refocus budget. A dial in flight already is the redial.
pub fn should_close_stale_link_on_resume(liveness: SyncLinkLiveness, idle_ms: u64) -> bool {
    liveness == SyncLinkLiveness::Open && idle_ms > SYNC_REFOCUS_STALE_MS
}

/// The redial loop's state. One per client, never reset by a credential change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncRedial {
    failures: u32,
    delay_ms: u64,
    hidden_parked: bool,
    resume_requested: bool,
    smoke_paused: bool,
    visible: bool,
    /// When the pending redial is due, or `None` with no redial pending.
    due_ms: Option<u64>,
    /// The reason the core itself gave for the close it asked the host for.
    pending_abort_reason: Option<String>,
    last_resume_ms: Option<u64>,
}

impl Default for SyncRedial {
    fn default() -> Self {
        Self {
            failures: 0,
            delay_ms: SYNC_REDIAL_BASE_MS,
            hidden_parked: false,
            resume_requested: false,
            smoke_paused: false,
            visible: true,
            due_ms: None,
            pending_abort_reason: None,
            last_resume_ms: None,
        }
    }
}

/// What `syncRedialStatus()` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncRedialStatus {
    /// Consecutive failed dials since the last frame.
    pub failures: u32,
    /// The delay the pending redial waits.
    pub next_delay_ms: u64,
    /// Whether a hidden document is sleeping instead of redialing.
    pub hidden_parked: bool,
}

impl SyncRedial {
    /// A frame arrived: the link works, so the backoff starts over.
    pub fn note_frame_received(&mut self) {
        self.failures = 0;
        self.delay_ms = SYNC_REDIAL_BASE_MS;
    }

    /// Remember why the core is closing the socket, so the close that follows
    /// is classified by the reason rather than as a failure.
    pub fn note_abort_reason(&mut self, reason: &str) {
        self.pending_abort_reason = Some(reason.to_owned());
    }

    /// Record the reason a close VERDICT implies, unless the core already chose
    /// one: v2 `sync.ts` `onclose` sets `abortReason = "flow"` only while it is
    /// still null, so this tab's own reason for closing wins over the peer's.
    pub fn note_close_abort_reason(&mut self, reason: &str) {
        if self.pending_abort_reason.is_none() {
            self.pending_abort_reason = Some(reason.to_owned());
        }
    }

    /// The socket closed: schedule the next dial (v2 `_waitForNextSyncDial`).
    pub fn schedule_after_close(&mut self, now_ms: u64) {
        let reason = self.pending_abort_reason.take();
        if is_immediate_sync_redial(reason.as_deref()) {
            self.delay_ms = SYNC_REDIAL_BASE_MS;
            self.due_ms = Some(now_ms);
            return;
        }
        self.failures = self.failures.saturating_add(1);
        self.delay_ms = next_redial_delay_ms(self.failures);
        if should_park_redial(self.failures, self.visible) {
            self.hidden_parked = true;
            self.due_ms = Some(now_ms);
            return;
        }
        if self.resume_requested {
            self.resume_requested = false;
            self.delay_ms = SYNC_REDIAL_BASE_MS;
            self.due_ms = Some(now_ms);
            return;
        }
        self.due_ms = Some(now_ms.saturating_add(self.delay_ms));
    }

    /// Wake a park and any backoff for an immediate dial (v2 `resumeSyncNow`).
    pub fn resume_now(&mut self, now_ms: u64) {
        self.hidden_parked = false;
        self.failures = 0;
        self.delay_ms = SYNC_REDIAL_BASE_MS;
        self.resume_requested = true;
        if self.due_ms.is_some() {
            self.due_ms = Some(now_ms);
        }
    }

    /// A page-lifecycle wake. `None` when it coalesces with the previous one or
    /// a hidden wake is not allowed to act; otherwise the wake was taken.
    pub fn take_lifecycle_wake(&mut self, now_ms: u64, allow_hidden: bool) -> Option<()> {
        if !allow_hidden && !self.visible {
            return None;
        }
        if self
            .last_resume_ms
            .is_some_and(|last| now_ms.saturating_sub(last) < SYNC_RESUME_COALESCE_MS)
        {
            return None;
        }
        self.last_resume_ms = Some(now_ms);
        self.resume_now(now_ms);
        Some(())
    }

    /// Record the document's visibility.
    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    /// Whether the document is visible, as last reported.
    pub fn visible(&self) -> bool {
        self.visible
    }

    /// Pause or release the smoke transport gate.
    pub fn set_smoke_paused(&mut self, paused: bool) {
        self.smoke_paused = paused;
    }

    /// Pre-arm the highest floor production can reach (smoke only).
    pub fn arm_floor(&mut self) {
        self.failures = SYNC_HIDDEN_PARK_FAILURES - 1;
        self.delay_ms = next_redial_delay_ms(SYNC_HIDDEN_PARK_FAILURES);
    }

    /// Whether the pending redial may dial now; consumes it when it may.
    pub fn take_due(&mut self, now_ms: u64) -> bool {
        if self.hidden_parked || self.smoke_paused {
            return false;
        }
        match self.due_ms {
            Some(due) if due <= now_ms => {
                self.due_ms = None;
                true
            }
            _ => false,
        }
    }

    /// A dial is starting: a pending resume is spent on it, so the close that
    /// follows waits its backoff (v2 `_waitForSyncDialPermission`, run before
    /// every dial, the boot dial included).
    pub fn note_dial_started(&mut self) {
        if self.resume_requested {
            self.resume_requested = false;
            self.delay_ms = SYNC_REDIAL_BASE_MS;
        }
    }

    /// Whether a redial is scheduled (parked or not).
    pub fn is_pending(&self) -> bool {
        self.due_ms.is_some()
    }

    /// The diagnostic status.
    pub fn status(&self) -> SyncRedialStatus {
        SyncRedialStatus {
            failures: self.failures,
            next_delay_ms: self.delay_ms,
            hidden_parked: self.hidden_parked,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backoff_doubles_from_one_second_and_saturates_at_thirty() {
        let delays: Vec<u64> = (1..=8).map(next_redial_delay_ms).collect();
        assert_eq!(
            delays,
            [1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000, 30_000]
        );
        assert_eq!(next_redial_delay_ms(0), 1_000);
    }

    #[test]
    fn only_a_hidden_document_past_the_budget_parks() {
        assert!(!should_park_redial(SYNC_HIDDEN_PARK_FAILURES, true));
        assert!(!should_park_redial(SYNC_HIDDEN_PARK_FAILURES - 1, false));
        assert!(should_park_redial(SYNC_HIDDEN_PARK_FAILURES, false));
    }

    #[test]
    fn an_immediate_reason_redials_now_and_a_failure_waits_its_backoff() {
        let mut redial = SyncRedial::default();
        redial.note_abort_reason("stale");
        redial.schedule_after_close(100);
        assert!(redial.take_due(100));
        assert_eq!(redial.status().failures, 0);
        redial.schedule_after_close(200);
        assert!(!redial.take_due(1_199));
        assert!(redial.take_due(1_200));
        redial.schedule_after_close(2_000);
        assert_eq!(redial.status().next_delay_ms, 2_000);
        assert!(!redial.take_due(3_999));
    }

    #[test]
    fn a_hidden_park_waits_for_a_resume_and_a_frame_resets_the_count() {
        let mut redial = SyncRedial::default();
        redial.set_visible(false);
        redial.arm_floor();
        redial.schedule_after_close(0);
        assert!(redial.status().hidden_parked);
        assert!(!redial.take_due(u64::MAX));
        assert!(redial.take_lifecycle_wake(10, true).is_some());
        assert!(redial.take_due(10));
        redial.note_frame_received();
        assert_eq!(redial.status().failures, 0);
    }

    #[test]
    fn a_frame_after_failed_dials_restarts_the_backoff_at_one_second() {
        let mut redial = SyncRedial::default();
        redial.schedule_after_close(0);
        assert!(redial.take_due(1_000));
        redial.schedule_after_close(1_000);
        assert_eq!(redial.status().failures, 2);
        redial.note_frame_received();
        assert_eq!(redial.status().failures, 0);
        redial.schedule_after_close(5_000);
        assert_eq!(redial.status().next_delay_ms, 1_000);
        assert!(redial.take_due(6_000));
    }

    #[test]
    fn a_smoke_pause_holds_the_dial_until_released() {
        let mut redial = SyncRedial::default();
        redial.set_smoke_paused(true);
        redial.note_abort_reason("manual");
        redial.schedule_after_close(0);
        assert!(!redial.take_due(0));
        redial.set_smoke_paused(false);
        assert!(redial.take_due(0));
    }

    #[test]
    fn only_an_open_silent_link_is_replaced_on_resume() {
        assert!(should_close_stale_link_on_resume(
            SyncLinkLiveness::Open,
            45_001
        ));
        assert!(!should_close_stale_link_on_resume(
            SyncLinkLiveness::Open,
            45_000
        ));
        assert!(!should_close_stale_link_on_resume(
            SyncLinkLiveness::Dialing,
            90_000
        ));
    }
}
