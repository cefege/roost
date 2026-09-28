//! Reader intent as the renderer applies it: parking, holding, resuming, the
//! anchor a park owns, and the bottom-park settle the pane runs. The pure rules
//! live in `reader_intent`; this is the DOM half. Ports `enterReading` through
//! `_resumeLive`, `_settleBottomPark` and `settleFollowBand` of
//! `apps/web/src/renderer/cellRenderer.ts`.

use crate::cell_renderer::CellGridRenderer;
use crate::presentation::{LiveInteractionResult, NO_LIVE_INTERACTION_RESULT};
use crate::reader_intent::{
    HoldChange, ReaderAnchor, ReaderIntent, ReaderIntentReason, reader_anchor_at_scroll,
};
use crate::render_element::RenderElement;

impl<E: RenderElement> CellGridRenderer<E> {
    /// The reader's intent: painting at the live tail, or frozen for a reader.
    pub fn reader_intent(&self) -> ReaderIntent {
        self.reader.intent()
    }

    /// Why the reader is parked, or `None` while it is live.
    pub fn reader_reason(&self) -> Option<ReaderIntentReason> {
        self.reader.reason()
    }

    /// The composed hold mask: the selection hold and the link hold, each a
    /// LEVEL the pane re-derives from the live document rather than latching on
    /// an edge that may never arrive to clear it.
    pub fn hold_mask(&self) -> u32 {
        self.reader.hold_mask()
    }

    /// Park the reader for `reason`, re-anchoring the position it is at.
    pub fn enter_reading(&mut self, reason: ReaderIntentReason) {
        self.live_selection_release_pending = false;
        self.reader.enter_reading(reason);
        self.capture_reader_anchor();
    }

    /// End a find interval WITHOUT moving the view: the park keeps its position
    /// and becomes an ordinary scroll park any resume can release.
    ///
    /// Dismissing the find bar must not resume — that would yank a reader off
    /// the match they are still looking at.
    pub fn end_find_reading(&mut self) {
        self.reader.end_find_reading();
    }

    /// Arm or release the selection hold. Arming it parks the reader, because a
    /// repaint under a live selection is what tears the selection apart.
    pub fn set_selection_hold(&mut self, active: bool) -> LiveInteractionResult {
        match self.reader.set_selection_hold(active) {
            HoldChange::Unchanged => NO_LIVE_INTERACTION_RESULT,
            HoldChange::Armed => {
                // v2 arms through `enterReading("selection")`, which also ends
                // a pending selection-release bracket.
                self.live_selection_release_pending = false;
                self.capture_reader_anchor();
                NO_LIVE_INTERACTION_RESULT
            }
            HoldChange::Released => self.flush_if_released(),
        }
    }

    /// Arm or release the link hold, which never parks the reader by itself: a
    /// link hold freezes paint but leaves the position alone.
    pub fn set_armed_hold(&mut self, active: bool) -> LiveInteractionResult {
        match self.reader.set_armed_hold(active) {
            HoldChange::Released => self.flush_if_released(),
            HoldChange::Unchanged | HoldChange::Armed => NO_LIVE_INTERACTION_RESULT,
        }
    }

    /// Try to resume after a hold dropped.
    ///
    /// A release resumes the selection park the hold itself created, any park
    /// once the box has no scroll range — there, no scroll event can exist and
    /// no anchor is reachable — and a position-only park riding the tail,
    /// because the hold swallowed the scroll event that proved the return. A
    /// park with range left keeps its interval: reaching the bottom, or the
    /// next frame's settle, resumes that one.
    fn flush_if_released(&mut self) -> LiveInteractionResult {
        if self.reader.holding() {
            return NO_LIVE_INTERACTION_RESULT;
        }
        let no_range = self.scroll_height() <= self.client_height();
        // Short-circuit like v2: the band is measured only for a position-only
        // park, because measuring it can probe the row pitch.
        let band_follower = self
            .reader
            .reason()
            .is_some_and(ReaderIntentReason::is_position_only)
            && self.follows_bottom();
        if !self
            .reader
            .should_flush_after_release(no_range, band_follower)
        {
            return NO_LIVE_INTERACTION_RESULT;
        }
        self.resume_live(false, no_range)
    }

    /// Clear every hold, adopt the newest frame and re-pin, as ONE transition.
    ///
    /// This is the one admitted local keystroke's path. Passive output and
    /// composer drafting never come through here, so a reader a user is
    /// actually reading is never cancelled by output.
    pub fn prepare_live_interaction(&mut self) -> LiveInteractionResult {
        self.live_selection_release_pending = false;
        self.resume_live(true, true)
    }

    /// Note that a selection is about to be released, so the scroll it may
    /// produce is consumed before it is read as a reader gesture.
    pub fn begin_live_selection_release(&mut self) {
        self.live_selection_release_pending = self.reader.intent() == ReaderIntent::Live;
    }

    /// Note that the selection release finished without a scroll following.
    pub fn finish_live_selection_release(&mut self) {
        self.live_selection_release_pending = false;
    }

