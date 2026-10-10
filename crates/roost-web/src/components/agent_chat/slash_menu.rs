//! Prefix-filtered slash commands shown above the agent composer.
//! The composer owns keyboard navigation and completion; this component only
//! presents the shared protocol registry as a Material list.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{AGENT_SLASH_COMMANDS, SlashCommand};

use crate::components::md::{List, ListRow};

#[component]
pub fn SlashMenu(
    query: String,
    selected: usize,
    on_choose: EventHandler<&'static SlashCommand>,
) -> Element {
    let commands: Vec<_> = AGENT_SLASH_COMMANDS
        .iter()
        .filter(|command| {
            let prefix = query.trim_start_matches('/');
            command.name.starts_with(prefix)
                || command
                    .aliases
                    .iter()
                    .any(|alias| alias.starts_with(prefix))
        })
        .collect();
    if commands.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "agent-chat__slash-menu", role: "listbox", aria_label: "Slash commands",
            List { contained: true,
                for (idx, command) in commands.iter().enumerate() {
                    ListRow {
                        key: "{command.name}",
                        leading_icon: Some("terminal".to_string()),
                        headline: rsx! { span { "/{command.name} {command.args}" } },
                        support: Some(rsx! { span { "{command.summary}" } }),
                        selected: idx == selected,
                        dense: true,
                        onclick: Some(EventHandler::new({
                            let command = *command;
                            move |_| on_choose.call(command)
                        })),
                    }
                }
            }
        }
    }
}
