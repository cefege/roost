//! Reader intent, the paint-hold mask, the follow band and the bottom-park
//! settle: every rule deciding whether a pane's DOM may be rewritten now, with
//! no DOM in sight. `CellGridRenderer` applies them (`cell_renderer/reader.rs`).
//! Ports the reader fields and transitions of `apps/web/src/renderer/cellRenderer.ts`
//! and the reader parts of `apps/web/src/renderer/cellRendererPresentation.ts`.

use crate::block_placeholder::DEFAULT_CELL_ROW_PX;

/// A native selection exists somewhere in the document, so painting is frozen.
pub const RENDERER_HOLD_SELECTION: u32 = 1;
/// A terminal link is being followed, so painting is frozen.
pub const RENDERER_HOLD_LINK: u32 = 2;

/// Rows of slack around the live tail. A reader inside the band is riding the
/// tail: sub-row jitter, a fractional clamp and a flick that lands a row short
/// must not freeze the pane, while one wheel notch (~100px) leaves it. The pin
/// predicate shares this band — a follower that is not re-pinned drifts out of
/// it one appended row later.
pub const BOTTOM_FOLLOW_SLACK_ROWS: u32 = 2;

/// Quiet time after the last scroll event before a rest inside the band
/// resumes. A browser animates a wheel gesture across frames and cancels that
/// animation when anything writes `scrollTop`, so a resume may only run once the
/// gesture has stopped emitting events.
pub const BOTTOM_FOLLOW_SETTLE_MS: u64 = 180;

/// Whether the renderer is painting at the live tail or frozen for a reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReaderIntent {
    /// Painting the live tail.
    #[default]
    Live,
    /// A reader parked somewhere; the painted DOM is immutable.
    Reading,
}

/// Why a reader parked. `selection` and `find` own an anchor; the other four
/// are a POSITION and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderIntentReason {
    /// A scroll event the renderer did not cause.
    NativeScroll,
    /// A wheel gesture, from the capture-phase forwarder.
    Wheel,
    /// A touch drag, from the capture-phase forwarder.
    Touch,
    /// The selection hold this renderer itself armed.
    Selection,
    /// A find match the pane scrolled to.
    Find,
    /// A shell prompt the reader jumped to (Mod+Shift+Up/Down). A position:
    /// it is released like a scroll, and only names who parked the reader so
    /// the next jump continues from the prompt this one landed on.
    PromptJump,
}

impl ReaderIntentReason {
    /// A park whose whole state is a scroll POSITION: re-pinning it to a new
    /// bottom loses nothing, so it is the one class a box resize or a hold
    /// release may end.
    pub fn is_position_only(self) -> bool {
        matches!(
            self,
            Self::NativeScroll | Self::Wheel | Self::Touch | Self::PromptJump
        )
    }
}

/// What one reader transition did, so the renderer knows which DOM work
/// follows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnterReadingOutcome {
    /// The reader was already parked for a different reason. The position is
    /// re-anchored and nothing else changes — a selection that starts while a
    /// find park owns the view does not take the anchor away from it.
    AnchorOnly,
    /// The reader is now parked for `reason`.
    Parked,
}

/// Whether a hold request changed the mask, and therefore whether a release
/// has to try to resume the reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldChange {
    /// The request asked for the state the mask was already in.
    Unchanged,
    /// A hold was armed. Painting stops here and stays stopped.
    Armed,
    /// A hold was released; the renderer may now resume.
    Released,
}

/// Whether a resume was admitted, and whether admitting it must pin the bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResumeAdmission {
    /// False means the reader state is untouched: a `find` park refuses an
    /// implicit resume, and a surviving hold outranks the resume outright.
    pub admitted: bool,
    /// True when the resume must re-pin rather than merely paint.
    pub pin_on_resume: bool,
}

/// The reader state machine: intent, its reason, the composed hold mask, and
/// the bottom-park settle armed last.
///
/// A non-zero hold mask is a TOTAL paint kill — frames are accepted and
/// swallowed — and `_resume_live` refuses a held pane BEFORE mutating reader
/// state, so a held pane keeps reporting its real intent and reason instead of a
/// `live`/`null` pair that hides the real block reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReaderState {
    intent: ReaderIntent,
    reason: Option<ReaderIntentReason>,
    hold_mask: u32,
    /// The epoch of the settle armed last and not yet run. Each arm replaces
    /// it, so only the latest arm can ever run, and it runs once.
    pending_settle: Option<u64>,
}