    /// Resume live, if the reader state admits it.
    ///
    /// A `find` park refuses an IMPLICIT resume — only an explicit one moves
    /// it, which is what stops a find navigation's own write from unparking
    /// the row the reader just asked to be shown. A surviving hold outranks the
    /// resume outright, and the reader state is left TRUTHFUL under it:
    /// `live`/null with a set mask would hide the real block reason and un-mute
    /// a stall watchdog into a redial the mask immediately refreezes.
    pub(crate) fn resume_live(
        &mut self,
        clear_holds: bool,
        explicit: bool,
    ) -> LiveInteractionResult {
        let admission = self.reader.begin_resume(clear_holds, explicit);
        if !admission.admitted {
            return NO_LIVE_INTERACTION_RESULT;
        }
        let before = self.backfill_anchor();
        self.reader_anchor = None;
        if let Some(pending) = self.reader_pending_frame.take() {
            self.frame = Some(pending);
            self.pending_render = true;
            self.reader_pending_frame_retains_history = true;
        }
        let needs_reconcile = self.frame.as_ref().is_some_and(|frame| {
            self.pending_render
                || self.reconciled_grid_epoch.as_deref() != Some(frame.grid_epoch.as_str())
                || self.reconciled_seq != Some(frame.seq)
        });
        let mut reconciled = false;
        if needs_reconcile {
            self.pending_render = false;
            reconciled = self
                .reconcile_canonical(true, admission.pin_on_resume)
                .is_ok();
        } else {
            self.pin_to_bottom(true);
        }
        let after = self.backfill_anchor();
        let anchor_changed = before != after;
        if !reconciled && !anchor_changed {
            return NO_LIVE_INTERACTION_RESULT;
        }
        LiveInteractionResult {
            reconciled,
            anchor_changed,
        }
    }

    /// Ask the pane to open the scroll-idle window a band rest needs, and arm
    /// this renderer's own epoch so a later callback can tell whether it is
    /// still the arm that is current.
    ///
    /// The renderer only ASKS for the idle window: writing `scrollTop`
    /// mid-gesture cancels the scroll animation the reader is still
    /// performing. Frames are the one event a stalled pane always has, so a
    /// park created AFTER its gesture's last scroll event recruits the settle
    /// from here and liveness stops depending on a scroll that may never come.
    pub fn settle_bottom_park(&mut self) -> u64 {
        if self.follows_bottom()
            && !self.at_bottom()
            && let Some(request) = self.request_follow_band_settle.as_ref()
        {
            request();
        }
        self.bottom_park_settle_epoch = self.bottom_park_settle_epoch.wrapping_add(1);
        self.reader
            .arm_bottom_park_settle(self.bottom_park_settle_epoch);
        self.bottom_park_settle_epoch
    }

    /// The bottom-park settle armed last and not yet run — v2's
    /// `scheduleReaderSettle` callback, which the renderer cannot schedule.
    ///
    /// Pane contract (TERM mount slice, `crates/roost-web/src/components/terminal/`,
    /// lead `WebLeadU`): after every `apply*` and `handle_scroll`, read this and,
    /// when it is `Some(epoch)`, call `resume_bottom_park(epoch)` from the next
    /// `requestAnimationFrame` (a settle must run after layout). v2 discards
    /// that call's result.
    pub fn pending_bottom_park_settle(&self) -> Option<u64> {
        self.reader.pending_bottom_park_settle()
    }

    /// Run the settle armed under `epoch`: resume a position-only park the
    /// layout clamped onto the exact bottom.
    ///
    /// Each arm runs at most once, and only the latest: an epoch a later arm
    /// or `dispose` superseded is inert, so a park that has since moved on is
    /// never unparked by an old callback.
    pub fn resume_bottom_park(&mut self, epoch: u64) -> LiveInteractionResult {
        if !self.reader.take_bottom_park_settle(epoch)
            || self.reader.holding()
            || !self.at_bottom()
            || self.reader.intent() != ReaderIntent::Reading
            || !self
                .reader
                .reason()
                .is_some_and(ReaderIntentReason::is_position_only)
        {
            return NO_LIVE_INTERACTION_RESULT;
        }
        self.resume_live(false, false)
    }

    /// Resume a position-only park resting inside the follow band, armed by the
    /// pane's own scroll-idle window.
    pub fn settle_follow_band(&mut self) -> LiveInteractionResult {
        if self.reader.intent() != ReaderIntent::Reading || self.reader.holding() {
            return NO_LIVE_INTERACTION_RESULT;
        }
        if !self
            .reader
            .reason()
            .is_some_and(ReaderIntentReason::is_position_only)
        {
            return NO_LIVE_INTERACTION_RESULT;
        }
        if !self.follows_bottom() {
            return NO_LIVE_INTERACTION_RESULT;
        }
        self.resume_live(false, false)
    }

    /// Re-anchor the reader's position against the layout as it stands.
    pub(crate) fn capture_reader_anchor(&mut self) {
        if self.reader.intent() != ReaderIntent::Reading || self.scrollback_layout_end == 0 {
            self.reader_anchor = None;
            return;
        }
        let row_height = self.row_height();
        if row_height <= 0.0 {
            return;
        }
        self.reader_anchor = reader_anchor_at_scroll(
            self.scroll_top(),
            self.spacer.offset_top(),
            row_height,
            self.scrollback_layout_end,
        );
    }

    /// The reader's anchor: where its own scroll position sits in absolute rows.
    pub fn reader_anchor(&self) -> Option<ReaderAnchor> {
        self.reader_anchor
    }
}
