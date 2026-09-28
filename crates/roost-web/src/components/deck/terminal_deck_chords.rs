//! The deck's document-level keyboard chords: one capture-phase listener for
//! the deck's life, reading the newest render's operations, that turns a
//! reserved press into the same operation the pointer UI runs. Called by
//! `terminal_deck`; the chord table is `terminal_deck_shortcuts`, the listener
//! is `deck_dom::Listeners`. Ports the binder in
//! `apps/web/src/components/deck/terminal-deck-shortcuts.ts`.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::deck::DeckIntent;

use super::deck_dom::Listeners;
use super::terminal_deck_operations::DeckOperations;
use super::terminal_deck_shortcuts::{
    DeckShortcut, adjacent_pane, deck_shortcut_for, focused_pane_view, tab_for_digit,
};
use crate::platform::browser_platform::{BrowserPlatform, ShortcutKey};

/// What a chord reads from the newest render.
#[derive(Debug, Clone)]
pub struct DeckChordState {
    /// This render's operations.
    pub operations: DeckOperations,
    /// Whether the deck is the visible surface.
    pub surface_visible: bool,
    /// Whether a session is floated.
    pub spotlight_active: bool,
}

impl DeckChordState {
    /// Run the chord `key` means, if any; `true` consumes the press.
    pub fn run_chord(&self, key: &ShortcutKey, platform: BrowserPlatform) -> bool {
        let operations = &self.operations;
        let deck_live = self.surface_visible && operations.folder.is_some();
        let Some(shortcut) = deck_shortcut_for(key, platform, self.spotlight_active, deck_live)
        else {
            return false;
        };
        tracing::debug!(target: "deck", ?shortcut, "deck chord");
        let focused = focused_pane_view(&operations.panes, operations.focused_pane_id.as_deref());
        match shortcut {
            DeckShortcut::ClearSpotlight => operations
                .pump
                .dispatch(ClientEvent::Deck(DeckIntent::ClearSpotlight)),
            DeckShortcut::FocusAdjacent(direction) => {
                if let Some(target) =
                    focused.and_then(|source| adjacent_pane(&operations.panes, source, direction))
                {
                    operations.focus_pane(target.pane_id.clone());
                }
            }
            DeckShortcut::NewTerminal => {
                operations.new_tab(operations.focused_pane_id.clone().unwrap_or_default())
            }
            DeckShortcut::TerminalTab(digit) => {
                if let Some(session_id) = focused.and_then(|pane| tab_for_digit(pane, digit)) {
                    operations.select(session_id.to_owned());
                }
            }
            DeckShortcut::Split(direction) => operations.split(direction),
            DeckShortcut::Spotlight => operations.spotlight(),
            DeckShortcut::Arrange(kind) => operations.arrange(kind),
        }
        true
    }
}

/// Install the chord listener once; every render hands it the newest state.
pub fn use_deck_chords(state: DeckChordState) {
    let latest: Rc<RefCell<Option<DeckChordState>>> = use_hook(Rc::default);
    *latest.borrow_mut() = Some(state);
    use_hook(move || {
        let platform = current_platform();
        Rc::new(Listeners::document_keys(move |key| {
            let Some(state) = latest.borrow().clone() else {
                return false;
            };
            state.run_chord(key, platform)
        }))
    });
}

#[cfg(target_arch = "wasm32")]
fn current_platform() -> BrowserPlatform {
    crate::platform::browser_platform::browser_platform()
}

#[cfg(not(target_arch = "wasm32"))]
fn current_platform() -> BrowserPlatform {
    BrowserPlatform::Other
}
