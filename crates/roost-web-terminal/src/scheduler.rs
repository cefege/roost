//! The terminal render scheduler: when one renderer's next frame is worth
//! painting, when several arriving frames are ONE paint, and when a paint is
//! held instead.
//!
//! A pure state machine: every clock value arrives as `now_ms` and the browser
//! frame that carries a batch belongs to the caller. Composes
//! `scheduler::frame_gate` (admission), `scheduler::frames` (the batch) and
//! `scheduler::cursor_poll` (the shared cursor tick); depends on
//! `roost-protocol` for the cell frame and on `reader_intent` for the hold mask
//! and the block reasons.

mod cursor_poll;
mod frame_gate;
mod frames;

use roost_protocol::cell::CellGridFrame;

use crate::presentation::RendererEpochSeq;
use crate::reader_intent::ReconcileBlockReason;
use frame_gate::{
    GridIdentity, MAX_PENDING_DELTA_FRAMES, MAX_PENDING_DELTA_SPANS, MAX_PENDING_SCROLLBACK_ROWS,
    ReconciledGrid, count_incoming_spans, delta_follows, full_conflicts_with_known_canonical,
    hold_block_reason, own_queued_delta,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameDecision<'a> {
    /// The held batch is due: paint it, then answer `complete_paint`.
    PaintNow(PaintRequest<'a>),
    /// The batch is retained and NOT painted: the pane is parked, so its
    /// canonical frame keeps folding off-DOM and the next activation paints the
    /// latest one.
    Parked,
    /// The batch is retained and NOT painted: the renderer is holding. The
    /// reason is the same name the reconcile snapshot reports for a held pane,
    /// read off the mask the caller passed in.
    Held { reason: ReconcileBlockReason },
    /// There is no batch to paint. A disposed scheduler reads here, because
    /// disposal empties the slot.
    Idle,
}

/// The counters a new delta would give a batch it may extend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DeltaExtend {
    appended_rows: u64,
    span_count: usize,
}

/// One renderer's pending batch, its reconcile watermark, and whether a browser
/// frame is armed.
#[derive(Debug, Clone, Default)]
pub struct RenderScheduler {
    pending: Option<PendingRender>,
    /// A browser frame is armed and has not fired. Owned here so a second
    /// arrival inside that frame cannot arm a second one, which is what makes
    /// the arm idempotent from the caller's side.
    frame_armed: bool,
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
            foreground: false,
            disposed: false,
            reconciled: None,
        }
    }

    /// Say whether the pane is in the foreground.
    ///
    /// Going to the background parks the batch: a queued delta run becomes the
    /// canonical full, the armed frame is dropped, and arrivals keep folding the
    /// canonical off-DOM with no DOM work. Coming back arms the frame that
    /// paints the latest state.
    pub fn set_foreground(&mut self, active: bool) {
        if self.disposed {
            return;
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
        self.disposed = true;
        self.pending = None;
        self.frame_armed = false;
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
        if let Some(extend) = self.admit_delta(frame) {
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
                // The admission rules answer `Some` only for a slot holding a
                // delta batch, so this arm is out of reach. It is spelled as the
                // repair rather than a panic because a queued batch is never
                // worth losing to a rule that drifted.
                Some(previous) => {
                    PendingRender::fallback_full(Some(previous), frame, canonical, now_ms)
                }
            };
            return self.retain(batch);
        }
        let previous = self.pending.take();
        self.retain(PendingRender::fallback_full(
            previous, frame, canonical, now_ms,
        ))
    }

    /// One browser frame came round at `now_ms`. Answer what is due.
    ///
    /// `hold_mask` is the renderer's, read from `ReaderState::hold_mask()`: the
    /// scheduler does not own the reader, and a hold it could not see would
    /// hand a batch to a DOM it must not touch.
    pub fn on_frame_fired(&mut self, now_ms: u64, hold_mask: u32) -> FrameDecision<'_> {
        self.frame_armed = false;
        let Some(pending) = self.pending.as_ref() else {
            return FrameDecision::Idle;
        };
        if !self.foreground {
            return FrameDecision::Parked;
        }
        if let Some(reason) = hold_block_reason(hold_mask) {
            return FrameDecision::Held { reason };
        }
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
        match outcome {
            PaintOutcome::Applied => {
                if let Some(pending) = self.pending.take() {
                    self.reconciled = Some(ReconciledGrid::of_canonical(pending.canonical()));
                }
            }
            PaintOutcome::Refused => {
                if let Some(pending) = self.pending.take() {
                    self.pending = Some(pending.downgrade_to_fallback_full());
                }
            }
        }
    }

    /// Arm the browser frame that will paint the held batch, and answer whether
    /// the caller must actually request one.
    ///
    /// This is the ONLY way to arm, and it is idempotent: a batch already riding
    /// a frame answers `false`, which is what makes two arrivals inside one
    /// interval cost one paint. A parked or disposed scheduler never arms. A
    /// host with no animation frame at all answers `false` forever, and calls
    /// `on_frame_fired` on the spot instead.
    pub fn schedule_browser_frame(&mut self) -> bool {
        if !self.needs_browser_frame() || self.frame_armed {
            return false;
        }
        self.frame_armed = true;
        true
    }

    /// Whether a batch is owed a paint, armed or not. A hold release and a
    /// foreground regain both ask this; neither may arm a frame on its own.
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

    /// The new batch counters when `frame` may extend the held batch, or `None`
    /// when it must be replaced by a full.
    ///
    /// Every rule here is about the BATCH, not the frame: a delta is admitted
    /// because the run it would join is still contiguous and still inside every
    /// bound, not because the delta is individually well formed.
    fn admit_delta(&self, frame: &CellGridFrame) -> Option<DeltaExtend> {
        if !self.foreground || frame.full || frame.seq != frame.base_seq.saturating_add(1) {
            return None;
        }
        let appended = u64::try_from(frame.scrollback_append.len()).unwrap_or(u64::MAX);
        let appended_rows = self
            .pending
            .as_ref()
            .map_or(0, PendingRender::appended_rows)
            .saturating_add(appended);
        if appended_rows > MAX_PENDING_SCROLLBACK_ROWS {
            return None;
        }
        let (previous, queued_frames, prior_spans) = match self.pending.as_ref() {
            Some(PendingRender::Delta {
                deltas, span_count, ..
            }) => (
                GridIdentity::of_frame(deltas.last()?),
                deltas.len(),
                *span_count,
            ),
            // A delta may never join a full: the full already IS the canonical
            // the run would have to fold onto.
            Some(PendingRender::Full { .. }) => return None,
            // With no baseline, nothing shows the delta is a continuation.
            None => (GridIdentity::of_reconciled(self.reconciled.as_ref()?), 0, 0),
        };
        if !delta_follows(previous, frame) || queued_frames + 1 > MAX_PENDING_DELTA_FRAMES {
            return None;
        }
        let remaining_spans = MAX_PENDING_DELTA_SPANS.saturating_sub(prior_spans);
        let incoming_spans = count_incoming_spans(frame, remaining_spans);
        if incoming_spans > remaining_spans {
            return None;
        }
        Some(DeltaExtend {
            appended_rows,
            span_count: prior_spans.saturating_add(incoming_spans),
        })
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
