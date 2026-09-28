//! The terminal render scheduler: which batch one renderer's next browser frame
//! paints, and what a landed or refused paint leaves behind. Ports
//! `apps/web/src/renderer/terminal-render-scheduler.ts`; `cursor_poll` ports
//! `apps/web/src/renderer/cursorPollTicker.ts`. Driven by the terminal stream
//! view's renderer subscriber; every clock value arrives as `now_ms` and the
//! browser frame is the caller's. Depends on `roost-protocol`'s cell frame.

mod cursor_poll;
mod frame_gate;
mod frames;

use roost_protocol::cell::CellGridFrame;

use crate::presentation::RendererEpochSeq;
use frame_gate::{
    GridIdentity, ReconciledGrid, admit_delta, full_conflicts_with_known_canonical,
    own_queued_delta,
};
use frames::PendingRender;

pub use cursor_poll::{
    CURSOR_POLL_INTERVAL_MS, CursorPollPane, CursorPollReading, CursorPollTicker, CursorPosition,
};
pub use frames::{ApplyMode, Delivery, PaintOutcome, PaintRequest};

/// What one arrival decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueDecision {
    /// Retained in the pending slot, riding the browser frame that is armed or
    /// about to be. `mode` is how the WHOLE batch will be applied, which is not
    /// always how this frame arrived: a delta that cannot continue the batch
    /// replaces it with a fallback full and answers `FallbackFull`.
    ///
    /// A second arrival inside one browser frame answers this again, and asks
    /// for no second arm. That is the whole coalescing rule.
    Coalesced {
        mode: ApplyMode,
        batch_frames: u32,
        appended_rows: u64,
    },
    /// Refused, and nothing was retained. The full names a sequence at or below
    /// the one already known, or a different grid at the same sequence: either
    /// way admitting it would move the canonical under a batch already queued.
    RefusedStaleFull,
    /// Refused, and nothing was retained: this scheduler is disposed.
    RefusedDisposed,
}

/// What one browser frame decided.
///
/// A renderer hold is NOT a reason to skip a paint: the held renderer folds the
/// batch into its canonical frame off-DOM and answers `Applied`, so deliveries
/// (terminal modes, cursor, activity) keep flowing while a selection is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameDecision<'a> {
    /// The held batch is due: paint it, then answer `complete_paint`.
    PaintNow(PaintRequest<'a>),
    /// The batch is retained and NOT painted: the pane is parked, so its
    /// canonical frame keeps folding off-DOM and the next activation paints the
    /// latest one.
    Parked,
    /// There is no batch to paint. A disposed scheduler reads here, because
    /// disposal empties the slot.
    Idle,
}

/// One renderer's pending batch, its reconcile watermark, and whether a browser
/// frame is armed.
///
/// Every mutation — `enqueue`, `set_foreground(true)`, `complete_paint` — is
/// followed by the caller asking `schedule_browser_frame`, which is the v2
/// `schedule()` those paths end in.
#[derive(Debug, Clone, Default)]
pub struct RenderScheduler {
    pending: Option<PendingRender>,
    /// A browser frame is armed and has not fired. Owned here so a second
    /// arrival inside that frame cannot arm a second one, which is what makes
    /// the arm idempotent from the caller's side.
    frame_armed: bool,
    /// When the browser frame that handed out the batch now being painted
    /// fired, or `None` when no paint is outstanding. An answer with no paint
    /// outstanding changes nothing, so a caller that answers a parked or idle
    /// frame cannot advance the watermark past a DOM that was never touched.
    painting_since_ms: Option<u64>,
    foreground: bool,
    disposed: bool,
    /// The grid the painted DOM is reconciled to, or `None` before the first
    /// paint lands.
    reconciled: Option<ReconciledGrid>,
}

impl RenderScheduler {
    /// A scheduler with no batch, no watermark, and no frame armed. A pane that
    /// has never painted starts here, and its first frame must be APPLIED:
    /// there is nothing for a delta to continue from.
    pub const fn new() -> Self {
        Self {
            pending: None,
            frame_armed: false,
            painting_since_ms: None,
            foreground: false,
            disposed: false,
            reconciled: None,
        }
    }

