//! `TerminalPresentationController`: the per-pane presentation state machine.
//! It owns the bounded receiving window, the foreground DOM stall behind a
//! `catching_up` pane and the detached-view grace; the pane feeds it explicit
//! inputs with `now_ms` and re-polls it at `next_deadline_ms`. Ports the
//! controller in `apps/web/src/renderer/terminalPresentation.ts`, whose timers
//! become the one pending deadline held here.

use roost_protocol::cell::CellGridFrame;

use super::state::{
    DETACHED_GRACE_MS, FRAME_ACTIVITY_WINDOW_MS, TerminalPresentationActivity,
    TerminalPresentationInput, TerminalPresentationState, TerminalViewHandleStatus,
    derive_terminal_presentation_state,
};
use super::{FOREGROUND_DOM_STALL_MS, PresentationRendererView, preserves_foreground_reader_hold};
use crate::presentation::RendererEpochSeq;

/// The pane facts presentation reads at every decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationPane {
    /// The pane is the one the operator is looking at.
    pub active: bool,
    /// The pane holds keyboard focus.
    pub focused: bool,
    /// The document is visible.
    pub page_visible: bool,
}

/// One decision's inputs. `renderer` is `None` before the pane mounted one.
#[derive(Debug)]
pub struct PresentationInputs<'a, R> {
    pub now_ms: u64,
    pub pane: PresentationPane,
    pub status: Option<TerminalViewHandleStatus>,
    pub renderer: Option<&'a R>,
}

/// The parts of an applied frame activity reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationFrameMark<'a> {
    pub full: bool,
    pub grid_epoch: &'a str,
    pub seq: u64,
}

impl<'a> From<&'a CellGridFrame> for PresentationFrameMark<'a> {
    fn from(frame: &'a CellGridFrame) -> Self {
        Self {
            full: frame.full,
            grid_epoch: &frame.grid_epoch,
            seq: frame.seq,
        }
    }
}

/// The DOM stayed behind canonical at this watermark for the whole foreground
/// stall window; the pane's DOM repair owns what happens next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatchUpStalled(pub RendererEpochSeq);

/// At most one deadline is ever pending: every path that arms one clears the
/// other first.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingDeadline {
    /// Re-decide when the receiving window or the detached grace ends.
    ActivityExpiry { due_ms: u64 },
    /// Report a stall if the DOM is still behind `watermark` at `due_ms`.
    CatchUpStall {
        due_ms: u64,
        watermark: RendererEpochSeq,
    },
}

/// Watermark identity for "already reported": an absent epoch and an empty
/// one are the same stall.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WatermarkKey {
    grid_epoch: String,
    seq: Option<u64>,
}

impl WatermarkKey {
    fn of(watermark: &RendererEpochSeq) -> Self {
        Self {
            grid_epoch: watermark.grid_epoch.clone().unwrap_or_default(),
            seq: watermark.seq,
        }
    }
}

/// A pane with no accepted, baseline-ready view has no watermarks to report.
const UNREADY_WATERMARK: RendererEpochSeq = RendererEpochSeq {
    grid_epoch: None,
    seq: None,
};

/// Per-pane presentation policy. Owned by the pane that owns the renderer.
#[derive(Debug, Default)]
pub struct TerminalPresentationController {
    state: TerminalPresentationState,
    deadline: Option<PendingDeadline>,
    activity: Option<TerminalPresentationActivity>,
    not_ready_since_ms: Option<u64>,
    notified_catch_up: Option<WatermarkKey>,
}

impl TerminalPresentationController {
    pub fn new() -> Self {
        Self::default()
    }

    /// What the pane's indicator shows.
    pub fn state(&self) -> TerminalPresentationState {
        self.state
    }

    /// When the host must call `fire_due_deadline` next, if ever.
    pub fn next_deadline_ms(&self) -> Option<u64> {
        match self.deadline {
            Some(PendingDeadline::ActivityExpiry { due_ms })
            | Some(PendingDeadline::CatchUpStall { due_ms, .. }) => Some(due_ms),
            None => None,
        }
    }

    /// Forget the receiving window, the grace start and any armed stall.
    pub fn clear_frame_activity(&mut self) {
        self.deadline = None;
        self.activity = None;
        self.not_ready_since_ms = None;
        self.clear_catch_up_stall(true);
    }

