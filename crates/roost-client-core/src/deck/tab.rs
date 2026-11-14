//! Deck tab identifiers and terminal routes.
//! Pane layouts persist terminal session IDs as plain strings.
//! Deck intents, selectors and views share this parser.

use roost_protocol::wire::SessionId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeckTab {
    Terminal(SessionId),
}

impl DeckTab {
    pub fn parse(tab_id: &str) -> Option<Self> {
        SessionId::try_from(tab_id.to_owned())
            .ok()
            .map(Self::Terminal)
    }

    pub fn path(&self) -> String {
        match self {
            Self::Terminal(session_id) => format!("/s/{session_id}"),
        }
    }
}