impl ReaderState {
    /// The intent a fresh renderer starts in: live, with no hold.
    pub const fn new() -> Self {
        Self {
            intent: ReaderIntent::Live,
            reason: None,
            hold_mask: 0,
            pending_settle: None,
        }
    }

    /// Whether the DOM is frozen: a reader is parked or a hold is armed.
    pub fn holding(&self) -> bool {
        self.hold_mask != 0
    }

    /// The raw hold mask, for the snapshot and the reconcile block reason.
    pub const fn hold_mask(&self) -> u32 {
        self.hold_mask
    }

    /// The current intent.
    pub const fn intent(&self) -> ReaderIntent {
        self.intent
    }

    /// Why the reader is parked, or `None` when it is live.
    pub const fn reason(&self) -> Option<ReaderIntentReason> {
        self.reason
    }

    /// Park the reader for `reason`, re-anchoring the current position.
    pub fn enter_reading(&mut self, reason: ReaderIntentReason) -> EnterReadingOutcome {
        if reason == ReaderIntentReason::Selection
            && self.intent == ReaderIntent::Reading
            && self
                .reason
                .is_some_and(|held| held != ReaderIntentReason::Selection)
        {
            return EnterReadingOutcome::AnchorOnly;
        }
        self.intent = ReaderIntent::Reading;
        self.reason = Some(reason);
        EnterReadingOutcome::Parked
    }

    /// End a find interval WITHOUT moving the view: the park keeps its position
    /// and becomes an ordinary scroll park any resume can release.
    pub fn end_find_reading(&mut self) {
        if self.reason == Some(ReaderIntentReason::Find) {
            self.reason = Some(ReaderIntentReason::NativeScroll);
        }
    }

    /// Arm or release the selection hold, which also parks the reader while it
    /// is armed.
    pub fn set_selection_hold(&mut self, active: bool) -> HoldChange {
        let held = self.hold_mask & RENDERER_HOLD_SELECTION != 0;
        if held == active {
            return HoldChange::Unchanged;
        }
        if active {
            self.hold_mask |= RENDERER_HOLD_SELECTION;
            self.enter_reading(ReaderIntentReason::Selection);
            return HoldChange::Armed;
        }
        self.hold_mask &= !RENDERER_HOLD_SELECTION;
        HoldChange::Released
    }

    /// Arm or release the link hold, which never parks the reader by itself.
    pub fn set_armed_hold(&mut self, active: bool) -> HoldChange {
        let held = self.hold_mask & RENDERER_HOLD_LINK != 0;
        if held == active {
            return HoldChange::Unchanged;
        }
        if active {
            self.hold_mask |= RENDERER_HOLD_LINK;
            return HoldChange::Armed;
        }
        self.hold_mask &= !RENDERER_HOLD_LINK;
        HoldChange::Released
    }

    /// Whether a hold release should try to resume the reader now.
    ///
    /// A release resumes the selection park the hold itself created, and any
    /// park once the box has no scroll range: there, no scroll event can exist
    /// and no anchor is reachable. A park with range keeps its interval —
    /// reaching the bottom, or the next frame's settle, resumes that one.
    pub fn should_flush_after_release(&self, no_scroll_range: bool, follows_bottom: bool) -> bool {
        if self.holding() {
            return false;
        }
        let band_follower = self
            .reason
            .is_some_and(ReaderIntentReason::is_position_only)
            && follows_bottom;
        // The conjunction below is v2's KEEP-the-park guard, which early-returns
        // NO_LIVE_INTERACTION_RESULT; anything it does not cover resumes. Answer
        // the question the name asks — should this release flush — so the guard
        // is negated rather than returned.
        !(self.intent == ReaderIntent::Reading
            && self.reason != Some(ReaderIntentReason::Selection)
            && !no_scroll_range
            && !band_follower)
    }

    /// Admit a resume, and say whether it must pin the bottom.
    ///
    /// `clear_holds` is set by a local keystroke, which ends every hold at once.
    /// An implicit resume never touches a `find` park — only an explicit one
    /// does — and a surviving hold outranks the resume.
    pub fn begin_resume(&mut self, clear_holds: bool, explicit: bool) -> ResumeAdmission {
        if !explicit && self.reason == Some(ReaderIntentReason::Find) {
            return ResumeAdmission {
                admitted: false,
                pin_on_resume: false,
            };
        }
        if clear_holds {
            self.hold_mask = 0;
        }
        if self.holding() {
            return ResumeAdmission {
                admitted: false,
                pin_on_resume: false,
            };
        }
        let pin_on_resume = explicit || self.reason == Some(ReaderIntentReason::Selection);
        self.intent = ReaderIntent::Live;
        self.reason = None;
        ResumeAdmission {
            admitted: true,
            pin_on_resume,
        }
    }

