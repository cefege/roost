//! Compact repeat-mode status and the action that disables it.
//! The chat surface mounts this beside the composer from the runtime's typed
//! LoopStatus item; changing the mode uses the shared command submit path.

use super::composer_commands::submit_direct_command;
use crate::pump::Pump;
use dioxus::prelude::*;
use libero::components::{Badge, Button, Flex, Pictogram, Text};
use libero::sx::sx;

#[component]
pub(super) fn LoopControls(status: String, conversation_id: String, pump: Pump) -> Element {
    let mut pending = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let complete = EventHandler::new(move |result: Result<(), String>| {
        pending.set(false);
        error.set(result.err());
    });
    let controls_sx = use_hook(|| {
        sx().display("flex")
            .flex_wrap("wrap")
            .align_items("center")
            .gap("sm")
    });
    let error_sx = use_hook(|| {
        sx().with("flex-basis", "100%")
            .font_size("sm")
            .color("error")
    });
    rsx! {
        Flex { direction: "row", sx: controls_sx, "data-testid": "agent-loop-controls",
            Text { role: "status", title: "Repeats the request until its limit, condition or Stop. Turn off to stop repeating.",
                Badge { variant: "tonal",
                    Pictogram { icon: pictogram_icons_lucide::repeat::outlined }
                    "{status}"
                }
            }
            Button {
                variant: "text",
                disabled: pending(),
                "aria-label": "Turn off repeat mode",
                title: "Turn off repeat mode",
                "aria-busy": pending().then_some("true"),
                "data-testid": "agent-loop-disable",
                onclick: move |_| {
                    if !pending() {
                        pending.set(true);
                        error.set(None);
                        submit_direct_command(pump.clone(), conversation_id.clone(), "/loop off".into(), complete);
                    }
                },
                Pictogram { icon: pictogram_icons_lucide::x::outlined }
            }
            if let Some(error) = error() {
                Text { sx: error_sx, "{error}" }
            }
        }
    }
}
