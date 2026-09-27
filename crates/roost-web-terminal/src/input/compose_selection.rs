//! The handoff between a terminal's native document range and the composer's
//! private textarea selection. A focused textarea cannot begin its native
//! editing command while a document range is still active, so the composer
//! yields the range before it edits and restores it after. `deferrals` owns
//! the one-shot tokens that survive across the browser's default edit.
//!
//! Every step is ordered against a browser default this module does not
//! control, so the state is here, the DOM is not, and the adapter fills a
//! `PaneInputs`.

mod deferrals;

use crate::input::selection::{DomNodeId, LiveSelection, RetainedRange, SelectionGuard};

/// The document facts one compose transition is decided from.
#[derive(Debug, Clone, Copy)]
pub struct PaneInputs<'a> {
    /// The document's selection, as read now.
    pub live: &'a LiveSelection,
    /// The capture the guard retained, as re-read now. `None` when the pane
    /// holds no capture.
    pub retained: Option<&'a RetainedRange>,
    /// This pane's display element.
    pub display: DomNodeId,
}

/// Which way a textarea selection runs, which the browser needs to place the
/// caret correctly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SelectionDirection {
    /// Anchor before focus.
    Forward,
    /// Focus before anchor.
    Backward,
    /// A bare caret, or a browser that reported nothing.
    #[default]
    None,
}

/// A textarea's own selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComposerSelection {
    /// The selection's start offset.
    pub start: u32,
    /// The selection's end offset.
    pub end: u32,
    /// Which way the selection runs.
    pub direction: SelectionDirection,
}

impl ComposerSelection {
    /// A collapsed caret at `position`.
    pub const fn caret(position: u32) -> Self {
        Self {
            start: position,
            end: position,
            direction: SelectionDirection::None,
        }
    }
}

/// What a transition asks the adapter to do, in field order. Every flag
/// defaults to false, so a transition that only moves state says nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ComposeEffects {
    /// Capture the pane's live selection for a guarded cycle.
    pub capture: bool,
    /// The adapter must clear the document's ranges, which is how the yield
    /// actually happens. The suspension is armed either way, so a keystroke is
    /// never swallowed by a refused clear.
    pub clear_ranges: bool,
    /// Restore the captured range.
    pub restore: bool,
    /// Drop the capture and every deferral.
    pub release: bool,
    /// Write a selection into the composer textarea.
    pub set_composer_selection: Option<ComposerSelection>,
    /// Arm the layout transaction that restores after the browser's default
    /// edit, carrying the version it was armed with.
    pub schedule_layout_restore: Option<u64>,
}

/// The one deferred keyup restore, identified by the input it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DeferredKeyupRestore {
    input: u32,
    guard_epoch: u64,
}

/// The composer half of the terminal selection handoff.
#[derive(Debug, Default)]
pub struct ComposeSelection {
    /// The guard epoch the retained capture belongs to, or `None` when the
    /// composer holds no range. The epoch IS the identity: a capture taken
    /// after a transition is a different capture even at the same address.
    retained_epoch: Option<u64>,
    selection_start: u32,
    selection_end: u32,
    selection_direction: SelectionDirection,
    composer_selection_active: bool,
    composing: bool,
    deferred_keyup_restore: Option<DeferredKeyupRestore>,
    deferred_layout_guard: Option<u64>,
    layout_restore_version: u64,
    pending_suspend_selection_change: bool,
}

impl ComposeSelection {
    /// A composer that has retained no range.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a terminal range is currently retained.
    pub fn has_guard(&self) -> bool {
        self.retained_epoch.is_some()
    }

    /// Whether an IME composition is in flight.
    pub fn is_composing(&self) -> bool {
        self.composing
    }

    /// Whether the composer owns the caret rather than the terminal range.
    pub fn composer_selection_active(&self) -> bool {
        self.composer_selection_active
    }

    /// The remembered composer selection.
    pub const fn remembered_selection(&self) -> ComposerSelection {
        ComposerSelection {
            start: self.selection_start,
            end: self.selection_end,
            direction: self.selection_direction,
        }
    }

    /// Record that the composer now owns its own selection.
    pub fn mark_composer_selection_active(&mut self) {
        self.composer_selection_active = true;
    }

    /// Remember where the composer puts its caret, with no selection.
    pub fn remember_caret_at(&mut self, position: u32) {
        self.remember_composer_selection(ComposerSelection::caret(position));
    }

    /// Read the composer's current selection out of the textarea.
    pub fn remember_composer_selection(&mut self, selection: ComposerSelection) {
        self.selection_start = selection.start;
        self.selection_end = selection.end;
        self.selection_direction = selection.direction;
    }

