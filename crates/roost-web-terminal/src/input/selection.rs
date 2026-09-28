//! The selection guard: when the front end may take or move the terminal's
//! selection, and when the renderer must hold instead of painting.
//!
//! The renderer replaces DOM on every accepted frame, so a selection the user
//! is still holding has to be captured by ROW IDENTITY and revalidated before
//! it is ever restored — a canonical repair can keep the same text while
//! replacing its nodes, and resurrecting that detached range is the defect
//! this module exists to make impossible.
//!
//! TWO READS, NEVER ONE. `LiveSelection` is the DOCUMENT's selection, and the
//! paint hold is derived from it on every sync, so no hold outlives the reason
//! it was taken for. `RetainedRange` is the CAPTURED range, which survives the
//! clear its own yield performs; a hold revalidated against the live selection
//! would lapse on the very clear that made the composer editable.
//!
//! The state is native and the DOM is not: `pane_selection` fills the two
//! reads and applies what the guard decides. Ports the capture/suspend/restore
//! rules of v2's `apps/web/src/renderer/terminalSelectionGuard.ts`.

mod snapshot;

pub use snapshot::{
    DomNodeId, FocusOwner, LiveSelection, OwnedRow, RetainedRange, SelectionEndpoint,
};

use crate::reader_intent::RENDERER_HOLD_SELECTION;

/// Why a suspension stopped holding paint. A lapse is named rather than
/// silently dropped: a silent one hides the defect behind a pane that merely
/// started painting again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YieldLapse {
    /// The captured range no longer validates by row identity, or another
    /// owner established a selection in its place.
    CaptureGone,
    /// The element that owned focus at suspend time lost it or disconnected.
    OwnerGone,
}

impl YieldLapse {
    /// The reason string the diagnostic sink records.
    pub const fn reason(self) -> &'static str {
        match self {
            Self::CaptureGone => "capture_gone",
            Self::OwnerGone => "owner_gone",
        }
    }
}

/// What one sync of the hold decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HoldSync {
    /// Whether paint must be held.
    pub hold: bool,
    /// A suspension that stopped holding during this sync.
    pub lapse: Option<YieldLapse>,
}

/// What a restore must write to the document before it can be judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreWrite {
    /// The document's selection already is the captured range.
    AlreadyLive,
    /// Re-establish the captured range at these endpoints (`setBaseAndExtent`).
    SetBaseAndExtent {
        /// The captured anchor.
        anchor: SelectionEndpoint,
        /// The captured focus.
        focus: SelectionEndpoint,
    },
}

/// One capture: the range as taken, and the rows that make it restorable.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CapturedRange {
    /// The epoch the capture was taken in. Any transition bumps the epoch,
    /// which invalidates every capture at once.
    epoch: u64,
    display: DomNodeId,
    anchor: SelectionEndpoint,
    focus: SelectionEndpoint,
    /// The selected text at capture time.
    text: String,
    /// The rows the capture depends on.
    rows: Vec<OwnedRow>,
}

impl CapturedRange {
    /// Whether `live` is this capture's own range, endpoint for endpoint.
    fn is_live_range(&self, live: &LiveSelection) -> bool {
        live.anchor == Some(self.anchor) && live.focus == Some(self.focus)
    }

    /// Whether the retained range still says what it said when it was taken.
    fn survives(&self, retained: &RetainedRange) -> bool {
        retained.display == self.display
            && retained.anchor == self.anchor
            && retained.focus == self.focus
            && retained.containers_connected
            && retained.range_text == self.text
            && self
                .rows
                .iter()
                .all(|captured| retained.rows.iter().any(|row| row == captured))
    }
}

/// The guard one pane owns: its epoch, its active capture, and the suspension
/// that capture may currently be under.
#[derive(Debug, Default)]
pub struct SelectionGuard {
    epoch: u64,
    captured: Option<CapturedRange>,
    suspended_for: Option<FocusOwner>,
}

impl SelectionGuard {
    /// A guard with no capture, no suspension, and hold off.
    pub fn new() -> Self {
        Self::default()
    }

    /// The hold bit this guard contributes to the renderer's mask.
    pub const fn hold_bit() -> u32 {
        RENDERER_HOLD_SELECTION
    }

    /// The guard's current generation. Every transition bumps it, which is
    /// what a caller compares against to tell one capture from the next.
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Whether a capture is retained.
    pub fn has_capture(&self) -> bool {
        self.captured.is_some()
    }

    /// Recompute the hold from the live document.
    ///
    /// This is the single evaluator, and it runs on every selection change AND
    /// on every re-attach. Latching the hold on an edge instead is the defect
    /// this replaces: the pane's global listeners are absent for a whole
    /// foreground withdraw, and a selection dropped inside that window never
    /// produces the edge that would clear it.
    pub fn sync_hold(
        &mut self,
        live: &LiveSelection,
        retained: Option<&RetainedRange>,
    ) -> HoldSync {
        let mut lapse = None;
        let suspension_holds = self.suspension_holds_paint(live, retained, &mut lapse);
        let live_range_holds = live.is_range() && live.pane_owns_endpoint();
        HoldSync {
            hold: suspension_holds || live_range_holds,
            lapse,
        }
    }

    /// Whether a yield is armed against the current capture, which is what
    /// tells a composer whose activation succeeded from one whose capture was
    /// refused. A suspension with no focus owner is armed and holds nothing.
    pub fn suspended_epoch(&self) -> Option<u64> {
        self.suspended_for.map(|_| self.epoch)
    }

