//! Models pane → "Model roles": for each role, the ordered model chain it tries,
//! scoped to all folders or one known folder, plus the default advisor switch.
//! Owns the `AgentSettingsGet`/`Set` calls the pane shares, so the settings
//! document has one loader and one writer. Rendered by `agent_models`.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{AgentSettingsView, ModelsCatalog};

use crate::components::agent_chat::model_picker::PickerOption;
use crate::components::agent_chat::{ToolbarMenu, ToolbarMenuItem, ToolbarTrigger};
use crate::components::settings::model_scope::{
    KnownFolder, ModelScope, known_folders, scope_from_id, scope_id, scope_label,
};
use crate::components::ui::{Button, ButtonSize, ButtonVariant, IconType, SwitchRow};
use crate::pump::{Pump, use_store};

mod rows;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::{GetAgentSettings, SetAgentSettings};
use rows::{RoleContext, RoleRow, role_row};

/// (role id, title as OMP's `MODEL_ROLES` names it, what it is for, what an unset
/// role inherits). Stored keys in `model_roles`; the composer lists them too.
pub const ROLES: [(&str, &str, &str, &str); 9] = [
    (
        "default",
        "Default",
        "The main model. New chats run on it, and every role you leave unset uses it.",
        "automatic selection",
    ),
    (
        "smol",
        "Fast",
        "Quick, cheap work: scout subagents that search and read code. Tiny falls back to it.",
        "Default",
    ),
    (
        "slow",
        "Thinking",
        "Hard reasoning: reviewer subagents, and the advisor when Advisor is unset.",
        "Default",
    ),
    (
        "plan",
        "Architect",
        "Plan mode: the model that writes the plan before any code changes.",
        "the conversation's model",
    ),
    (
        "task",
        "Subtask",
        "General subagents the agent hands work to with its task tool.",
        "the conversation's model",
    ),
    (
        "tiny",
        "Tiny",
        "Housekeeping: /btw side questions and shrinking long chats (compaction).",
        "Fast",
    ),
    (
        "judge",
        "Judge",
        "Yes/no calls the agent makes itself, such as choosing the auto thinking level. Classifier models are offered only here.",
        "automatic selection",
    ),
    (
        "advisor",
        "Advisor",
        "Reviews each turn and suggests corrections while the advisor is on.",
        "Thinking",
    ),
    (
        "web",
        "Web search",
        "Engines the web tools try in order. Separate from chat models.",
        "the default search chain",
    ),
];

