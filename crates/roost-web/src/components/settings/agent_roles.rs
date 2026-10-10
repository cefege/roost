//! Model-role selectors and the default advisor setting for agent chat.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{AgentSettingsView, ModelsCatalog};

use crate::components::agent_chat::{ToolbarMenu, ToolbarMenuItem, ToolbarTrigger};
use crate::components::md::SwitchRow;
use crate::pump::{Pump, use_store};
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::{GetAgentSettings, SetAgentSettings};

const ROLES: [&str; 8] = [
    "default", "smol", "slow", "plan", "task", "tiny", "judge", "advisor",
];

/// Edits the saved model selector for each harness role.
#[component]
pub fn AgentRoles(catalog: ModelsCatalog) -> Element {
    let pump = use_store();
    let error = use_signal(String::new);
    let load_pump = pump.clone();
    let state = use_signal(AgentSettingsView::default);
    use_effect(move || load_settings(load_pump.clone(), state, error));
    let current = state();
    rsx! {
        div { class: "agent-settings__section", "data-testid": "agent-model-roles",
            h2 { class: "agent-settings__section-title", "Model roles" }
            for role in ROLES {
                {role_row(role, current.clone(), catalog.clone(), pump.clone(), state, error)}
            }
            SwitchRow {
                test_id: "agent-advisor-default",
                headline: "Advisor reviews every turn by default",
                support: Some("Conversations use the advisor unless individually overridden.".to_owned()),
                checked: current.advisor_enabled,
                on_change: move |enabled| {
                    let mut updated = current.clone(); updated.advisor_enabled = enabled;
                    save_settings(pump.clone(), updated, state, error);
                },
            }
            if !error().is_empty() { p { class: "md-body-s", role: "alert", {error()} } }
        }
    }
}

fn role_row(
    role: &'static str,
    settings: AgentSettingsView,
    catalog: ModelsCatalog,
    pump: Pump,
    state: Signal<AgentSettingsView>,
    error: Signal<String>,
) -> Element {
    let selected = settings.model_roles.get(role).cloned();
    let mut items = vec![ToolbarMenuItem::choice(
        "__default",
        "Use default",
        selected.is_none(),
    )];
    items.extend(
        catalog
            .models
            .iter()
            .filter(|model| model.available && (!model.classifier || role == "judge"))
            .map(|model| {
                let selector = format!("{}/{}", model.provider, model.model_id);
                let mut item = ToolbarMenuItem::choice(
                    selector.clone(),
                    model.name.clone(),
                    selected.as_deref() == Some(&selector),
                );
                item.detail = Some(model.provider.clone());
                item
            }),
    );
    let label = selected.as_deref().unwrap_or("Use default").to_owned();
    rsx! {
        div { class: "agent-settings__role-row", key: "{role}",
            span { class: "agent-settings__provider-name", "{role}" }
            ToolbarMenu {
                menu_key: format!("agent-role-{role}"),
                trigger: ToolbarTrigger::Picker { label },
                aria_label: format!("{role} model"),
                items,
                on_choose: move |choice| {
                    let mut updated = settings.clone();
                    if choice == "__default" { updated.model_roles.remove(role); }
                    else { updated.model_roles.insert(role.to_owned(), choice); }
                    save_settings(pump.clone(), updated, state, error);
                },
            }
        }
    }
}

fn load_settings(pump: Pump, settings: Signal<AgentSettingsView>, error: Signal<String>) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut settings = settings;
        let mut error = error;
        match pump.rpc().call(&GetAgentSettings {}).await {
            Ok(json) => match serde_json::from_str(&json) {
                Ok(value) => {
                    settings.set(value);
                    error.set(String::new());
                }
                Err(failure) => error.set(format!("Could not decode agent settings: {failure}")),
            },
            Err(failure) => error.set(format!("Could not load agent settings: {failure}")),
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, settings, error);
}

fn save_settings(
    pump: Pump,
    settings: AgentSettingsView,
    state: Signal<AgentSettingsView>,
    error: Signal<String>,
) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut state = state;
        let mut error = error;
        match serde_json::to_string(&settings) {
            Ok(settings_json) => match pump.rpc().call(&SetAgentSettings { settings_json }).await {
                Ok(()) => {
                    state.set(settings);
                    error.set(String::new());
                }
                Err(failure) => error.set(format!("Could not save model roles: {failure}")),
            },
            Err(failure) => error.set(format!("Could not encode model roles: {failure}")),
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, settings, state, error);
}
