//! Compact repeat-mode status and the action that disables it.
//! The chat surface mounts this beside the composer from the runtime's typed
//! LoopStatus item; changing the mode uses the shared command submit path.

use dioxus::prelude::*;

use super::agent_alert::ERROR_TEXT_CLASS;
use super::composer_commands::submit_direct_command;
use crate::components::ui::{ButtonSize, Chip, IconButton, IconType};
use crate::pump::Pump;

#[component]
pub(super) fn LoopControls(status: String, conversation_id: String, pump: Pump) -> Element {
    let mut pending = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let complete = EventHandler::new(move |result: Result<(), String>| {
        pending.set(false);
        error.set(result.err());
    });
    rsx! {
        div { class: "flex flex-wrap items-center gap-2", "data-testid": "agent-loop-controls",
            span { role: "status",
                Chip {
                    label: status,
                    icon: Some(IconType::Repeat),
                    small: true,
                    title: Some("Repeats the request until its limit, condition or Stop. Turn off to stop repeating.".to_owned()),
                }
            }
            IconButton {
                icon: IconType::X,
                label: "Turn off repeat mode",
                title: "Turn off repeat mode",
                size: ButtonSize::IconSm,
                disabled: pending(),
                "aria-busy": pending().then_some("true"),
                "data-testid": "agent-loop-disable",
                onclick: move |_| {
                    if !pending() {
                        pending.set(true);
                        error.set(None);
                        submit_direct_command(pump.clone(), conversation_id.clone(), "/loop off".into(), complete);
                    }
                },
            }
            if let Some(error) = error() {
                div { class: "basis-full",
                    span { class: ERROR_TEXT_CLASS, role: "alert", "{error}" }
                }
            }
        }
    }
}
