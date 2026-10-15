//! What the models hub shows, as pure functions of the catalog and settings:
//! the sidebar views, each view's picker rows with their model facts, and the
//! `/model` argument completions. `ModelHub` and the slash menu render these.

use roost_protocol::wire::agent_chat::{
    AgentSettingsView, ConversationSummary, ModelEntry, ModelsCatalog, model_search_text,
    rank_matches,
};

use crate::components::agent_chat::model_picker::PickerOption;
use crate::components::settings::model_chain::split_thinking;

/// How many models `/model <partial>` offers.
const MODEL_COMPLETIONS: usize = 8;

/// Role selectors `/model @…` completes to.
const ROLE_ALIASES: [&str; 8] = [
    "@default", "@smol", "@slow", "@plan", "@task", "@tiny", "@judge", "@advisor",
];

/// One sidebar entry of the hub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HubView {
    Recent,
    Roles,
    All,
    Provider(String),
}

impl HubView {
    /// A stable id for keys and test ids.
    pub fn id(&self) -> String {
        match self {
            Self::Recent => "recent".to_owned(),
            Self::Roles => "roles".to_owned(),
            Self::All => "all".to_owned(),
            Self::Provider(id) => format!("provider:{id}"),
        }
    }
}

/// A sidebar row: the view, its label, how many models it lists, and whether
/// its provider still needs a sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubSection {
    pub view: HubView,
    pub label: String,
    pub count: Option<usize>,
    pub locked: bool,
}

fn is_chat(entry: &ModelEntry) -> bool {
    !entry.classifier
}

fn is_available_chat(entry: &ModelEntry) -> bool {
    entry.available && is_chat(entry)
}

fn recent_entries(catalog: &ModelsCatalog) -> Vec<&ModelEntry> {
    catalog
        .recent_models
        .iter()
        .filter_map(|recent| {
            catalog.models.iter().find(|entry| {
                entry.provider == recent.provider && entry.model_id == recent.model_id
            })
        })
        .filter(|entry| is_available_chat(entry))
        .collect()
}

/// Recent (when any), Roles, All models, signed-in providers, then providers
/// still to sign in to, each in catalog order. Providers without chat models
/// are left out.
pub fn hub_sections(catalog: &ModelsCatalog) -> Vec<HubSection> {
    let mut sections = Vec::new();
    let recent = recent_entries(catalog).len();
    if recent > 0 {
        sections.push(HubSection {
            view: HubView::Recent,
            label: "Recent".to_owned(),
            count: Some(recent),
            locked: false,
        });
    }
    sections.push(HubSection {
        view: HubView::Roles,
        label: "Roles".to_owned(),
        count: None,
        locked: false,
    });
    sections.push(HubSection {
        view: HubView::All,
        label: "All models".to_owned(),
        count: Some(
            catalog
                .models
                .iter()
                .filter(|entry| is_available_chat(entry))
                .count(),
        ),
        locked: false,
    });
    let model_providers = catalog
        .providers
        .iter()
        .filter(|provider| provider.category == "model");
    for configured in [true, false] {
        for provider in model_providers
            .clone()
            .filter(|provider| provider.configured == configured)
        {
            let count = catalog
                .models
                .iter()
                .filter(|entry| entry.provider == provider.id)
                .filter(|entry| {
                    if configured {
                        is_available_chat(entry)
                    } else {
                        is_chat(entry)
                    }
                })
                .count();
            if count > 0 {
                sections.push(HubSection {
                    view: HubView::Provider(provider.id.clone()),
                    label: provider.name.clone(),
                    count: Some(count),
                    locked: !configured,
                });
            }
        }
    }
    sections
}

/// Recent when it lists anything, else All models.
pub fn initial_view(catalog: &ModelsCatalog) -> HubView {
    if recent_entries(catalog).is_empty() {
        HubView::All
    } else {
        HubView::Recent
    }
}

/// The picker rows of a view; Roles has none (it shows the role editor).
pub fn hub_options(
    catalog: &ModelsCatalog,
    view: &HubView,
    settings: &AgentSettingsView,
) -> Vec<PickerOption> {
    let entries: Vec<&ModelEntry> = match view {
        HubView::Roles => Vec::new(),
        HubView::Recent => recent_entries(catalog),
        HubView::All => catalog
            .models
            .iter()
            .filter(|entry| is_available_chat(entry))
            .collect(),
        HubView::Provider(id) => catalog
            .models
            .iter()
            .filter(|entry| entry.provider == *id && is_available_chat(entry))
            .collect(),
    };
    entries
        .into_iter()
        .map(|entry| {
            let selector = format!("{}/{}", entry.provider, entry.model_id);
            let mut detail = model_facts(entry);
            let roles = assigned_roles(settings, &selector, &catalog.thinking_levels);
            if !roles.is_empty() {
                detail.push_str(&format!(" · roles: {}", roles.join(", ")));
            }
            PickerOption {
                group: if *view == HubView::Recent {
                    "Recent".to_owned()
                } else {
                    provider_display_name(catalog, &entry.provider)
                },
                selector,
                name: entry.name.clone(),
                detail: Some(detail),
            }
        })
        .collect()
}

