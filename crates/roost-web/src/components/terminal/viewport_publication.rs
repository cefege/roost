//! When one pane publishes its terminal geometry: the 50 ms trailing resize
//! debounce, the frame-then-100 ms retry of a transiently unmeasured box, and
//! the 250 ms grace that absorbs a zero-sized deck instead of withdrawing.
//! Target-independent: the pane's wasm mount arms real timers at
//! `next_deadline_ms` and an animation frame while `wants_animation_frame`.
//! Ports `apps/web/src/components/terminal/cell-terminal-viewport.ts`.

/// Trailing debounce of a resize burst.
pub const VIEWPORT_DEBOUNCE_MS: u64 = 50;
/// The second retry of an unmeasured box, after its animation-frame retry.
pub const UNMEASURED_VIEWPORT_RETRY_MS: u64 = 100;
/// A withdraw caused by a zero-sized deck waits this long: far below the view
/// heartbeat, above a couple of layout ticks.
pub const LAYOUT_GAP_PARK_GRACE_MS: u64 = 250;

/// What publication reads and writes on the pane.
pub trait ViewportHost {
    /// The pane's grid for its measured box, or `None` while it measures zero.
    fn measure(&mut self) -> Option<(u32, u32)>;
    /// Mounted, not pending, page visible, and the view active.
    fn should_publish_active(&self) -> bool;
    /// A display and a view both exist.
    fn has_view(&self) -> bool;
    /// Claim the view at this geometry.
    fn publish(&mut self, cols: u32, rows: u32);
    /// Stop claiming: clear frame activity and the cursor blink, suspend the
    /// pager, and mark the view inactive.
    fn withdraw(&mut self);
    /// Release every paint hold before a park.
    fn release_paint_holds(&mut self);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum RetryPhase {
    #[default]
    Idle,
    Frame,
    Timer,
}

/// One pane's publication timers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViewportPublication {
    debounce_due_ms: Option<u64>,
    retry_phase: RetryPhase,
    retry_frame_armed: bool,
    retry_due_ms: Option<u64>,
    grace_due_ms: Option<u64>,
}

impl ViewportPublication {
    /// Nothing armed.
    pub fn new() -> Self {
        Self::default()
    }

    /// The earliest timer the host must fire `on_deadline` at.
    pub fn next_deadline_ms(&self) -> Option<u64> {
        [self.debounce_due_ms, self.retry_due_ms, self.grace_due_ms]
            .into_iter()
            .flatten()
            .min()
    }

    /// Whether the unmeasured retry waits on the next animation frame.
    pub fn wants_animation_frame(&self) -> bool {
        self.retry_frame_armed
    }

    /// Whether a layout-gap grace is holding a withdraw back.
    pub fn grace_armed(&self) -> bool {
        self.grace_due_ms.is_some()
    }

    /// Publish once the resize burst settles.
    pub fn schedule(&mut self, now_ms: u64) {
        self.debounce_due_ms = Some(now_ms + VIEWPORT_DEBOUNCE_MS);
    }

    /// Drop a pending trailing publish.
    pub fn cancel_scheduled(&mut self) {
        self.debounce_due_ms = None;
    }

    /// Publish now, superseding a pending trailing publish.
    pub fn publish_now(&mut self, now_ms: u64, host: &mut dyn ViewportHost) -> bool {
        self.cancel_scheduled();
        self.publish(now_ms, host)
    }

    /// Claim the measured geometry, or park, or retry an unmeasured box.
    pub fn publish(&mut self, now_ms: u64, host: &mut dyn ViewportHost) -> bool {
        if !host.has_view() {
            self.cancel_retry();
            return false;
        }
        if !host.should_publish_active() {
            // An armed grace owns the transition until it ends: parking here
            // would turn the gap it absorbs into the instant leave it prevents.
            if self.grace_armed() {
                return false;
            }
            self.park(host);
            return false;
        }
        self.end_grace("absorbed");
        let Some((cols, rows)) = host.measure() else {
            // A lifecycle-active 0×0 box is transiently unmeasured: keep the
            // last positive lease rather than turn layout jitter into a leave.
            self.retry_unmeasured(now_ms, host);
            return false;
        };
        self.cancel_scheduled();
        self.cancel_retry();
        host.publish(cols, rows);
        tracing::debug!(target: "terminal", cols, rows, "terminal.view_publish");
        true
    }

