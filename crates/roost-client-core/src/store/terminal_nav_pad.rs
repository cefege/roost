//! The terminal key pad: whether the touch/controller key sheet is open, and
//! how many times a real close has asked the mounted sheet to drop its latches.
//!
//! Called by the controller router (`roost-web` `input_nav::pad_router`) and by
//! the key sheet component that renders the pad. Depends on `store` and the
//! host's `KeyValueStore`. Ported from `apps/web/src/store/terminalNavPad.ts`.
//!
//! The mounted sheet owns the Ctrl/Alt latches, and closing is the only thing
//! that can clear a latched Ctrl. v2 registered a disarm callback with an
//! identity guard so a replacement sheet could register before the previous one
//! cleaned up; here every real close advances [`TerminalNavPad::disarm_count`],
//! and whichever sheet is mounted drops its latches when it sees the count move,
//! which has no registration to race.

use crate::platform::KeyValueStore;
use crate::store::Store;

/// The stored key. `"1"` is open; anything else is closed.
pub const TERMINAL_NAV_PAD_OPEN_KEY: &str = "roostNavPadOpen";

/// The key pad's open state and its close counter.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TerminalNavPad {
    open: bool,
    disarm_count: u64,
}

impl TerminalNavPad {
    /// Closed, never closed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the sheet is open.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// How many real closes have run. A mounted sheet disarms its modifier
    /// latches whenever this differs from the value it last saw.
    pub fn disarm_count(&self) -> u64 {
        self.disarm_count
    }
}

/// Whether the key pad is open.
pub fn terminal_nav_pad_open(store: &Store) -> bool {
    store.terminal_nav_pad.open
}

/// Restore the persisted open state at boot.
///
/// Returns whether the store changed. A stored value other than `"1"` reads as
/// closed, matching v2's `=== "1"` read: a corrupt value never opens a sheet
/// over the terminal.
pub fn load_terminal_nav_pad(store: &mut Store, storage: &dyn KeyValueStore) -> bool {
    let open = storage.get(TERMINAL_NAV_PAD_OPEN_KEY).as_deref() == Some("1");
    if store.terminal_nav_pad.open == open {
        return false;
    }
    store.terminal_nav_pad.open = open;
    tracing::debug!(target: "input_nav", open, "terminal nav pad restored");
    store.note_change();
    true
}

/// The ONE close path, so every caller disarms.
///
/// A no-op when already closed: no disarm and no storage write, because a
/// close that disarmed a closed pad would drop a Ctrl the user armed from the
/// sheet's own toggle a moment earlier. Returns whether the store changed.
pub fn close_terminal_nav_pad(store: &mut Store, storage: &dyn KeyValueStore) -> bool {
    store.terminal_nav_pad.disarm_count += 1;
    persist_open(store, storage, false);
    true
}

/// The ONE key-pad toggle: the sheet's own button and the controller's
/// `keypad` / `activate` intents. Closing runs [`close_terminal_nav_pad`], so a
/// toggle-close disarms exactly like any other close. Returns whether the pad is
/// open afterwards.
pub fn toggle_terminal_nav_pad(store: &mut Store, storage: &dyn KeyValueStore) -> bool {
    if store.terminal_nav_pad.open {
        close_terminal_nav_pad(store, storage);
        return false;
    }
    persist_open(store, storage, true);
    true
}

fn persist_open(store: &mut Store, storage: &dyn KeyValueStore, open: bool) {
    store.terminal_nav_pad.open = open;
    storage.set(TERMINAL_NAV_PAD_OPEN_KEY, if open { "1" } else { "0" });
    tracing::info!(
        target: "input_nav",
        open,
        disarm_count = store.terminal_nav_pad.disarm_count,
        "terminal nav pad toggled"
    );
    store.note_change();
}