/// `anthropic/claude-sonnet-5-5 · 1M ctx · $2/10 · thinking · images`.
pub fn model_facts(entry: &ModelEntry) -> String {
    let mut parts = vec![format!("{}/{}", entry.provider, entry.model_id)];
    if entry.context_window > 0 {
        parts.push(format!("{} ctx", format_tokens(entry.context_window)));
    }
    if let Some(cost) = format_cost_pair(entry.input_cost_micros, entry.output_cost_micros) {
        parts.push(cost);
    }
    if entry.reasoning {
        parts.push("thinking".to_owned());
    }
    if entry.images {
        parts.push("images".to_owned());
    }
    parts.join(" · ")
}

/// `1M`, `1.5M`, `200k`, `512`.
pub fn format_tokens(count: u64) -> String {
    if count >= 1_000_000 {
        let millions = format!("{:.1}", count as f64 / 1e6);
        format!("{}M", millions.strip_suffix(".0").unwrap_or(&millions))
    } else if count >= 1_000 {
        format!("{}k", (count + 500) / 1000)
    } else {
        count.to_string()
    }
}

/// `$input/output` in USD per million tokens; `None` when both are unknown.
pub fn format_cost_pair(input_micros: u64, output_micros: u64) -> Option<String> {
    if input_micros == 0 && output_micros == 0 {
        return None;
    }
    Some(format!(
        "${}/{}",
        cost_leg(input_micros),
        cost_leg(output_micros)
    ))
}

fn cost_leg(micros: u64) -> String {
    let usd = micros as f64 / 1e6;
    let text = if usd >= 100.0 {
        format!("{usd:.0}")
    } else if usd >= 10.0 {
        format!("{usd:.1}")
    } else if usd >= 0.01 {
        format!("{usd:.2}")
    } else {
        format!("{usd:.4}")
    };
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        text
    }
}

/// All-folders roles whose chain starts with `selector` (any `:level` ignored).
pub fn assigned_roles(
    settings: &AgentSettingsView,
    selector: &str,
    levels: &[String],
) -> Vec<String> {
    settings
        .model_roles
        .iter()
        .filter(|(_, chain)| {
            let first = chain.split(',').next().unwrap_or_default().trim();
            split_thinking(first, levels).0 == selector
        })
        .map(|(role, _)| role.clone())
        .collect()
}

/// `/model <partial>` completions as `(value, label)`: role aliases after `@`,
/// nothing once a `:level` is typed, else the best fuzzy matches among
/// `models` (`(selector, name)`, catalog order).
pub fn model_argument_values(models: &[(String, String)], partial: &str) -> Vec<(String, String)> {
    if partial.contains(':') {
        return Vec::new();
    }
    if partial.starts_with('@') {
        return ROLE_ALIASES
            .iter()
            .filter(|alias| alias.starts_with(partial))
            .map(|alias| ((*alias).to_owned(), "Role".to_owned()))
            .collect();
    }
    let haystacks: Vec<String> = models
        .iter()
        .map(|(selector, name)| model_search_text(selector, name))
        .collect();
    rank_matches(&haystacks, partial)
        .into_iter()
        .take(MODEL_COMPLETIONS)
        .map(|index| models[index].clone())
        .collect()
}

/// Every available model, grouped under its provider's name, plus the session's
/// own model when the catalog no longer lists it as available, so the picker
/// can always show what is in use.
pub fn model_picker_options(
    catalog: Option<&ModelsCatalog>,
    conversation: &ConversationSummary,
) -> Vec<PickerOption> {
    let mut options: Vec<PickerOption> = catalog
        .map(|catalog| {
            catalog
                .models
                .iter()
                .filter(|model| is_available_chat(model))
                .map(|model| PickerOption {
                    selector: format!("{}/{}", model.provider, model.model_id),
                    name: model.name.clone(),
                    group: provider_display_name(catalog, &model.provider),
                    detail: Some(model.model_id.clone()),
                })
                .collect()
        })
        .unwrap_or_default();
    if let Some(current) = conversation.model.as_ref()
        && !catalog.is_some_and(|catalog| {
            catalog.models.iter().any(|entry| {
                entry.classifier
                    && entry.provider == current.provider
                    && entry.model_id == current.model_id
            })
        })
    {
        let selector = format!("{}/{}", current.provider, current.model_id);
        if !options.iter().any(|option| option.selector == selector) {
            options.insert(
                0,
                PickerOption {
                    selector,
                    name: current.model_id.clone(),
                    group: "Current".to_string(),
                    detail: None,
                },
            );
        }
    }
    options
}

pub fn provider_display_name(catalog: &ModelsCatalog, provider_id: &str) -> String {
    catalog
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .map_or_else(|| provider_id.to_string(), |provider| provider.name.clone())
}