    /// Say whether the pane is in the foreground.
    ///
    /// Going to the background parks the batch: a queued delta run becomes the
    /// canonical full, the armed frame is dropped, and arrivals keep folding the
    /// canonical off-DOM with no DOM work. Coming back owes the frame that
    /// paints the latest state.
    pub fn set_foreground(&mut self, active: bool) {
        if self.disposed {
            return;
        }
        if self.foreground != active {
            tracing::debug!(target: "terminal", active, pending = ?self.pending_mode(),
                "render scheduler foreground changed");
        }
        self.foreground = active;
        if active {
            return;
        }
        if let Some(pending) = self.pending.take() {
            self.pending = Some(pending.downgrade_to_fallback_full());
        }
        self.frame_armed = false;
    }

    /// Retire the scheduler: no batch is retained and no frame is armed, so a
    /// callback that was already in flight finds nothing to paint.
    pub fn dispose(&mut self) {
        if self.disposed {
            return;
        }
        tracing::debug!(target: "terminal", pending = ?self.pending_mode(),
            "render scheduler disposed");
        self.disposed = true;
        self.pending = None;
        self.frame_armed = false;
        self.painting_since_ms = None;
    }

    /// Offer one arriving frame and the canonical frame it folded onto, and
    /// answer what became of it.
    pub fn enqueue(
        &mut self,
        frame: &CellGridFrame,
        canonical: CellGridFrame,
        now_ms: u64,
    ) -> EnqueueDecision {
        if self.disposed {
            return EnqueueDecision::RefusedDisposed;
        }
        if frame.full {
            if full_conflicts_with_known_canonical(&canonical, self.known_grid_identity()) {
                tracing::debug!(target: "terminal", seq = canonical.seq,
                    stream_id = %canonical.stream_id, "stale or conflicting full refused");
                return EnqueueDecision::RefusedStaleFull;
            }
            let appended_rows = self
                .pending
                .as_ref()
                .map_or(0, PendingRender::appended_rows);
            // A wire full RESTAMPS the queue clock even when a batch was already
            // waiting: it is a new baseline, not a continuation of the batch
            // that clock was measuring. Every other path into a full keeps it.
            return self.retain(PendingRender::full(
                canonical,
                frame.clone(),
                ApplyMode::WireFull,
                appended_rows,
                1,
                true,
                now_ms,
            ));
        }
        if let Some(extend) = admit_delta(
            self.foreground,
            self.pending.as_ref(),
            self.reconciled.as_ref(),
            frame,
        ) {
            let owned = own_queued_delta(frame);
            let batch = match self.pending.take() {
                None => PendingRender::delta(
                    canonical,
                    owned,
                    extend.appended_rows,
                    extend.span_count,
                    now_ms,
                ),
                // The queue clock carries over: a batch that grew is still one
                // batch, and the wait a caller reports is the wait for the
                // FIRST frame in it, not for the newest arrival.
                Some(PendingRender::Delta {
                    mut deltas,
                    queued_at_ms,
                    ..
                }) => {
                    deltas.push(owned);
                    PendingRender::Delta {
                        canonical,
                        deltas,
                        appended_rows: extend.appended_rows,
                        span_count: extend.span_count,
                        queued_at_ms,
                    }
                }
                // `admit_delta` answers `Some` only for an empty slot or a delta
                // batch; the repair is spelled out rather than a panic because a
                // queued batch is never worth losing to a rule that drifted.
                Some(previous) => {
                    PendingRender::fallback_full(Some(previous), frame, canonical, now_ms)
                }
            };
            return self.retain(batch);
        }
        let previous = self.pending.take();
        let batch = PendingRender::fallback_full(previous, frame, canonical, now_ms);
        tracing::debug!(target: "terminal", seq = frame.seq, base_seq = frame.base_seq,
            batch_frames = batch.batch_frames(), foreground = self.foreground,
            "delta cannot ride; batch repairs from the canonical full");
        self.retain(batch)
    }

