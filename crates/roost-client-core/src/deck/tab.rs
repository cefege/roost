//! What a deck tab id names. Pane layouts store tabs as plain strings (and
//! persist them under `roost.paneLayout.v1`), so a terminal tab is its session
//! id and a built-in agent tab is `agent:<conversation_id>`; this is the one
//! place that tells them apart. Used by the deck intents, selectors and views.

use roost_protocol::wire::SessionId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeckTab {
    Terminal(SessionId),
    Agent(String),
}

pub fn agent_tab_id(conversation_id: &str) -> String {
    format!("agent:{conversation_id}")
}

impl DeckTab {
    pub fn parse(tab_id: &str) -> Option<Self> {
        if let Some(conversation_id) = tab_id.strip_prefix("agent:") {
            return (!conversation_id.is_empty()).then(|| Self::Agent(conversation_id.to_owned()));
        }
        SessionId::try_from(tab_id.to_owned())
            .ok()
            .map(Self::Terminal)
    }

    pub fn path(&self) -> String {
        match self {
            Self::Terminal(session_id) => format!("/s/{session_id}"),
            Self::Agent(conversation_id) => format!("/a/{conversation_id}"),
        }
    }
}
