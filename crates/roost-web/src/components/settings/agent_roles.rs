//! Models pane → "Model roles": which model each harness role uses, in plain
//! words, plus the default advisor switch. Owns the `AgentSettingsGet`/`Set`
//! calls the pane shares, so the settings document has one loader and one
//! writer. Rendered by `agent_models`.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{AgentSettingsView, ModelsCatalog};

use crate::components::agent_chat::{ToolbarMenu, ToolbarMenuItem, ToolbarTrigger};
use crate::components::md::{StatusDot, SwitchRow};
use crate::pump::{Pump, use_store};
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::{GetAgentSettings, SetAgentSettings};

/// (role id, title, what it is used for, what runs when it is unset), in the
/// order the pane lists them. The fallbacks mirror `roost_agent::roles`.
const ROLES: [(&str, &str, &str, &str); 9] = [
    (
        "default",
        "Main",
        "New conversations start on this model.",
        "Automatic",
    ),
    (
        "smol",
        "Fast",
        "Scout subagents and quick background work.",
        "Same as Main",
    ),
    (
        "slow",
        "Thorough",
        "Reviewer subagents and the advisor.",
        "Same as Main",
    ),
    (
        "plan",
        "Plan mode",
        "Used while a conversation is in plan mode.",
        "Conversation's model",
    ),
    (
        "task",
        "Subagents",
        "Delegated task subagents.",
        "Conversation's model",
    ),
    (
        "tiny",
        "Tiny",
        "The smallest background jobs.",
        "Same as Fast",
    ),
    (
        "judge",
        "Judge",
        "Quick yes/no checks; may use a classifier model.",
        "Automatic",
    ),
    (
        "advisor",
        "Advisor",
        "Reviews each turn while the advisor is on.",
        "Same as Thorough",
    ),
    (
        "web",
        "Web search",
        "The search engine the web tools call.",
        "Default search chain",
    ),
];

const WEB_ENGINES: [(&str, &str); 16] = [
    ("web/hosted", "Hosted search"),
    ("web/exa", "Exa"),
    ("web/parallel", "Parallel"),
    ("web/firecrawl", "Firecrawl"),
    ("web/brave", "Brave"),
    ("web/tavily", "Tavily"),
    ("web/perplexity", "Perplexity"),
    ("web/jina", "Jina"),
    ("web/kagi", "Kagi"),
    ("web/searxng", "SearXNG"),
    ("web/duckduckgo", "DuckDuckGo"),
    ("web/startpage", "Startpage"),
    ("web/google", "Google"),
    ("web/ecosia", "Ecosia"),
    ("web/mojeek", "Mojeek"),
    ("web/public", "Public"),
];

const USE_DEFAULT: &str = "__default";

/// Edits the saved model selector for each harness role.
#[component]
pub fn AgentRoles(
    catalog: ModelsCatalog,
    settings: Signal<AgentSettingsView>,
    error: Signal<String>,
) -> Element {
    let pump = use_store();
    let current = settings();
    rsx! {
        for (role, title, purpose, fallback) in ROLES {
            {role_row(RoleRow { role, title, purpose, fallback }, current.clone(), &catalog, pump.clone(), settings, error)}
        }
        SwitchRow {
            test_id: "agent-advisor-default",
            headline: "Advisor reviews every turn by default",
            support: Some("Conversations use the advisor unless individually overridden.".to_owned()),
            checked: current.advisor_enabled,
            on_change: move |enabled| {
                let mut updated = current.clone(); updated.advisor_enabled = enabled;
                save_settings(pump.clone(), updated, settings, error);
            },
        }
    }
}

struct RoleRow {
    role: &'static str,
    title: &'static str,
    purpose: &'static str,
    fallback: &'static str,
}