    /// Arm the bottom-park settle under `epoch`, superseding any armed before.
    pub fn arm_bottom_park_settle(&mut self, epoch: u64) {
        self.pending_settle = Some(epoch);
    }

    /// The epoch of the settle armed last and not yet run.
    pub const fn pending_bottom_park_settle(&self) -> Option<u64> {
        self.pending_settle
    }

    /// Consume the settle armed under `epoch`. False — and nothing consumed —
    /// when a later arm superseded it or it already ran.
    pub fn take_bottom_park_settle(&mut self, epoch: u64) -> bool {
        if self.pending_settle != Some(epoch) {
            return false;
        }
        self.pending_settle = None;
        true
    }

    /// The reconcile block reason, or `None` when the DOM is reconciled to the
    /// canonical frame.
    pub fn reconcile_block_reason(
        &self,
        reader_pending: bool,
        pending_render: bool,
        canonical: (Option<&str>, Option<u64>),
        reconciled: (Option<&str>, Option<u64>),
    ) -> ReconcileBlockReason {
        if reader_pending {
            return ReconcileBlockReason::ReaderPendingFrame;
        }
        let selection = self.hold_mask & RENDERER_HOLD_SELECTION != 0;
        let link = self.hold_mask & RENDERER_HOLD_LINK != 0;
        match (selection, link) {
            (true, true) => return ReconcileBlockReason::SelectionAndLinkHold,
            (true, false) => return ReconcileBlockReason::SelectionHold,
            (false, true) => return ReconcileBlockReason::LinkHold,
            (false, false) => {}
        }
        if pending_render {
            return ReconcileBlockReason::PendingRender;
        }
        if canonical != reconciled {
            return ReconcileBlockReason::NotReconciled;
        }
        ReconcileBlockReason::None
    }
}

/// Why the DOM is not yet reconciled to the canonical frame, read by the
/// diagnostic snapshot. `None` means it is reconciled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileBlockReason {
    /// A frame is held back for a parked reader.
    ReaderPendingFrame,
    /// A native selection exists.
    SelectionHold,
    /// A terminal link is being followed.
    LinkHold,
    /// Both holds at once.
    SelectionAndLinkHold,
    /// A frame was accepted but not yet painted.
    PendingRender,
    /// Nothing blocked it: the DOM is behind the canonical watermark.
    NotReconciled,
    /// The DOM is reconciled.
    None,
}

/// The three numbers a scroll box reports, all in the same pixel space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollBoxGeometry {
    pub scroll_top: f64,
    pub scroll_height: f64,
    pub client_height: f64,
}

/// Whether the box rests within the follow band of its bottom clamp. The
/// distance is zero exactly at the clamp and never negative past it.
///
/// This is the POLICY predicate. The exact clamp predicate stays exact: it is
/// what the resume and the rAF settle ask, and widening it is how a reader that
/// merely moved half a row stops streaming.
pub fn follows_scroll_bottom(box_geometry: ScrollBoxGeometry, row_height: f64) -> bool {
    let row = if row_height > 0.0 {
        row_height
    } else {
        DEFAULT_CELL_ROW_PX
    };
    let distance =
        (box_geometry.scroll_height - box_geometry.client_height - box_geometry.scroll_top)
            .max(0.0);
    distance <= f64::from(BOTTOM_FOLLOW_SLACK_ROWS) * row
}

/// Where the reader's own scroll position sits in absolute history rows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReaderAnchor {
    /// The history row the reader is looking at.
    pub row: u32,
    /// How far into that row the reader has scrolled, in pixels.
    pub offset_px: f64,
}

/// The anchor a scroll position implies, or `None` when the position is at or
/// past the end of painted layout — there is no row to come back to — or the
/// pitch is unmeasured, which the renderer never passes.
pub fn reader_anchor_at_scroll(
    scroll_top: f64,
    spacer_top: f64,
    row_height: f64,
    layout_end: u64,
) -> Option<ReaderAnchor> {
    if row_height <= 0.0 {
        return None;
    }
    let exact = (scroll_top - spacer_top) / row_height;
    if exact >= layout_end as f64 {
        return None;
    }
    let row = exact.floor().max(0.0) as u32;
    Some(ReaderAnchor {
        row,
        offset_px: (scroll_top - spacer_top - f64::from(row) * row_height).clamp(0.0, row_height),
    })
}