    /// Retain the pane's current selection for a guarded cycle.
    ///
    /// A new capture supersedes the old one entirely, its pending restores
    /// included: an older schedule restoring a newer range is how a stale
    /// caret ends up over text the user has since selected elsewhere.
    pub fn capture(
        &mut self,
        guard: &mut SelectionGuard,
        inputs: PaneInputs<'_>,
    ) -> ComposeEffects {
        if !guard.capture(inputs.live, inputs.display) {
            return ComposeEffects::default();
        }
        self.supersede_deferrals();
        self.retained_epoch = Some(guard.epoch());
        self.composer_selection_active = false;
        ComposeEffects {
            capture: true,
            ..ComposeEffects::default()
        }
    }

    /// Drop the retained range and every deferral, leaving the document's own
    /// selection untouched.
    pub fn release(&mut self, guard: &mut SelectionGuard) -> ComposeEffects {
        self.supersede_deferrals();
        self.retained_epoch = None;
        self.composer_selection_active = false;
        guard.release();
        ComposeEffects {
            release: true,
            ..ComposeEffects::default()
        }
    }

    /// Restore the terminal range, reporting whether it was still restorable.
    ///
    /// A restore that finds the range gone ends the capture rather than
    /// retrying it, and only when no layout transaction still owns it: a
    /// superseded schedule is inert, and releasing on its account would drop a
    /// range the newer transaction is about to restore correctly.
    pub fn restore(
        &mut self,
        guard: &mut SelectionGuard,
        inputs: PaneInputs<'_>,
    ) -> ComposeEffects {
        if self.retained_epoch.is_none() {
            return ComposeEffects::default();
        }
        if guard.restore(inputs.live, inputs.retained) {
            self.composer_selection_active = false;
            return ComposeEffects {
                restore: true,
                ..ComposeEffects::default()
            };
        }
        if self.deferred_layout_guard != self.retained_epoch {
            self.release(guard);
        }
        ComposeEffects::default()
    }

    /// Yield the retained range to the focused editor, or `None` when the
    /// capture cannot be suspended at all.
    ///
    /// `clear_ranges` is separate from the suspension because the document's
    /// clear removes ALL of its ranges: when the capture is not the document's
    /// own range there is nothing to remove, and another owner's selection
    /// must survive.
    pub fn suspend(
        &mut self,
        guard: &mut SelectionGuard,
        inputs: PaneInputs<'_>,
    ) -> Option<ComposeEffects> {
        let queues_empty_selection = inputs.live.is_live_range();
        if !guard.suspend(inputs.live, inputs.retained) {
            return None;
        }
        if queues_empty_selection {
            // The clear the yield performs arrives as its own selectionchange.
            // Treating that as a user collapse would release the very capture
            // the composer is still holding, so this token consumes it.
            self.pending_suspend_selection_change = true;
        }
        Some(ComposeEffects {
            clear_ranges: guard.suspend_clears_ranges(inputs.live),
            ..ComposeEffects::default()
        })
    }

    /// Give the composer its caret before it edits.
    ///
    /// Browser automation, soft keyboards and paste can establish a real
    /// caret before any `beforeinput` arrives, so a collapsed native selection
    /// is taken as the composer already owning the document rather than
    /// overwritten with a remembered one.
    pub fn activate_composer_selection(
        &mut self,
        guard: &mut SelectionGuard,
        inputs: PaneInputs<'_>,
    ) -> ComposeEffects {
        if self.retained_epoch.is_none() || self.composer_selection_active {
            return ComposeEffects::default();
        }
        let composer_already_owns = !inputs.live.is_live_range();
        let Some(mut effects) = self.suspend(guard, inputs) else {
            self.release(guard);
            return ComposeEffects::default();
        };
        self.composer_selection_active = true;
        if !composer_already_owns {
            effects.set_composer_selection = Some(self.remembered_selection());
        }
        effects
    }

    /// A key went down, or the editor is about to insert text. Both must find
    /// the composer holding its own caret before the browser's default runs.
    pub fn on_key_or_before_input(
        &mut self,
        guard: &mut SelectionGuard,
        inputs: PaneInputs<'_>,
    ) -> ComposeEffects {
        if self.composing {
            return ComposeEffects::default();
        }
        self.activate_composer_selection(guard, inputs)
    }

    /// An IME composition began: the composer needs its caret before the
    /// preedit appears.
    pub fn on_composition_start(
        &mut self,
        guard: &mut SelectionGuard,
        inputs: PaneInputs<'_>,
    ) -> ComposeEffects {
        self.composing = true;
        self.activate_composer_selection(guard, inputs)
    }

    /// A programmatic write — a fill, an autofill, an accessibility action —
    /// has no pointerdown to capture the terminal range first, so the capture
    /// is taken and suspended together.
    pub fn prepare_programmatic_write(
        &mut self,
        guard: &mut SelectionGuard,
        inputs: PaneInputs<'_>,
    ) -> ComposeEffects {
        if self.retained_epoch.is_none() {
            self.capture(guard, inputs);
        }
        if self.retained_epoch.is_some()
            && let Some(effects) = self.suspend(guard, inputs)
        {
            return effects;
        }
        ComposeEffects::default()
    }

}