    /// Re-decide the indicator. The pane calls this whenever `active`,
    /// `page_visible` or the view status changes, and on page show.
    pub fn refresh_terminal_presentation<R: PresentationRendererView>(
        &mut self,
        inputs: &PresentationInputs<'_, R>,
    ) -> TerminalPresentationState {
        let active = inputs.pane.active && inputs.pane.page_visible;
        let accepted_with_baseline = inputs
            .status
            .is_some_and(TerminalViewHandleStatus::accepted_with_baseline);
        let ready_renderer = inputs.renderer.filter(|_| active && accepted_with_baseline);
        let Some(renderer) = ready_renderer else {
            self.present_unready_view(
                inputs.now_ms,
                active && inputs.renderer.is_some(),
                active,
                accepted_with_baseline,
            );
            return self.state;
        };
        // The view is ready; a later loss measures its own grace from scratch.
        self.not_ready_since_ms = None;
        let canonical = renderer.canonical_epoch_seq();
        let reconciled = renderer.reconciled_epoch_seq();
        let next = derive_terminal_presentation_state(TerminalPresentationInput {
            active,
            accepted_with_baseline,
            canonical: &canonical,
            reconciled: &reconciled,
            activity: self.activity.as_ref(),
            now_ms: inputs.now_ms,
            not_ready_since_ms: self.not_ready_since_ms,
        });
        if next == TerminalPresentationState::CatchingUp {
            self.clear_activity_expiry();
            self.set_state(next);
            self.arm_catch_up_stall(inputs.now_ms, &canonical);
            return self.state;
        }
        self.clear_catch_up_stall(true);
        if next == TerminalPresentationState::Receiving
            && let Some(activity) = &self.activity
        {
            let window_ends_ms = activity
                .started_at_ms
                .saturating_add(FRAME_ACTIVITY_WINDOW_MS);
            self.set_state(next);
            self.deadline = Some(PendingDeadline::ActivityExpiry {
                due_ms: window_ends_ms.max(inputs.now_ms),
            });
            return self.state;
        }
        self.clear_activity_expiry();
        self.set_state(TerminalPresentationState::Idle);
        self.state
    }

    /// Record one applied frame. Only a delta is activity: a full is a repair
    /// or an attach, not output.
    pub fn note_frame_activity<R: PresentationRendererView>(
        &mut self,
        frame: PresentationFrameMark<'_>,
        inputs: &PresentationInputs<'_, R>,
    ) -> TerminalPresentationState {
        if !frame.full {
            self.activity = Some(TerminalPresentationActivity {
                grid_epoch: frame.grid_epoch.to_owned(),
                seq: frame.seq,
                started_at_ms: inputs.now_ms,
            });
        }
        self.refresh_terminal_presentation(inputs)
    }

    /// Run the pending deadline if `now_ms` reached it. A stall the DOM is
    /// still behind, with no reader hold deferring it, is returned for the
    /// pane's DOM repair; every other outcome re-decides the indicator.
    #[must_use]
    pub fn fire_due_deadline<R: PresentationRendererView>(
        &mut self,
        inputs: &PresentationInputs<'_, R>,
    ) -> Option<CatchUpStalled> {
        let due = self
            .next_deadline_ms()
            .is_some_and(|due_ms| inputs.now_ms >= due_ms);
        if !due {
            return None;
        }
        match self.deadline.take() {
            Some(PendingDeadline::CatchUpStall { watermark, .. }) => {
                self.fire_catch_up_stall(watermark, inputs)
            }
            Some(PendingDeadline::ActivityExpiry { .. }) | None => {
                self.refresh_terminal_presentation(inputs);
                None
            }
        }
    }

    /// Stop the cursor blinking, e.g. while the page is hidden.
    pub fn clear_cursor_blink<R: PresentationRendererView>(&self, renderer: Option<&mut R>) {
        if let Some(renderer) = renderer {
            renderer.set_cursor_blink_enabled(false);
        }
    }

    /// Blink only the focused pane the operator is looking at on a visible
    /// page. The pane calls this whenever `active`, `focused` or
    /// `page_visible` changes.
    pub fn refresh_cursor_blink<R: PresentationRendererView>(
        &self,
        pane: PresentationPane,
        renderer: Option<&mut R>,
    ) {
        if let Some(renderer) = renderer {
            renderer.set_cursor_blink_enabled(pane.active && pane.focused && pane.page_visible);
        }
    }

    /// The pane unmounted: nothing may fire and the cursor stops.
    pub fn dispose<R: PresentationRendererView>(&mut self, renderer: Option<&mut R>) {
        self.clear_frame_activity();
        self.clear_cursor_blink(renderer);
    }

    fn set_state(&mut self, next: TerminalPresentationState) {
        if self.state != next {
            tracing::debug!(target: "terminal", from = self.state.as_str(), to = next.as_str(),
                "terminal presentation changed");
            self.state = next;
        }
    }

    fn clear_activity_expiry(&mut self) {
        if matches!(self.deadline, Some(PendingDeadline::ActivityExpiry { .. })) {
            self.deadline = None;
        }
    }