    /// Stop claiming the view. Supersedes any armed grace.
    pub fn publish_inactive(&mut self, host: &mut dyn ViewportHost) {
        self.end_grace("superseded");
        host.withdraw();
        self.cancel_scheduled();
        self.cancel_retry();
    }

    /// Release paint holds, then stop claiming.
    pub fn park(&mut self, host: &mut dyn ViewportHost) {
        host.release_paint_holds();
        self.publish_inactive(host);
    }

    /// Withdraw after the layout-gap grace instead of now.
    pub fn park_after_layout_gap(&mut self, now_ms: u64) {
        if self.grace_armed() {
            return;
        }
        tracing::debug!(target: "terminal", grace_ms = LAYOUT_GAP_PARK_GRACE_MS,
            "terminal.view_park_grace start");
        self.grace_due_ms = Some(now_ms + LAYOUT_GAP_PARK_GRACE_MS);
    }

    /// A withdraw: graced when it is only a transient layout gap.
    pub fn withdraw(&mut self, now_ms: u64, transient_gap: bool, host: &mut dyn ViewportHost) {
        if transient_gap {
            self.park_after_layout_gap(now_ms);
        } else {
            self.park(host);
        }
    }

    /// The retry's animation frame came round.
    pub fn on_animation_frame(&mut self, now_ms: u64, host: &mut dyn ViewportHost) {
        if !self.retry_frame_armed {
            return;
        }
        self.retry_frame_armed = false;
        if !self.can_retry(host) {
            self.cancel_retry();
            return;
        }
        self.publish(now_ms, host);
    }

    /// Fire every timer due at `now_ms`.
    pub fn on_deadline(&mut self, now_ms: u64, host: &mut dyn ViewportHost) {
        if self.retry_due_ms.is_some_and(|due| due <= now_ms) {
            self.retry_due_ms = None;
            if !self.can_retry(host) {
                self.cancel_retry();
            } else if !self.publish(now_ms, host) {
                self.retry_phase = RetryPhase::Idle;
            }
        }
        if self.debounce_due_ms.is_some_and(|due| due <= now_ms) {
            self.debounce_due_ms = None;
            self.publish(now_ms, host);
        }
        if self.grace_due_ms.is_some_and(|due| due <= now_ms) {
            self.grace_due_ms = None;
            if host.should_publish_active() {
                self.publish(now_ms, host);
            } else {
                tracing::debug!(target: "terminal", "terminal.view_park_grace expired");
                self.park(host);
            }
        }
    }

    fn can_retry(&self, host: &dyn ViewportHost) -> bool {
        host.should_publish_active() && host.has_view()
    }

    /// Frame first, then one 100 ms timer; a failed timer ends the episode.
    fn retry_unmeasured(&mut self, now_ms: u64, host: &dyn ViewportHost) {
        if !self.can_retry(host) {
            self.cancel_retry();
            return;
        }
        match self.retry_phase {
            RetryPhase::Idle => {
                self.retry_phase = RetryPhase::Frame;
                self.retry_frame_armed = true;
            }
            RetryPhase::Frame if !self.retry_frame_armed && self.retry_due_ms.is_none() => {
                self.retry_phase = RetryPhase::Timer;
                self.retry_due_ms = Some(now_ms + UNMEASURED_VIEWPORT_RETRY_MS);
            }
            RetryPhase::Frame | RetryPhase::Timer => {}
        }
    }

    fn cancel_retry(&mut self) {
        self.retry_frame_armed = false;
        self.retry_due_ms = None;
        self.retry_phase = RetryPhase::Idle;
    }

    fn end_grace(&mut self, phase: &'static str) {
        if self.grace_due_ms.take().is_some() {
            tracing::debug!(target: "terminal", phase, "terminal.view_park_grace");
        }
    }
}
