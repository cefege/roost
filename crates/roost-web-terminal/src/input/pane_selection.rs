//! The pane's native-selection guard, wired to the DOM: the `SelectionGuard`
//! state machine, the `DomSelectionReader` that feeds it, and the paint hold it
//! drives. The terminal pane owns one per mount and calls
//! `sync_native_selection_hold` on every `selectionchange` and re-attach,
//! `prepare_live_interaction` before live input and `release_paint_holds` when
//! it leaves the visible surface; the composer drives captures through
//! `compose_dom`. Ports v2's `apps/web/src/renderer/terminalSelectionGuard.ts`
//! (`createTerminalSelectionGuard`).
//!
//! Selection-API call ordering is load-bearing: Chromium resets the native
//! editing target only through the Selection-wide clear, and can dispatch a
//! reveal scroll after animation callbacks. Nothing here may be reordered.

use web_sys::{Document, Element};

use crate::cell_renderer::CellGridRenderer;
use crate::input::compose_selection::{ComposeEffects, PaneInputs};
use crate::input::dom::DomSelectionReader;
use crate::input::selection::{
    DomNodeId, LiveSelection, RestoreWrite, RetainedRange, SelectionGuard,
};
use crate::presentation::LiveInteractionResult;

/// One owned read of the pane's selection facts.
#[derive(Debug, Clone)]
pub struct PaneRead {
    /// The document's selection now.
    pub live: LiveSelection,
    /// The retained capture, re-read now.
    pub retained: Option<RetainedRange>,
    /// The pane's display identity.
    pub display: DomNodeId,
}

impl PaneRead {
    /// Borrow this read as the facts one compose transition is decided from.
    pub fn inputs(&self) -> PaneInputs<'_> {
        PaneInputs {
            live: &self.live,
            retained: self.retained.as_ref(),
            display: self.display,
        }
    }
}

/// Applies a paint hold to the renderer. The pane supplies it: when `held`,
/// `enter_reading(Selection)`, then `set_selection_hold(held)`, and a result
/// whose anchor moved is handed to the backfill pager. It runs inside this
/// guard's calls, so it must not call back into the guard.
type HoldSink = Box<dyn FnMut(bool)>;

/// One pane's selection guard.
pub struct PaneSelection {
    guard: SelectionGuard,
    reader: DomSelectionReader,
    apply_hold: HoldSink,
}

impl std::fmt::Debug for PaneSelection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaneSelection")
            .field("guard", &self.guard)
            .field("reader", &self.reader)
            .finish_non_exhaustive()
    }
}

impl PaneSelection {
    /// A guard over the pane's display (its scroll container).
    pub fn new(
        document: &Document,
        display: &Element,
        apply_hold: impl FnMut(bool) + 'static,
    ) -> Self {
        Self {
            guard: SelectionGuard::new(),
            reader: DomSelectionReader::new(document, display),
            apply_hold: Box::new(apply_hold),
        }
    }

    /// Read the document's selection and the retained capture.
    pub fn read(&self) -> PaneRead {
        let display = self.reader.display_id();
        PaneRead {
            live: self.reader.read(),
            retained: self.reader.read_retained(display),
            display,
        }
    }

    /// The guard, for a compose transition that judges against it.
    pub fn guard_mut(&mut self) -> &mut SelectionGuard {
        &mut self.guard
    }

    /// The document this guard reads.
    pub fn document(&self) -> &Document {
        self.reader.document()
    }

    /// Recompute the renderer's selection hold from the live document. A
    /// suspension that stopped holding is named, never silently dropped.
    pub fn sync_native_selection_hold(&mut self) {
        let read = self.read();
        let sync = self.guard.sync_hold(&read.live, read.retained.as_ref());
        if let Some(lapse) = sync.lapse {
            tracing::info!(target: "selection", reason = lapse.reason(), "cell.selection_yield_lapsed");
        }
        (self.apply_hold)(sync.hold);
    }

    /// Retain the pane's current selection for a guarded suspend/restore
    /// cycle. False when there is no pane-owned range to retain.
    pub fn capture_terminal_selection(&mut self) -> bool {
        let read = self.read();
        if !self.guard.capture(&read.live, read.display) {
            return false;
        }
        self.apply(&ComposeEffects {
            capture: true,
            ..ComposeEffects::default()
        });
        true
    }

    /// Apply what a compose transition decided, in v2's order: retain, yield,
    /// forget, then re-derive the hold from what the document now says.
    pub fn apply(&mut self, effects: &ComposeEffects) {
        if effects.capture && !self.reader.retain_current_range() {
            self.guard.release();
        }
        if effects.clear_ranges {
            self.reader.clear_ranges();
        }
        if effects.suspended {
            let focus_owner = self.guard.suspended_epoch().is_some();
            tracing::debug!(target: "selection", focus_owner, "cell.selection_yield_suspended");
        }
        if effects.release {
            self.reader.forget_retained();
        }
        if effects.capture || effects.release || effects.restore {
            self.sync_native_selection_hold();
        }
    }

    /// The write half of a restore: put the captured range back in the
    /// document when it is not already there, and read what the document says
    /// now. A document that refuses the write ends the capture.
    pub fn write_restore(&mut self) -> PaneRead {
        let read = self.read();
        let target = self
            .guard
            .restore_target(&read.live, read.retained.as_ref());
        let Some(RestoreWrite::SetBaseAndExtent { .. }) = target else {
            return read;
        };
        let written = read
            .retained
            .as_ref()
            .is_some_and(|retained| self.reader.restore_retained(retained));
        if !written {
            self.guard.release();
        }
        self.read()
    }

    /// Transition to live: drop reader holds and any pane-owned selection.
    pub fn prepare_live_interaction(
        &mut self,
        renderer: &mut CellGridRenderer,
    ) -> LiveInteractionResult {
        self.end_reader_intervals(renderer, "live_interaction")
    }

    /// Leaving the visible surface ends every reader interval: a kept selection
    /// would freeze the pane on the frame that was current when it left and
    /// present that stale grid on its next reveal.
    pub fn release_paint_holds(
        &mut self,
        renderer: &mut CellGridRenderer,
    ) -> LiveInteractionResult {
        self.end_reader_intervals(renderer, "release_paint_holds")
    }

    /// The caller still owes `release_interaction` on the link attachment and,
    /// when `anchor_changed`, the backfill pager's `on_full_frame`.
    fn end_reader_intervals(
        &mut self,
        renderer: &mut CellGridRenderer,
        cause: &'static str,
    ) -> LiveInteractionResult {
        self.guard.prepare_live_interaction();
        self.reader.forget_retained();
        // Ownership is read BEFORE the renderer reconciles, which can detach
        // the row the selection sits in.
        let owned = self.reader.read().pane_owns_endpoint();
        let result = renderer.prepare_live_interaction();
        // The renderer's intent, both composed holds, the canonical frame and
        // the bottom anchor have moved as one transition; the reveal scroll the
        // clear triggers is bracketed until it arrives.
        if owned {
            renderer.begin_live_selection_release();
            self.reader.clear_ranges();
        }
        tracing::debug!(target: "selection", cause, owned, "terminal selection released");
        result
    }
}
