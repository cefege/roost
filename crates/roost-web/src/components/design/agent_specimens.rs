//! `TimelineSpecimen` and `PeekSpecimen`: the agent-chat step group and peek
//! tiles, rendered from the shipped components so the gallery shows their real
//! markup. Each state is passed explicitly, so no store conversation is needed.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::TranscriptItem;

use crate::components::agent_chat::peek_tile::PeekCard;
use crate::components::agent_chat::timeline_rail::StepGroup;

/// One tool call in a sample run. Only the fields the step's mark and summary read are set.
fn sample_step(
    id: &str,
    tool_name: &str,
    args_json: &str,
    running: bool,
    queued: bool,
    is_error: bool,
) -> TranscriptItem {
    TranscriptItem::Tool {
        id: id.to_owned(),
        call_id: id.to_owned(),
        tool_name: tool_name.to_owned(),
        args_json: args_json.to_owned(),
        output: String::new(),
        is_error,
        running,
        queued,
        details_json: String::new(),
        children: Vec::new(),
    }
}

/// One run's tool calls as a step group, with a step in every mark state.
#[component]
pub fn TimelineSpecimen() -> Element {
    let steps = vec![
        sample_step(
            "read",
            "read",
            r#"{"path":"src/lib.rs"}"#,
            false,
            false,
            false,
        ),
        sample_step(
            "test",
            "bash",
            r#"{"command":"cargo test -p roost-web"}"#,
            true,
            false,
            false,
        ),
        sample_step(
            "edit",
            "edit",
            r#"{"path":"src/components/ui/mod.rs"}"#,
            false,
            true,
            false,
        ),
        sample_step(
            "clippy",
            "bash",
            r#"{"command":"cargo clippy"}"#,
            false,
            false,
            true,
        ),
    ];
    rsx! {
        div { class: "max-w-reading",
            StepGroup { steps, expanded: false }
        }
    }
}

/// The peek tile in each state: working, failed and idle.
#[component]
pub fn PeekSpecimen() -> Element {
    rsx! {
        div { class: "grid w-peek gap-3",
            PeekCard {
                title: "Refactor the bridge",
                detail: "~/Code/roost",
                status: "running",
                word: "Working",
                on_open: EventHandler::new(|()| {}),
            }
            PeekCard {
                title: "Fix CI on v3",
                detail: "exit 101: test failed",
                status: "error",
                word: "Failed",
                on_open: EventHandler::new(|()| {}),
            }
            PeekCard {
                title: "Notes on the fleet",
                detail: "~/Code/notes",
                status: "idle",
                word: "Idle",
                on_open: EventHandler::new(|()| {}),
            }
        }
    }
}
