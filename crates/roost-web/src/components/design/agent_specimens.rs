//! `TimelineSpecimen` and `PeekSpecimen`: the agent-chat timeline rail and peek
//! tiles, rendered from the shipped components so the gallery shows their real
//! markup. Each state is passed explicitly, so no store conversation is needed.

use dioxus::prelude::*;

use crate::components::agent_chat::peek_tile::PeekCard;
use crate::components::agent_chat::timeline_rail::{StepMark, TimelineRail, TimelineStep};

const STEP_TEXT_CLASS: &str = "min-w-0 truncate font-mono text-xs text-foreground";
const QUEUED_TEXT_CLASS: &str = "min-w-0 truncate font-mono text-xs text-muted-foreground";

/// One run's tool steps on the rail, in every mark state.
#[component]
pub fn TimelineSpecimen() -> Element {
    rsx! {
        div { class: "max-w-reading",
            TimelineRail {
                TimelineStep { mark: StepMark::Done,
                    p { class: STEP_TEXT_CLASS, "read_file · src/lib.rs" }
                }
                TimelineStep { mark: StepMark::Running,
                    p { class: STEP_TEXT_CLASS, "bash · cargo test -p roost-web" }
                }
                TimelineStep { mark: StepMark::Queued,
                    p { class: QUEUED_TEXT_CLASS, "edit · src/components/ui/mod.rs" }
                }
                TimelineStep { mark: StepMark::Failed,
                    p { class: STEP_TEXT_CLASS, "bash · cargo clippy (exit 1)" }
                }
            }
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
                word: "working",
                on_open: EventHandler::new(|()| {}),
            }
            PeekCard {
                title: "Fix CI on v3",
                detail: "exit 101: test failed",
                status: "error",
                word: "failed",
                on_open: EventHandler::new(|()| {}),
            }
            PeekCard {
                title: "Notes on the fleet",
                detail: "~/Code/notes",
                status: "idle",
                word: "idle",
                on_open: EventHandler::new(|()| {}),
            }
        }
    }
}