    /// One browser frame came round at `now_ms`. Answer what is due.
    pub fn on_frame_fired(&mut self, now_ms: u64) -> FrameDecision<'_> {
        self.frame_armed = false;
        let foreground = self.foreground;
        let Some(pending) = self.pending.as_ref() else {
            return FrameDecision::Idle;
        };
        if !foreground {
            return FrameDecision::Parked;
        }
        self.painting_since_ms = Some(now_ms);
        FrameDecision::PaintNow(PaintRequest {
            mode: pending.apply_mode(),
            canonical: pending.canonical(),
            deltas: pending.deltas(),
            delivery: pending.delivery(),
            appended_rows: pending.appended_rows(),
            batch_frames: pending.batch_frames(),
            had_wire_full: pending.had_wire_full(),
            queue_delay_ms: now_ms.saturating_sub(pending.queued_at_ms()),
        })
    }

    /// Answer the paint that `on_frame_fired` handed out.
    ///
    /// A landed batch advances the reconcile watermark to its canonical frame,
    /// so the next delta extends what the DOM holds. A refused one is repaired
    /// as a fallback full and keeps its queue clock, so the next browser frame
    /// paints the canonical instead of retrying a batch that already failed.
    pub fn complete_paint(&mut self, outcome: PaintOutcome) {
        let Some(fired_at_ms) = self.painting_since_ms.take() else {
            tracing::warn!(target: "terminal", ?outcome,
                "render paint answered with no paint outstanding; ignored");
            return;
        };
        let Some(pending) = self.pending.take() else {
            return;
        };
        let queue_ms = fired_at_ms.saturating_sub(pending.queued_at_ms());
        match outcome {
            PaintOutcome::Applied => {
                tracing::debug!(target: "terminal", seq = pending.canonical().seq,
                    mode = ?pending.apply_mode(), batch_frames = pending.batch_frames(),
                    appended_rows = pending.appended_rows(), queue_ms, "cell.apply_dur");
                self.reconciled = Some(ReconciledGrid::of_canonical(pending.canonical()));
            }
            PaintOutcome::Refused => {
                tracing::info!(target: "terminal", seq = pending.canonical().seq,
                    mode = ?pending.apply_mode(), queue_ms,
                    "render paint refused; repairing from the canonical full");
                self.pending = Some(pending.downgrade_to_fallback_full());
            }
        }
    }

    /// Arm the browser frame that will paint the held batch, and answer whether
    /// the caller must actually request one.
    ///
    /// This is the ONLY way to arm, and it is idempotent: a batch already riding
    /// a frame answers `false`, which is what makes two arrivals inside one
    /// interval cost one paint. A parked or disposed scheduler never arms. A
    /// host with no animation frame calls `on_frame_fired` on the spot instead
    /// of requesting one.
    pub fn schedule_browser_frame(&mut self) -> bool {
        if !self.needs_browser_frame() || self.frame_armed {
            return false;
        }
        self.frame_armed = true;
        true
    }

    /// Whether a batch is owed a paint, armed or not.
    pub fn needs_browser_frame(&self) -> bool {
        !self.disposed && self.foreground && self.pending.is_some()
    }

    /// How the held batch would be applied, or `None` when nothing is held.
    pub fn pending_mode(&self) -> Option<ApplyMode> {
        self.pending.as_ref().map(PendingRender::apply_mode)
    }

    /// Whether a browser frame is armed and has not yet fired.
    pub fn is_frame_armed(&self) -> bool {
        self.frame_armed
    }

    /// Whether the pane is in the foreground.
    pub fn is_foreground(&self) -> bool {
        self.foreground
    }

    /// Whether this scheduler is retired.
    pub fn is_disposed(&self) -> bool {
        self.disposed
    }

    /// The watermark the painted DOM has reached, in the shape the
    /// presentation snapshot names it, or `None` before the first paint lands.
    pub fn reconciled_watermark(&self) -> Option<RendererEpochSeq> {
        self.reconciled.as_ref().map(|grid| RendererEpochSeq {
            grid_epoch: Some(grid.grid_epoch.clone()),
            seq: Some(grid.seq),
        })
    }

    /// Put a batch in the slot and report what it is.
    fn retain(&mut self, batch: PendingRender) -> EnqueueDecision {
        let decision = EnqueueDecision::Coalesced {
            mode: batch.apply_mode(),
            batch_frames: batch.batch_frames(),
            appended_rows: batch.appended_rows(),
        };
        self.pending = Some(batch);
        decision
    }

    /// The grid identity a stale full is measured against: the canonical the
    /// held batch would reconcile to, or the watermark the DOM is at.
    fn known_grid_identity(&self) -> Option<GridIdentity<'_>> {
        match self.pending.as_ref() {
            Some(pending) => Some(GridIdentity::of_frame(pending.canonical())),
            None => self.reconciled.as_ref().map(GridIdentity::of_reconciled),
        }
    }
}