pub(crate) const WEB_ENGINES: [(&str, &str); 16] = [
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

/// Edits the model chain for each role, for all folders or one folder.
#[component]
pub fn AgentRoles(
    catalog: ModelsCatalog,
    settings: Signal<AgentSettingsView>,
    error: Signal<String>,
) -> Element {
    let pump = use_store();
    let mut scope = use_signal(|| ModelScope::Global);
    let mut specialized = use_signal(|| false);
    let current = settings();
    let folders = known_folders_in_store(&pump, &current);
    let active = scope();
    let scope_items = scope_menu_items(&folders, &active);
    let context = RoleContext {
        current: &current,
        scope: &active,
        catalog: &catalog,
        pump: pump.clone(),
        settings,
        error,
    };
    rsx! {
        div { class: "flex flex-wrap items-center gap-3", "data-testid": "agent-model-scope",
            ToolbarMenu {
                menu_key: "model-scope".to_owned(),
                trigger: ToolbarTrigger::Picker { label: scope_label(&active, &folders) },
                aria_label: "Models apply to".to_owned(),
                items: scope_items,
                on_choose: move |choice: String| scope.set(scope_from_id(&choice)),
            }
            p { class: "basis-full text-sm text-muted-foreground",
                "A folder's chain replaces the all-folders chain for that role only. A role with no chain inherits."
            }
            if folders.is_empty() {
                p { class: "basis-full text-sm text-muted-foreground",
                    "Open a folder or start a conversation to customize its models."
                }
            }
        }
        {role_row(RoleRow {
            role: ROLES[0].0,
            title: ROLES[0].1,
            purpose: ROLES[0].2,
            inherits: ROLES[0].3,
        }, &context)}
        Button {
            variant: ButtonVariant::Ghost,
            size: ButtonSize::Sm,
            icon: Some(if specialized() { IconType::ChevronUp } else { IconType::ChevronDown }),
            "aria-expanded": specialized().to_string(),
            "aria-controls": "agent-work-role-models",
            "data-testid": "agent-work-role-expand",
            title: "Optional overrides for planning, delegated work, quick tasks, review and search.",
            onclick: move |_| specialized.toggle(),
            if specialized() { "Hide work-role overrides" } else { "Customize by task" }
        }
        if specialized() {
            div { id: "agent-work-role-models", class: "flex flex-col gap-3",
                for (role, title, purpose, inherits) in ROLES.into_iter().skip(1) {
                    {role_row(RoleRow { role, title, purpose, inherits }, &context)}
                }
            }
        }
        SwitchRow {
            test_id: Some("agent-advisor-default".to_owned()),
            headline: "Advisor reviews every turn by default".to_owned(),
            support: Some("Conversations use the advisor unless individually overridden.".to_owned()),
            checked: current.advisor_enabled,
            on_checked_change: move |enabled| {
                let mut updated = current.clone();
                updated.advisor_enabled = enabled;
                save_settings(pump.clone(), updated, settings, error);
            },
        }
    }
}

/// Saved overrides remain reachable after their last conversation is deleted.
fn known_folders_in_store(pump: &Pump, settings: &AgentSettingsView) -> Vec<KnownFolder> {
    let core = pump.core();
    let core = core.borrow();
    let store = core.store();
    let conversations = store.agent_chat.conversations.values().map(|row| {
        (
            row.worker_fp.as_str(),
            row.cwd.as_str(),
            row.worker_label.as_str(),
        )
    });
    let workspaces = store.workspaces.values().map(|row| {
        let worker_fp = row.worker_fp.as_str();
        let label = store
            .workers
            .get(worker_fp)
            .map_or(worker_fp, |worker| worker.label.as_str());
        (worker_fp, row.folder_path.as_str(), label)
    });
    let configured = settings.folder_model_roles.iter().map(|row| {
        let label = store
            .workers
            .get(&row.worker_fp)
            .map_or(row.worker_fp.as_str(), |worker| worker.label.as_str());
        (row.worker_fp.as_str(), row.cwd.as_str(), label)
    });
    known_folders(conversations.chain(workspaces).chain(configured))
}

fn scope_menu_items(folders: &[KnownFolder], active: &ModelScope) -> Vec<ToolbarMenuItem> {
    let mut items = vec![ToolbarMenuItem::choice(
        scope_id(&ModelScope::Global),
        "All folders",
        matches!(active, ModelScope::Global),
    )];
    items.extend(folders.iter().map(|folder| {
        let scope = ModelScope::Folder {
            worker_fp: folder.worker_fp.clone(),
            cwd: folder.cwd.clone(),
        };
        ToolbarMenuItem::choice(scope_id(&scope), folder.label.clone(), &scope == active)
    }));
    items
}

/// The models a role's chain may name. Web search draws from its engines; chat
/// roles draw from signed-in, non-classifier models, and the judge also gets
/// classifier models.
pub fn role_options(catalog: &ModelsCatalog, role: &str) -> Vec<PickerOption> {
    if role == "web" {
        return WEB_ENGINES
            .iter()
            .map(|(selector, name)| PickerOption {
                selector: (*selector).to_owned(),
                name: (*name).to_owned(),
                group: "Search engines".to_owned(),
                detail: None,
            })
            .collect();
    }
    catalog
        .models
        .iter()
        .filter(|model| model.available && (!model.classifier || role == "judge"))
        .map(|model| PickerOption {
            selector: format!("{}/{}", model.provider, model.model_id),
            name: model.name.clone(),
            group: provider_name(catalog, &model.provider),
            detail: model.classifier.then(|| "Classifier".to_owned()),
        })
        .collect()
}

fn provider_name(catalog: &ModelsCatalog, provider_id: &str) -> String {
    catalog
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .map_or_else(|| provider_id.to_owned(), |provider| provider.name.clone())
}

pub fn load_settings(
    pump: Pump,
    settings: Signal<AgentSettingsView>,
    error: Signal<String>,
    mut loaded: Signal<bool>,
) {
    loaded.set(false);
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut settings = settings;
        let mut error = error;
        match pump.rpc().call(&GetAgentSettings {}).await {
            Ok(json) => match serde_json::from_str(&json) {
                Ok(value) => {
                    settings.set(value);
                    error.set(String::new());
                    loaded.set(true);
                }
                Err(failure) => error.set(format!("Could not read agent settings: {failure}")),
            },
            Err(failure) => error.set(format!("Could not load agent settings: {failure}")),
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, settings, error, loaded);
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
