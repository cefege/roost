//! Model-role editors for all folders or one known folder, plus the advisor switch.
//! `agent_models` supplies catalog and account data; this module loads the shared
//! settings document. The document pump owns ordered, optimistic writes.

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
use roost_client_core::client::rpc::calls::agent_chat::GetAgentSettings;
use rows::{RoleContext, RoleRow, role_row};

/// (role id, title, what it is for, what an unset role inherits). Stored keys in
/// `model_roles`; the composer lists them too. Titles name the native consumer, so
/// the label says where the model is used. `"automatic selection"` is matched by
/// `leader_value`, so keep that text for roles that pick their own model.
pub const ROLES: [(&str, &str, &str, &str); 9] = [
    (
        "default",
        "Default model",
        "New chats start on it. Every role you leave unset uses it too.",
        "automatic selection",
    ),
    (
        "smol",
        "Fast model",
        "Code searches, /btw side questions and conversation summaries. Pick a cheaper model to save cost.",
        "the chat's model, else Default model",
    ),
    (
        "plan",
        "Plan mode",
        "Writes the plan in plan mode, before any code changes.",
        "the chat's model, else Default model",
    ),
    (
        "task",
        "General helpers",
        "Work the agent hands to a helper.",
        "the chat's model, else Default model",
    ),
    (
        "slow",
        "Reviewer helpers",
        "Helpers that review work. The advisor uses this model when it is set.",
        "the chat's model, else Default model",
    ),
    (
        "tiny",
        "Side questions & summaries",
        "/btw and conversation summaries, when they should differ from the fast model.",
        "Fast model when set, else the chat's model",
    ),
    (
        "judge",
        "Judge",
        "Background yes/no decisions, such as choosing thinking effort. Leave unset to pick automatically.",
        "automatic selection",
    ),
    (
        "advisor",
        "Advisor model",
        "Reviews each turn while the advisor is on.",
        "Reviewer helpers, else Default model",
    ),
    (
        "web",
        "Search engine order",
        "Engines the web tools try in order.",
        "the default search chain",
    ),
];

/// The roles every user sees; the rest sit behind Advanced overrides.
pub(crate) const BASIC_ROLES: [&str; 2] = ["default", "smol"];
/// Optional per-task overrides. The advisor model sits by its switch and web
/// search in the Web search tab, so neither is listed here.
pub(crate) const ADVANCED_ROLES: [&str; 5] = ["plan", "task", "slow", "tiny", "judge"];

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
    let mut advanced = use_signal(|| false);
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
                "A folder's choice replaces the all-folders choice for that setting only."
            }
            if folders.is_empty() {
                p { class: "basis-full text-sm text-muted-foreground",
                    "Open a folder or start a conversation to customize its models."
                }
            }
        }
        for role in BASIC_ROLES {
            {role_row(role_spec(role), &context)}
        }
        Button {
            variant: ButtonVariant::Ghost,
            size: ButtonSize::Sm,
            icon: Some(if advanced() { IconType::ChevronUp } else { IconType::ChevronDown }),
            "aria-expanded": advanced().to_string(),
            "aria-controls": "agent-advanced-role-models",
            "data-testid": "agent-work-role-expand",
            title: "Separate models for planning, helpers, side questions and background decisions.",
            onclick: move |_| advanced.toggle(),
            if advanced() { "Hide advanced overrides" } else { "Advanced overrides" }
        }
        if advanced() {
            div { id: "agent-advanced-role-models", class: "flex flex-col gap-3",
                for role in ADVANCED_ROLES {
                    {role_row(role_spec(role), &context)}
                }
            }
        }
        SwitchRow {
            test_id: Some("agent-advisor-default".to_owned()),
            headline: "Advisor reviews every turn by default".to_owned(),
            support: Some("Conversations use the advisor unless individually overridden.".to_owned()),
            checked: current.advisor_enabled,
            on_checked_change: {
                let advisor_current = current.clone();
                move |enabled| {
                let mut updated = advisor_current.clone();
                updated.advisor_enabled = enabled;
                pump.save_agent_settings(updated, settings, error);
                }
            },
        }
        {role_row(role_spec("advisor"), &context)}
    }
}

/// The web tools' engine order, for all folders. Lives in the Web search tab
/// because engines are search services, not chat models.
#[component]
pub fn WebSearchOrder(
    catalog: ModelsCatalog,
    settings: Signal<AgentSettingsView>,
    error: Signal<String>,
) -> Element {
    let current = settings();
    let context = RoleContext {
        current: &current,
        scope: &ModelScope::Global,
        catalog: &catalog,
        pump: use_store(),
        settings,
        error,
    };
    role_row(role_spec("web"), &context)
}

fn role_spec(role: &str) -> RoleRow {
    let (role, title, purpose, inherits) = ROLES
        .into_iter()
        .find(|(id, ..)| *id == role)
        .unwrap_or(ROLES[0]);
    RoleRow {
        role,
        title,
        purpose,
        inherits,
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
    spawn(async move {
        let mut settings = settings;
        let mut error = error;
        pump.wait_for_agent_settings_writes().await;
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
