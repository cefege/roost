//! The one-shot tokens a browser's own default edit leaves behind: the deferred
//! keyup restore, the armed layout transaction, and the selectionchange the
//! composer's own yield produces.
//!
//! A file split, not a type split: the state lives on `ComposeSelection`, which
//! `compose_selection` owns. That half decides when the terminal range is
//! retained and who owns the caret; this decides which of its own notifications
//! are consumed as its own doing rather than read as the user giving up.

use crate::input::compose_selection::{ComposeEffects, ComposeSelection, DeferredKeyupRestore};
use crate::input::selection::LiveSelection;

impl ComposeSelection {
    /// A key came back up: the browser's default edit has run, so the layout
    /// transaction that restores the range may be armed.
    pub fn on_key_up(&mut self, input: u32) -> ComposeEffects {
        if self.composing {
            return ComposeEffects::default();
        }
        self.restore_after_layout(Some(input));
        self.layout_effects()
    }

    /// An IME composition ended: the range comes back once the commit has
    /// landed, which the adapter does on the following microtask.
    pub fn on_composition_end(&mut self) -> ComposeEffects {
        self.composing = false;
        self.layout_effects()
    }

    /// Arm the one layout transaction that restores after the browser's
    /// default edit, superseding any schedule already armed.
    pub fn restore_after_layout(&mut self, input: Option<u32>) {
        let Some(epoch) = self.retained_epoch else {
            return;
        };
        self.layout_restore_version += 1;
        self.deferred_layout_guard = Some(epoch);
        if let Some(input) = input {
            self.deferred_keyup_restore = Some(DeferredKeyupRestore {
                input,
                guard_epoch: epoch,
            });
        }
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

    /// The document selection changed somewhere on the page. The clear the
    /// composer's own yield performs arrives here, and the keyup restore's own
    /// notification can arrive before the browser's following collapse — so
    /// both are consumed by their one-shot tokens rather than treated as the
    /// user abandoning the terminal range.
    pub fn on_document_selection_change(
        &mut self,
        live: &LiveSelection,
        focus_in_dock: bool,
    ) -> ComposeEffects {
        if self.pending_suspend_selection_change && live.range_count == 0 {
            self.pending_suspend_selection_change = false;
            return ComposeEffects::default();
        }
        if live.is_live_range() {
            return ComposeEffects {
                capture: self.deferred_keyup_restore.is_none(),
                ..ComposeEffects::default()
            };
        }
        if self.deferred_keyup_restore.is_some() && !self.composing {
            return self.layout_effects();
        }
        ComposeEffects {
            release: !focus_in_dock,
            ..ComposeEffects::default()
        }
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
