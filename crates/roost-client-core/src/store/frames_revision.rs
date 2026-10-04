//! The store's second mutation counter: painted-frame movement only. Owned by
//! `Store`; moved by `handle_sync`'s frame folds through [`Store::note_fold`],
//! read by the web host's pump. A terminal frame changes nothing the chrome
//! renders, so a host that repainted every `revision` subscriber per frame
//! would re-render the tab strip and deck up to sixty times a second per busy
//! session.

use super::Store;
use crate::terminal::session::TerminalSession;

/// What one replica fold can move that a host repaints for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaintedMark {
    frame_revision: u64,
    baseline_ready: bool,
}

impl PaintedMark {
    /// The replica's mark as it stands.
    pub(crate) fn of(replica: &TerminalSession) -> Self {
        Self {
            frame_revision: replica.frame_revision(),
            baseline_ready: replica.baseline_ready(),
        }
    }
}

impl Store {
    /// How many painted-frame movements this store has folded. The painter
    /// subscribes to this; everything else subscribes to `revision`.
    pub fn frames_revision(&self) -> u64 {
        self.frames_revision
    }

    /// Record what one fold moved. A moved frame is a frames change only; a
    /// baseline that became ready or was lost is also a `revision` change,
    /// because the pane's status and transport label read it.
    pub(crate) fn note_fold(&mut self, before: PaintedMark, after: PaintedMark) {
        if after.frame_revision != before.frame_revision {
            self.frames_revision += 1;
        }
        if after.baseline_ready != before.baseline_ready {
            self.note_change();
        }
    }
}
