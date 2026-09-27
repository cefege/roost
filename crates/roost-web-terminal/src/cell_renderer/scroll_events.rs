//! The ONE writer of `scrollTop`, and the events that decide whether a reader
//! may be unparked.
//!
//! Two rules are load-bearing. A pane mutates its scroll space constantly —
//! appending history, filling a gap, evicting a block — and only a capture
//! taken BEFORE that mutation may authorise a position write, so a non-bottom
//! mutation never moves the reader. And the capture is the FOLLOW BAND, not a
//! widened clamp: the exact clamp predicate stays exact, because it is what the
//! resume and the band settle ask, and slack there is how a reader that moved
//! half a row stops streaming.

use crate::cell_renderer::CellGridRenderer;
use crate::cell_renderer_dom::effective_row_height;
use crate::element_style::{scroll_top_of, set_scroll_top_of};
use crate::presentation::{LiveInteractionResult, NO_LIVE_INTERACTION_RESULT};
use crate::reader_intent::{
    ReaderIntent, ReaderIntentReason, ScrollBoxGeometry, follows_scroll_bottom,
};
use roost_client_core::terminal::history::HistoryRange;

impl CellGridRenderer {
    /// Whether the box rests exactly on its bottom clamp.
    pub fn at_bottom(&self) -> bool {
        self.scroll_top() >= self.scroll_max()
    }

    /// Whether a reader inside the follow band is riding the live tail.
    pub fn follows_bottom(&self) -> bool {
        follows_scroll_bottom(
            self.scroll_box_geometry(),
            effective_row_height(self.row_height()),
        )
    }

    /// The exact bottom clamp, which is zero when the box has no range at all.
    pub fn scroll_max(&self) -> f64 {
        (self.scroll_height() - self.client_height()).max(0.0)
    }

    /// The live scroll geometry, for a caller that derives its own offsets.
    pub fn scroll_box_geometry(&self) -> ScrollBoxGeometry {
        ScrollBoxGeometry {
            scroll_top: self.scroll_top(),
            scroll_height: self.scroll_height(),
            client_height: self.client_height(),
        }
    }

    /// The pitch every derived scroll offset multiplies, falling back to the
    /// default so an unmeasured pane never computes against zero.
    pub fn effective_row_pitch(&self) -> f64 {
        effective_row_height(self.row_height())
    }

    pub(crate) fn scroll_top(&self) -> f64 {
        scroll_top_of(&self.container)
    }

    pub(crate) fn scroll_height(&self) -> f64 {
        f64::from(self.container.scroll_height())
    }

    pub(crate) fn client_height(&self) -> f64 {
        f64::from(self.container.client_height())
    }

    /// Write a scroll position and stamp it with an owned epoch.
    ///
    /// The epoch is what makes the scroll event this write provokes
    /// recognisable as the renderer's own, so a find navigation's write to a
    /// tail is not read as a user gesture that returns to live. A write that
    /// changes nothing, or that lands fully clamped, leaves no stale ownership.
    fn write_scroll_top(&mut self, value: f64) {
        let before = self.scroll_top();
        if before != value {
            set_scroll_top_of(&self.container, value);
        }
        let after = self.scroll_top();
        if after != before && self.owned_scroll_epoch == 0 {
            self.next_owned_scroll_epoch = self.next_owned_scroll_epoch.wrapping_add(1);
            self.owned_scroll_epoch = self.next_owned_scroll_epoch;
        }
        if self.owned_scroll_epoch != 0 {
            self.owned_scroll_top = after;
        }
    }

    /// Pin the pane to the bottom, if the pre-mutation capture said it was
    /// following. This is the ONLY path that writes position.
    pub(crate) fn pin_to_bottom(&mut self, should_pin: bool) {
        if !should_pin {
            return;
        }
        let bottom = self.scroll_max();
        self.last_scroll_max = bottom;
        self.write_scroll_top(bottom);
    }

    /// The pre-mutation capture: a band follower, or an exact renderer-owned
    /// placement.
    pub(crate) fn at_bottom_or_owned_placement(&self) -> bool {
        self.follows_bottom()
            || (self.owned_scroll_epoch != 0 && self.scroll_top() == self.owned_scroll_top)
    }