fn role_row(
    row: RoleRow,
    settings: AgentSettingsView,
    catalog: &ModelsCatalog,
    pump: Pump,
    state: Signal<AgentSettingsView>,
    error: Signal<String>,
) -> Element {
    let RoleRow {
        role,
        title,
        purpose,
        fallback,
    } = row;
    let selected = settings.model_roles.get(role).cloned();
    let default_label = if role == "default" {
        catalog
            .default_model
            .as_ref()
            .and_then(|model| {
                catalog.models.iter().find(|entry| {
                    entry.provider == model.provider && entry.model_id == model.model_id
                })
            })
            .map_or_else(
                || fallback.to_owned(),
                |model| format!("{fallback} ({})", model.name),
            )
    } else {
        fallback.to_owned()
    };
    let mut items = vec![ToolbarMenuItem::choice(
        USE_DEFAULT,
        default_label.clone(),
        selected.is_none(),
    )];
    // A saved model whose provider is signed out still shows, so the row
    // can say what it is set to and why it will not run.
    let mut unavailable = false;
    let label = if role == "web" {
        items.extend(WEB_ENGINES.iter().map(|(selector, name)| {
            ToolbarMenuItem::choice(*selector, *name, selected.as_deref() == Some(*selector))
        }));
        selected
            .as_deref()
            .and_then(|selector| WEB_ENGINES.iter().find(|(id, _)| *id == selector))
            .map_or_else(
                || selected.clone().unwrap_or(default_label),
                |(_, name)| (*name).to_owned(),
            )
    } else {
        items.extend(
            catalog
                .models
                .iter()
                .filter(|model| model.available && (!model.classifier || role == "judge"))
                .map(|model| {
                    let selector = format!("{}/{}", model.provider, model.model_id);
                    ToolbarMenuItem {
                        detail: Some(model.provider.clone()),
                        ..ToolbarMenuItem::choice(
                            selector.clone(),
                            model.name.clone(),
                            selected.as_deref() == Some(&selector),
                        )
                    }
                }),
        );
        match selected.as_deref() {
            None => default_label,
            Some(selector) => {
                let model = catalog
                    .models
                    .iter()
                    .find(|model| format!("{}/{}", model.provider, model.model_id) == selector);
                unavailable = model.is_none_or(|model| !model.available);
                model.map_or_else(|| selector.to_owned(), |model| model.name.clone())
            }
        }
    };
    rsx! {
        div { class: "agent-settings__role-row", key: "{role}", "data-testid": "agent-role-{role}",
            div { class: "agent-settings__provider-info",
                span { class: "agent-settings__provider-name", "{title}" }
                span { class: "agent-settings__provider-id",
                    if unavailable && role == "advisor" { "Its provider is not signed in, so the advisor stays off. " }
                    else if unavailable { "Its provider is not signed in; “{fallback}” runs instead. " }
                    "{purpose}"
                }
            }
            if unavailable {
                StatusDot { status: "warn".to_owned(), title: Some("Provider not signed in".to_owned()) }
            }
            ToolbarMenu {
                menu_key: format!("agent-role-{role}"),
                trigger: ToolbarTrigger::Picker { label },
                aria_label: format!("{title} model"),
                items,
                on_choose: move |choice: String| {
                    let mut updated = settings.clone();
                    if choice == USE_DEFAULT { updated.model_roles.remove(role); }
                    else { updated.model_roles.insert(role.to_owned(), choice); }
                    save_settings(pump.clone(), updated, state, error);
                },
            }
        }
    }
}

pub fn load_settings(pump: Pump, settings: Signal<AgentSettingsView>, error: Signal<String>) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut settings = settings;
        let mut error = error;
        match pump.rpc().call(&GetAgentSettings {}).await {
            Ok(json) => match serde_json::from_str(&json) {
                Ok(value) => settings.set(value),
                Err(failure) => error.set(format!("Could not read agent settings: {failure}")),
            },
            Err(failure) => error.set(format!("Could not load agent settings: {failure}")),
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, settings, error);
}

pub fn save_settings(
    pump: Pump,
    updated: AgentSettingsView,
    state: Signal<AgentSettingsView>,
    error: Signal<String>,
) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut state = state;
        let mut error = error;
        match serde_json::to_string(&updated) {
            Ok(settings_json) => match pump.rpc().call(&SetAgentSettings { settings_json }).await {
                Ok(()) => {
                    state.set(updated);
                    error.set(String::new());
                }
                Err(failure) => error.set(format!("Could not save agent settings: {failure}")),
            },
            Err(failure) => error.set(format!("Could not encode agent settings: {failure}")),
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, updated, state, error);
}