    /// Capture the live selection for a guarded suspend/restore cycle, and
    /// report whether there was one to capture.
    ///
    /// Refused unless the selection is a real range carrying text AND an
    /// endpoint resolves to a row this pane painted. A selection with no
    /// endpoint in a row is not a range the pane can revalidate, and restoring
    /// it would drag the user's selection across the rest of the page.
    pub fn capture(&mut self, live: &LiveSelection, display: DomNodeId) -> bool {
        if !live.is_live_range() || !live.pane_owns_endpoint() || live.owned_rows.is_empty() {
            return false;
        }
        let (Some(anchor), Some(focus)) = (live.anchor, live.focus) else {
            return false;
        };
        self.captured = Some(CapturedRange {
            epoch: self.epoch,
            display,
            anchor,
            focus,
            text: live.text.clone(),
            rows: live.owned_rows.clone(),
        });
        self.suspended_for = None;
        true
    }

    /// Yield the captured range to the focused editor, retaining it for a
    /// guarded restore. Reports whether the suspension is now live.
    ///
    /// The document's clear removes ALL of its ranges, so a selection the
    /// capture does not account for is left alone: the yield then has nothing
    /// to remove. It still reports suspended, so a keystroke is never
    /// swallowed by a refused range removal.
    pub fn suspend(&mut self, live: &LiveSelection, retained: Option<&RetainedRange>) -> bool {
        if !self.capture_is_restorable(live, retained) {
            self.captured = None;
            return false;
        }
        if self
            .captured
            .as_ref()
            .is_some_and(|held| held.is_live_range(live))
            && live.range_count != 1
        {
            self.captured = None;
            self.suspended_for = None;
            return false;
        }
        // A suspension with nothing focused has no owner to wait for and never
        // holds paint at all, which is what stops a torn-down composer from
        // wedging the pane for good.
        self.suspended_for = live.focus_owner;
        true
    }

    /// Whether the document's own range IS the captured one, i.e. whether the
    /// adapter must clear the document's ranges to actually yield them.
    pub fn suspend_clears_ranges(&self, live: &LiveSelection) -> bool {
        self.captured
            .as_ref()
            .is_some_and(|held| held.is_live_range(live))
    }

    /// The first half of a restore: what must be written to the document so
    /// `restore` can judge the result. `None` ends the capture — it no longer
    /// validates, or another owner established a range in its place.
    pub fn restore_target(
        &mut self,
        live: &LiveSelection,
        retained: Option<&RetainedRange>,
    ) -> Option<RestoreWrite> {
        if !self.capture_is_restorable(live, retained) {
            self.captured = None;
            return None;
        }
        let held = self.captured.as_ref()?;
        Some(if held.is_live_range(live) {
            RestoreWrite::AlreadyLive
        } else {
            RestoreWrite::SetBaseAndExtent {
                anchor: held.anchor,
                focus: held.focus,
            }
        })
    }

    /// Judge a restore against the document AFTER `restore_target`'s write,
    /// reporting whether the captured range is live again.
    ///
    /// A restore that finds the range gone ends the capture rather than
    /// retrying it: the rows the capture named are gone, and the next attempt
    /// would resurrect a detached range.
    pub fn restore(&mut self, live: &LiveSelection, retained: Option<&RetainedRange>) -> bool {
        if !self.capture_is_restorable(live, retained) {
            self.captured = None;
            return false;
        }
        let restored = self
            .captured
            .as_ref()
            .is_some_and(|held| held.is_live_range(live) && live.text == held.text);
        if live.collapsed || !restored {
            self.captured = None;
            self.suspended_for = None;
            return false;
        }
        self.suspended_for = None;
        true
    }

    /// Drop every retained reference without changing the current selection.
    pub fn release(&mut self) {
        self.captured = None;
        self.suspended_for = None;
    }

    /// Transition to live: end every reader interval this pane owns.
    ///
    /// The epoch moves FIRST, so a capture taken in the previous epoch is
    /// already invalid before any DOM callback can run against it.
    pub fn prepare_live_interaction(&mut self) {
        self.epoch += 1;
        self.release();
    }

    /// Leaving the visible surface ends the same intervals. A kept selection
    /// would freeze the pane on the frame that was current when it left, and
    /// present that stale grid on its next reveal.
    pub fn release_paint_holds(&mut self) {
        self.prepare_live_interaction();
    }

    /// Whether the capture still validates: same epoch, same display, a
    /// retained range that still says what it said, over rows that are still
    /// the rows it captured, and no other owner in its place.
    ///
    /// Every conjunct is a fact that can stop being true on its own, which is
    /// why the hold is derived and never latched: the row replaced by a
    /// canonical repair, the display swapped by a remount, the selection
    /// another owner has since established.
    fn capture_is_restorable(
        &self,
        live: &LiveSelection,
        retained: Option<&RetainedRange>,
    ) -> bool {
        let (Some(captured), Some(retained)) = (self.captured.as_ref(), retained) else {
            return false;
        };
        if captured.epoch != self.epoch || !captured.survives(retained) {
            return false;
        }
        // A COLLAPSED live selection is the browser's editable-focus
        // artifact, not another owner claiming the range.
        live.collapsed || !live.is_range() || captured.is_live_range(live)
    }

    /// Whether a suspension still holds, recording which reason lapsed it.
    fn suspension_holds_paint(
        &mut self,
        live: &LiveSelection,
        retained: Option<&RetainedRange>,
        lapse: &mut Option<YieldLapse>,
    ) -> bool {
        let Some(owner) = self.suspended_for else {
            return false;
        };
        let capture_live = self.capture_is_restorable(live, retained);
        let owner_live = owner.connected && live.focus_owner == Some(owner);
        if capture_live && owner_live {
            return true;
        }
        self.suspended_for = None;
        *lapse = Some(if capture_live {
            YieldLapse::OwnerGone
        } else {
            YieldLapse::CaptureGone
        });
        false
    }
}