    /// Handle one scroll event on the container.
    ///
    /// Origin is distinguished from the facts the event itself carries: the
    /// renderer-owned epoch computed at the top, plus the last observed scroll
    /// MAXIMUM. A user scroll onto the exact bottom is the universal return to
    /// live and means the same thing for every reason, `find` included — so it
    /// resumes explicitly. An event that only follows a shrunken maximum is a
    /// clamp no gesture aimed at, and an anchor park outranks it; because the
    /// maximum is recorded before every classification, the suppression is
    /// one-shot per SHRINK, not per gesture. A maximum of zero is never a clamp:
    /// with no range nothing is aimed at, so every park yields.
    pub fn handle_scroll(&mut self) -> LiveInteractionResult {
        let max = self.scroll_max();
        let clamped = max > 0.0 && max < self.last_scroll_max;
        self.last_scroll_max = max;
        let mut owned = false;
        if self.owned_scroll_epoch != 0 {
            owned = self.scroll_top() == self.owned_scroll_top;
            let bottom = self.at_bottom();
            if !owned || bottom {
                self.owned_scroll_epoch = 0;
            }
            // An owned event that landed on the bottom is the only proof a
            // parked reader returned; swallowing it leaves the pane parked with
            // no retry.
            if owned && !bottom {
                return NO_LIVE_INTERACTION_RESULT;
            }
        }
        if self.reader.intent() == ReaderIntent::Reading
            && self.reader.reason() == Some(ReaderIntentReason::Find)
        {
            if owned || clamped || !self.at_bottom() {
                self.capture_reader_anchor();
                return NO_LIVE_INTERACTION_RESULT;
            }
            return self.resume_live(false, true);
        }
        if self.live_selection_release_pending && !self.at_bottom() {
            self.live_selection_release_pending = false;
            return self.resume_live(false, false);
        }
        if self.at_bottom() {
            return self.resume_live(false, false);
        }
        if self.reader.intent() == ReaderIntent::Live && !self.follows_bottom() {
            self.enter_reading(ReaderIntentReason::NativeScroll);
            self.settle_bottom_park();
        }
        NO_LIVE_INTERACTION_RESULT
    }

    /// A box change the pane's resize observer saw.
    ///
    /// The previous height is consumed even on a refusal, because the observer
    /// cannot retry the same transition: a park that declined here and was
    /// allowed to try again on the next tick would be the pane's only repair
    /// running with no deadline at all.
    pub fn note_box_resize(&mut self) -> LiveInteractionResult {
        let height = self.client_height();
        let previous = self.last_box_height;
        if height > 0.0 {
            self.last_box_height = height;
        }
        if previous <= 0.0 || height <= 0.0 || height == previous {
            return NO_LIVE_INTERACTION_RESULT;
        }
        let was_at_old_bottom =
            self.scroll_top() >= (self.scroll_height() - previous.max(height)).max(0.0);
        if !was_at_old_bottom {
            return NO_LIVE_INTERACTION_RESULT;
        }
        // A grow that leaves no scroll range can never fire another scroll
        // event, so this observer tick is the last chance to resume; and with no
        // range there is no reader position left to protect, whatever parked it.
        if self.reader.intent() == ReaderIntent::Reading
            && !self
                .reader
                .reason()
                .is_some_and(ReaderIntentReason::is_position_only)
            && self.scroll_height() > height
        {
            return NO_LIVE_INTERACTION_RESULT;
        }
        self.resume_live(false, true)
    }

    /// Scroll the reader to a history row it has already painted, parking it on
    /// that row's find anchor.
    ///
    /// Only a painted row is a legal destination: jumping to a reserved
    /// interval would land the reader in blank space that no page was ever
    /// asked for.
    pub fn scroll_to_scrollback_row(&mut self, absolute_index: u32) -> bool {
        if !self.has_painted_scrollback_range(absolute_index, absolute_index.saturating_add(1)) {
            return false;
        }
        let row_height = self.row_height();
        if row_height <= 0.0 {
            return false;
        }
        self.reader.enter_reading(ReaderIntentReason::Find);
        self.capture_reader_anchor();
        let top = f64::from(self.spacer.offset_top()) + f64::from(absolute_index) * row_height;
        let target = (top - self.client_height() / 3.0).clamp(0.0, self.scroll_max());
        self.write_scroll_top(target);
        true
    }

    /// The missing interval the reader's own scroll position exposes, for a
    /// demand page.
    pub fn missing_scrollback_range_at_scroll(
        &self,
        ahead_rows: u32,
    ) -> Option<roost_client_core::terminal::history::HistoryScrollTarget> {
        let anchor = self.backfill_anchor()?;
        if self.scrollback_layout_end != anchor.total {
            return None;
        }
        self.painted.missing_range_at_scroll(
            u32::try_from(anchor.total).unwrap_or(u32::MAX),
            self.scroll_top(),
            f64::from(self.spacer.offset_top()),
            self.client_height(),
            effective_row_height(self.row_height()),
            ahead_rows,
        )
    }

    /// The missing interval containing one history row, for a demand.
    pub fn missing_scrollback_range(&self, row: u32) -> Option<HistoryRange> {
        let anchor = self.backfill_anchor()?;
        if self.scrollback_layout_end != anchor.total {
            return None;
        }
        self.painted
            .missing_range_at(u32::try_from(anchor.total).unwrap_or(u32::MAX), row)
    }
}