    fn clear_catch_up_stall(&mut self, reset_notification: bool) {
        if matches!(self.deadline, Some(PendingDeadline::CatchUpStall { .. })) {
            self.deadline = None;
        }
        if reset_notification {
            self.notified_catch_up = None;
        }
    }

    /// The stall is measured from the OLDEST unreconciled watermark: a newer
    /// frame on the same epoch keeps the armed deadline instead of resetting it,
    /// so a busy PTY cannot defer its own repair forever.
    fn arm_catch_up_stall(&mut self, now_ms: u64, watermark: &RendererEpochSeq) {
        if self.notified_catch_up.as_ref() == Some(&WatermarkKey::of(watermark)) {
            return;
        }
        if let Some(PendingDeadline::CatchUpStall {
            watermark: armed, ..
        }) = &self.deadline
        {
            let same_epoch = armed.grid_epoch == watermark.grid_epoch;
            let still_ahead = matches!((armed.seq, watermark.seq), (Some(armed_seq), Some(seq)) if seq >= armed_seq);
            if same_epoch && still_ahead {
                return;
            }
            self.clear_catch_up_stall(false);
        }
        tracing::debug!(target: "terminal", grid_epoch = ?watermark.grid_epoch, seq = ?watermark.seq,
            "foreground DOM stall armed");
        self.deadline = Some(PendingDeadline::CatchUpStall {
            due_ms: now_ms.saturating_add(FOREGROUND_DOM_STALL_MS),
            watermark: watermark.clone(),
        });
    }

    fn fire_catch_up_stall<R: PresentationRendererView>(
        &mut self,
        captured: RendererEpochSeq,
        inputs: &PresentationInputs<'_, R>,
    ) -> Option<CatchUpStalled> {
        let Some(renderer) = inputs.renderer else {
            self.refresh_terminal_presentation(inputs);
            return None;
        };
        let current = renderer.canonical_epoch_seq();
        let reconciled = renderer.reconciled_epoch_seq();
        let still_active = inputs.pane.active
            && inputs.pane.page_visible
            && inputs
                .status
                .is_some_and(TerminalViewHandleStatus::accepted_with_baseline);
        let still_owns_watermark = current.grid_epoch == captured.grid_epoch
            && matches!((current.seq, captured.seq), (Some(seq), Some(target)) if seq >= target);
        let remains_unreconciled = reconciled.grid_epoch != captured.grid_epoch
            || match (reconciled.seq, captured.seq) {
                (Some(seq), Some(target)) => seq < target,
                _ => true,
            };
        if !still_active || !still_owns_watermark || !remains_unreconciled {
            self.refresh_terminal_presentation(inputs);
            return None;
        }
        let reader_reason = renderer.reader_reason();
        if preserves_foreground_reader_hold(reader_reason) {
            tracing::debug!(target: "terminal", ?reader_reason, seq = ?captured.seq,
                "foreground DOM stall deferred to a reader hold");
            self.refresh_terminal_presentation(inputs);
            return None;
        }
        tracing::info!(target: "terminal", grid_epoch = ?captured.grid_epoch, seq = ?captured.seq,
            "foreground DOM stall reported");
        self.notified_catch_up = Some(WatermarkKey::of(&captured));
        Some(CatchUpStalled(captured))
    }

    /// No view to paint. A detached pane delivers no frame and no status
    /// change, so the grace deadline is the only thing that can carry an
    /// actively-viewed pane from `idle` to `detached`.
    fn present_unready_view(
        &mut self,
        now_ms: u64,
        actively_viewed: bool,
        active: bool,
        accepted_with_baseline: bool,
    ) {
        let grace_started_ms = actively_viewed.then(|| self.not_ready_since_ms.unwrap_or(now_ms));
        if grace_started_ms.is_some() && self.not_ready_since_ms.is_none() {
            tracing::debug!(target: "terminal", now_ms, "actively viewed pane lost its view");
        }
        self.clear_frame_activity();
        self.not_ready_since_ms = grace_started_ms;
        self.set_state(derive_terminal_presentation_state(
            TerminalPresentationInput {
                active,
                accepted_with_baseline,
                canonical: &UNREADY_WATERMARK,
                reconciled: &UNREADY_WATERMARK,
                activity: None,
                now_ms,
                not_ready_since_ms: grace_started_ms,
            },
        ));
        let Some(started_ms) = grace_started_ms else {
            return;
        };
        let grace_ends_ms = started_ms.saturating_add(DETACHED_GRACE_MS);
        if grace_ends_ms > now_ms {
            self.deadline = Some(PendingDeadline::ActivityExpiry {
                due_ms: grace_ends_ms,
            });
        }
    }
}
