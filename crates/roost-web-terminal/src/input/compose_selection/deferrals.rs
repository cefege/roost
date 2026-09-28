//! The one-shot tokens a browser's own default edit leaves behind: the deferred
//! keyup restore, the armed layout transaction, and the selectionchange the
//! composer's own yield produces.
//!
//! A file split, not a type split: the state lives on `ComposeSelection`, which
//! `compose_selection` owns. That half decides when the terminal range is
//! retained and who owns the caret; this decides which of its own notifications
//! are consumed as its own doing rather than read as the user giving up. Ports
//! the keyup/layout/selectionchange half of v2's
//! `apps/web/src/renderer/terminalComposeSelection.ts`.

use crate::input::compose_selection::{
    ComposeEffects, ComposeSelection, DeferredKeyupRestore, PaneInputs,
};
use crate::input::selection::SelectionGuard;

/// What the adapter knows about the page when the document selection changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionChangeFacts {
    /// Focus is inside the composer's dock.
    pub focus_in_dock: bool,
    /// The composer's current input, when it is connected and is the
    /// document's active element — the only input a deferred keyup restore
    /// may still run for.
    pub focused_input: Option<u32>,
}

impl ComposeSelection {
    /// A key came back up: the browser's default edit has run, so the layout
    /// transaction that restores the range may be armed.
    pub fn on_key_up(&mut self, input: u32) -> ComposeEffects {
        if self.composing {
            return ComposeEffects::default();
        }
        if self.retained_epoch.is_none() {
            self.deferred_keyup_restore = None;
        }
        self.restore_after_layout(Some(input));
        self.layout_effects()
    }

    /// An IME composition ended: the range comes back once the commit has
    /// landed, on the following microtask.
    pub fn on_composition_end(&mut self) -> ComposeEffects {
        self.composing = false;
        ComposeEffects {
            restore_next_microtask: true,
            ..ComposeEffects::default()
        }
    }

    /// Arm the one layout transaction that restores after the browser's
    /// default edit, superseding any schedule already armed. `input` is the
    /// composer's textarea, when it has one.
    pub fn restore_after_layout(&mut self, input: Option<u32>) {
        let Some(epoch) = self.retained_epoch else {
            return;
        };
        if let Some(input) = input {
            self.deferred_keyup_restore = Some(DeferredKeyupRestore {
                input,
                guard_epoch: epoch,
            });
        }
        self.layout_restore_version += 1;
        self.deferred_layout_guard = Some(epoch);
    }

    /// The armed layout transaction, as the version the adapter's timer must
    /// carry back. A stale version is inert, so a superseded schedule cannot
    /// restore over a newer one.
    pub fn pending_layout_version(&self) -> Option<(u64, u64)> {
        self.deferred_layout_guard
            .map(|epoch| (self.layout_restore_version, epoch))
    }

    /// Whether a timer carrying `version` still owns this guard.
    pub fn layout_restore_is_current(&self, version: u64, epoch: u64) -> bool {
        self.layout_restore_version == version
            && self.deferred_layout_guard == Some(epoch)
            && self.retained_epoch == Some(epoch)
    }

    /// The layout transaction has run.
    pub fn finish_layout_restore(&mut self) {
        self.deferred_layout_guard = None;
    }

    /// Whether the keyup restore armed for `input` may still run: the same
    /// capture, and no composition begun since.
    pub fn keyup_restore_is_current(&self, input: u32) -> bool {
        !self.composing
            && self.deferred_keyup_restore.is_some_and(|deferred| {
                deferred.input == input && self.retained_epoch == Some(deferred.guard_epoch)
            })
    }

    /// The keyup restore for `input` succeeded; its token is spent.
    pub fn finish_keyup_restore(&mut self, input: u32) {
        if self
            .deferred_keyup_restore
            .is_some_and(|deferred| deferred.input == input)
        {
            self.deferred_keyup_restore = None;
        }
    }

    /// The composer's textarea unmounted; a keyup restore armed for it is void.
    pub fn forget_input(&mut self, input: u32) {
        self.finish_keyup_restore(input);
    }

    /// The document selection changed somewhere on the page. The clear the
    /// composer's own yield performs arrives here, and the keyup restore's own
    /// notification can arrive before the browser's following collapse — so
    /// both are consumed by their one-shot tokens rather than treated as the
    /// user abandoning the terminal range. The adapter asks only while the
    /// composer is active.
    pub fn on_document_selection_change(
        &mut self,
        guard: &mut SelectionGuard,
        inputs: PaneInputs<'_>,
        facts: SelectionChangeFacts,
    ) -> ComposeEffects {
        let live = inputs.live;
        if self.pending_suspend_selection_change && live.range_count == 0 {
            self.pending_suspend_selection_change = false;
            return ComposeEffects::default();
        }
        if live.present && !live.collapsed {
            // Programmatic focus (a fill, autofill, an accessibility action)
            // has no pointerdown to capture the range before the browser
            // collapses it into the textarea, so the newest non-collapsed
            // range is retained — unless a keyup restore is still waiting for
            // the browser's following collapse, whose token it must keep.
            if self.deferred_keyup_restore.is_some() {
                return ComposeEffects::default();
            }
            return self.capture(guard, inputs);
        }
        if let Some(deferred) = self.deferred_keyup_restore
            && facts.focused_input == Some(deferred.input)
            && !self.composing
            && self.retained_epoch == Some(deferred.guard_epoch)
        {
            return ComposeEffects {
                schedule_keyup_restore: Some(deferred.input),
                ..ComposeEffects::default()
            };
        }
        if facts.focus_in_dock {
            return ComposeEffects::default();
        }
        self.release(guard)
    }

    /// The armed version, or `None` when no transaction owns the guard.
    fn layout_effects(&self) -> ComposeEffects {
        ComposeEffects {
            schedule_layout_restore: self
                .deferred_layout_guard
                .map(|_| self.layout_restore_version),
            ..ComposeEffects::default()
        }
    }

    /// Cancel every pending restore. Each new capture bumps the version, so a
    /// schedule armed against an older one can never restore this capture.
    pub(super) fn supersede_deferrals(&mut self) {
        self.layout_restore_version += 1;
        self.deferred_keyup_restore = None;
        self.deferred_layout_guard = None;
        self.pending_suspend_selection_change = false;
    }
}
