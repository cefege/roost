//! Peek tiles: the other top-level conversations, shown beside the transcript so
//! an operator can see what else is running without leaving the chat. The model
//! is pi-pocket's peek column: each tile is a hairline card with a title, its
//! working directory, the state as a dot and word, and one primary action. The
//! conversation summary carries no transcript, so a tile shows the run's error
//! where there is one and the directory otherwise, never transcript lines.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{AgentRunState, ConversationSummary};

use super::display_conversation_title;
use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, CardContent, CardSize, CardWindow, StatusDot,
};
use crate::pump::Pump;
use crate::router_state::use_navigate;

/// How many other conversations the column shows at once.
pub const PEEK_LIMIT: usize = 3;

/// The peer conversations to peek at: top-level conversations other than the one
/// on screen, most recently updated first. Child agents are reached from the
/// inspector, so they stay out of the column.
pub fn peek_conversations(pump: &Pump, current_id: &str) -> Vec<ConversationSummary> {
    let core = pump.core();
    let core = core.borrow();
    let mut peers: Vec<ConversationSummary> = core
        .store()
        .agent_chat
        .conversations
        .values()
        .filter(|conversation| conversation.id != current_id && conversation.parent_id.is_none())
        .cloned()
        .collect();
    peers.sort_by_key(|conversation| std::cmp::Reverse(conversation.updated_ms));
    peers.truncate(PEEK_LIMIT);
    peers
}

/// The dot's status name and the word a tile prints beside it.
pub fn peek_status(state: AgentRunState) -> (&'static str, &'static str) {
    match state {
        AgentRunState::Running => ("running", "working"),
        AgentRunState::Failed => ("error", "failed"),
        AgentRunState::Idle => ("idle", "idle"),
    }
}

/// The column beside the transcript. It is hidden below the `peek` breakpoint,
/// so a phone keeps the transcript at full width.
#[component]
pub fn PeekColumn(conversations: Vec<ConversationSummary>) -> Element {
    if conversations.is_empty() {
        return rsx! {};
    }
    rsx! {
        aside {
            "data-name": "PeekColumn",
            "aria-label": "Other conversations",
            class: "hidden peek:flex w-peek shrink-0 flex-col gap-3 overflow-y-auto py-5 pr-4",
            for conversation in conversations {
                PeekTile { key: "{conversation.id}", conversation }
            }
        }
    }
}

/// One conversation from the store, mapped onto its card.
#[component]
pub fn PeekTile(conversation: ConversationSummary) -> Element {
    let navigate = use_navigate();
    let (dot, word) = peek_status(conversation.run_state);
    let href = crate::routes::agent_href(&conversation.id);
    let detail = conversation
        .error
        .clone()
        .unwrap_or_else(|| conversation.cwd.clone());
    rsx! {
        PeekCard {
            title: display_conversation_title(&conversation.title).to_owned(),
            detail,
            status: dot.to_owned(),
            word: word.to_owned(),
            on_open: EventHandler::new(move |()| navigate.call(href.clone())),
        }
    }
}

/// The card itself: the presentational half of a tile, so the `/design`
/// gallery can show every state without a store conversation.
#[component]
pub fn PeekCard(
    title: String,
    detail: String,
    status: String,
    word: String,
    on_open: EventHandler<()>,
) -> Element {
    rsx! {
        CardWindow {
            size: CardSize::Sm,
            CardContent { class: "flex min-w-0 flex-col gap-2",
                span { class: "truncate text-sm font-medium text-foreground", "{title}" }
                span { class: "truncate text-xs text-muted-foreground", "{detail}" }
                div { class: "flex items-center justify-between gap-2",
                    span { class: "flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground",
                        StatusDot { status, size: 6 }
                        "{word}"
                    }
                    Button {
                        variant: ButtonVariant::Default,
                        size: ButtonSize::Sm,
                        onclick: move |_| on_open.call(()),
                        "Open"
                    }
                }
            }
        }
    }
}
