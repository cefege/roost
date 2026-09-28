//! What a deck-issued spawn becomes once the coordinator answers: a new tab
//! lands in the pane it was opened from, a split lands beside its pane, and a
//! refusal is an error card that commits no layout and asks for no route.
//! Called by the web deck's spawn flow, whose events `handle_event` applies.
//! Ports the answer handling of `newTab` and `split` in
//! `apps/web/src/components/deck/terminal-deck-operations.ts`.

use roost_protocol::layout::document::LayoutDirection;

use super::intent::{DeckFolder, DeckIntent};
use crate::event::ClientEvent;
use crate::store::shell_intent::ShellIntent;

/// Why the deck asked for a terminal.
#[derive(Debug, Clone, PartialEq)]
pub enum DeckSpawn {
    /// The strip's +, the filler's double-click, a phone bar's + or swipe.
    NewTab {
        /// The pane it opens in.
        pane_id: String,
    },
    /// A split chord: the new terminal takes a new pane after this one.
    Split {
        /// The pane that splits.
        pane_id: String,
        /// The axis of the split.
        direction: LayoutDirection,
    },
}

impl DeckSpawn {
    /// The pane the spawn was asked from.
    pub fn pane_id(&self) -> &str {
        match self {
            Self::NewTab { pane_id } | Self::Split { pane_id, .. } => pane_id,
        }
    }

    /// A short name for the incident log.
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::NewTab { .. } => "new_tab",
            Self::Split { .. } => "split",
        }
    }

    /// The error card a refused spawn raises. It is the whole answer: the
    /// arrangement and the route stay as they were.
    pub fn refused(&self, error: &str) -> ClientEvent {
        let prefix = match self {
            Self::NewTab { .. } => "New terminal failed",
            Self::Split { .. } => "Split terminal failed",
        };
        ClientEvent::Shell(ShellIntent::ActionFailed {
            message: format!("{prefix}: {error}"),
        })
    }

    /// The intent that lands an admitted spawn once its row is live in
    /// `folder`.
    pub fn landed(self, folder: DeckFolder, session_id: String, compact: bool) -> DeckIntent {
        match self {
            Self::NewTab { pane_id } => DeckIntent::OpenSpawned {
                folder,
                pane_id,
                session_id,
                compact,
            },
            Self::Split { pane_id, direction } => DeckIntent::SplitPane {
                folder,
                pane_id,
                direction,
                tab_id: session_id,
                insert_first: false,
            },
        }
    }
}
