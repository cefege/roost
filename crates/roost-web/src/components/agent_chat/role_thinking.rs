//! The thinking level a role's leading model runs at, as filter chips in the
//! composer's role editor: "Chat's level" (no suffix, so `/effort` decides),
//! Auto, then the levels the model accepts. Saved as the leader's `:level`
//! suffix through `role_switch::with_role_thinking`. Mounted by `role_switcher`.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{AgentSettingsView, ModelsCatalog};

use super::model_controls::thinking_label;
use super::role_switch::{RoleLine, thinking_choices, with_role_thinking};
use crate::components::md::Chip;
use crate::components::settings::agent_roles::save_settings;
use crate::components::settings::model_scope::ModelScope;
use crate::pump::Pump;

#[component]
pub(super) fn RoleThinking(
    line: RoleLine,
    catalog: ModelsCatalog,
    scope: ModelScope,
    pump: Pump,
    settings: Signal<AgentSettingsView>,
    error: Signal<String>,
) -> Element {
    let Some(selector) = line.selector.clone() else {
        return rsx! {
            p { class: "agent-chat__roles-note", "Choose a model for this role to set its thinking level." }
        };
    };
    let choices = thinking_choices(&catalog, &selector);
    if choices.is_empty() {
        return rsx! {
            p { class: "agent-chat__roles-note", "This model does not think before answering." }
        };
    }
    let levels = catalog.thinking_levels.clone();
    let options = std::iter::once(None).chain(choices.into_iter().map(Some));
    rsx! {
        div { class: "agent-chat__roles-thinking", role: "group", "aria-label": "Thinking level",
            span { class: "agent-chat__roles-label", "Thinking" }
            for level in options {
                {
                    let selected = line.thinking == level;
                    let label = level.as_deref().map_or_else(|| "Chat's level".to_owned(), thinking_label);
                    let title = match level.as_deref() {
                        None => "Follow this chat's own thinking level (/effort)".to_owned(),
                        Some("auto") => "Pick a level per request from how open-ended it is".to_owned(),
                        Some(other) => format!("Always think at {other}"),
                    };
                    let pump = pump.clone();
                    let scope = scope.clone();
                    let levels = levels.clone();
                    let role = line.role;
                    rsx! {
                        Chip {
                            key: "{label}",
                            label,
                            small: true,
                            selected: Some(selected),
                            icon: selected.then(|| "check".to_owned()),
                            title: Some(title),
                            onclick: Some(EventHandler::new(move |_| {
                                let updated = with_role_thinking(&settings.peek(), &scope, role, level.as_deref(), &levels);
                                save_settings(pump.clone(), updated, settings, error);
                            })),
                        }
                    }
                }
            }
        }
    }
}
