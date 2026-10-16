//! What the composer's role switcher shows and saves, as pure functions of the
//! agent settings: each model role's running model for this chat's folder,
//! which of them this chat runs on (`ConversationSummary::running_role`), where
//! each comes from, and the settings after a new model leads a role. The
//! component is `role_switcher`; scope rules are `settings::model_scope`.

use std::collections::BTreeMap;

use roost_protocol::wire::agent_chat::{
    AgentSettingsView, ModelsCatalog, PRESET_NAME_HINT, apply_preset, valid_preset_name,
};

use crate::components::settings::agent_roles::ROLES;
use crate::components::settings::model_chain::{join_chain, parse_chain, split_thinking};
use crate::components::settings::model_scope::{
    ModelScope, RoleSource, role_source, role_value, with_role_value,
};

/// One role as the switcher lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RoleLine {
    pub(super) role: &'static str,
    pub(super) title: &'static str,
    /// What the agent uses the role for, in plain words.
    pub(super) purpose: &'static str,
    /// The model that leads the role's chain, by name, or what it inherits.
    pub(super) model: String,
    /// The leading selector without its thinking suffix, for the picker's mark.
    pub(super) selector: Option<String>,
    /// The leader's own thinking level; `None` follows the chat's level.
    pub(super) thinking: Option<String>,
    /// This folder overrides the all-folders chain for the role.
    pub(super) folder_override: bool,
    /// The role this chat's own turns run on.
    pub(super) this_chat: bool,
}

/// Every chat model role, in the settings pane's order. Web search picks
/// engines, not models, and stays in Settings.
pub(super) fn role_lines(
    settings: &AgentSettingsView,
    folder: &ModelScope,
    catalog: Option<&ModelsCatalog>,
    running: &str,
) -> Vec<RoleLine> {
    let levels = catalog.map_or(&[][..], |catalog| catalog.thinking_levels.as_slice());
    ROLES
        .iter()
        .filter(|(role, ..)| *role != "web")
        .map(|&(role, title, purpose, inherits)| {
            let leader = role_value(settings, folder, role)
                .and_then(|value| parse_chain(&value).into_iter().next());
            let (model, selector, thinking) = match leader {
                Some(leader) => {
                    let (selector, level) = split_thinking(&leader, levels);
                    let name = model_name(catalog, selector);
                    let model = match level {
                        Some(level) => format!("{name} · {level}"),
                        None => name,
                    };
                    (model, Some(selector.to_owned()), level.map(str::to_owned))
                }
                None => (inherited_text(inherits), None, None),
            };
            RoleLine {
                role,
                title,
                purpose,
                model,
                selector,
                thinking,
                folder_override: role_source(settings, folder, role) == RoleSource::Folder,
                this_chat: role == running,
            }
        })
        .collect()
}

/// Whether a session pin hides an edit to `role` from this chat. The agent
/// tries the pin before the Architect and Default chains, so moving this chat
/// onto either means clearing it; a subagent's role chain already beats it.
pub(super) fn pin_hides(role: &str, running: &str) -> bool {
    role == running && matches!(role, "plan" | "default")
}

/// Whether an edit that touches `touched` clears the chat's model pin. Only a
/// pinned chat has a pin to clear, and only an edit to the role it runs (Default
/// or Plan) changes what the chat runs.
pub(super) fn pin_clears_for(pinned: bool, running: &str, touched: &[&str]) -> bool {
    pinned && touched.iter().any(|role| pin_hides(role, running))
}

/// Where a role edit lands first: this folder when it already overrides the
/// role, otherwise all folders.
pub(super) fn initial_scope(
    settings: &AgentSettingsView,
    folder: &ModelScope,
    role: &str,
) -> ModelScope {
    if role_source(settings, folder, role) == RoleSource::Folder {
        folder.clone()
    } else {
        ModelScope::Global
    }
}

/// The settings after `selector` leads `role` in `scope`. It replaces the
/// chain's first model and keeps the fallbacks after it; a fallback that is
/// now the leader is not listed twice. A folder that had no override starts
/// from the chain it inherited.
pub(super) fn with_role_leader(
    settings: &AgentSettingsView,
    scope: &ModelScope,
    role: &str,
    selector: &str,
) -> AgentSettingsView {
    let mut chain = parse_chain(&role_value(settings, scope, role).unwrap_or_default());
    if chain.is_empty() {
        chain.push(selector.to_owned());
    } else {
        chain[0] = selector.to_owned();
    }
    let mut seen = Vec::with_capacity(chain.len());
    chain.retain(|entry| {
        let fresh = !seen.contains(entry);
        if fresh {
            seen.push(entry.clone());
        }
        fresh
    });
    with_role_value(settings, scope, role, Some(join_chain(&chain)))
}

/// The settings after `role`'s leading model in `scope` takes thinking
/// `level` (`None` drops the suffix, so the chat's own level applies). The
/// fallbacks keep theirs. Unchanged when the role has no chain of its own.
pub(super) fn with_role_thinking(
    settings: &AgentSettingsView,
    scope: &ModelScope,
    role: &str,
    level: Option<&str>,
    levels: &[String],
) -> AgentSettingsView {
    let mut chain = parse_chain(&role_value(settings, scope, role).unwrap_or_default());
    let Some(leader) = chain.first_mut() else {
        return settings.clone();
    };
    let base = split_thinking(leader, levels).0.to_owned();
    *leader = match level {
        Some(level) => format!("{base}:{level}"),
        None => base,
    };
    with_role_value(settings, scope, role, Some(join_chain(&chain)))
}

/// The thinking levels a picker offers for `selector`: `auto`, then the
/// levels the model accepts (the catalog-wide list when it names none).
/// Empty for a model that does not reason.
pub(super) fn thinking_choices(catalog: &ModelsCatalog, selector: &str) -> Vec<String> {
    let (provider, model_id) = selector.split_once('/').unwrap_or(("", selector));
    let model = catalog
        .models
        .iter()
        .find(|model| model.provider == provider && model.model_id == model_id);
    if model.is_some_and(|model| !model.reasoning) {
        return Vec::new();
    }
    let levels = match model {
        Some(model) if !model.thinking_levels.is_empty() => &model.thinking_levels,
        _ => &catalog.thinking_levels,
    };
    std::iter::once("auto".to_owned())
        .chain(levels.iter().filter(|level| *level != "auto").cloned())
        .collect()
}

/// The settings with the all-folders roles saved as preset `name`, replacing a
/// preset of that name. `Err` carries the message to show: a bad name, or no
/// role set to save.
pub(super) fn with_saved_preset(
    settings: &AgentSettingsView,
    name: &str,
) -> Result<AgentSettingsView, String> {
    if !valid_preset_name(name) {
        return Err(format!(
            "Invalid preset name \"{name}\": {PRESET_NAME_HINT}"
        ));
    }
    let roles: BTreeMap<String, String> = settings
        .model_roles
        .iter()
        .filter(|(_, chain)| !chain.trim().is_empty())
        .map(|(role, chain)| (role.clone(), chain.clone()))
        .collect();
    if roles.is_empty() {
        return Err("Set at least one all-folders role before saving a preset.".to_owned());
    }
    let mut updated = settings.clone();
    updated.model_presets.insert(name.to_owned(), roles);
    Ok(updated)
}

/// The settings after applying a preset to the all-folders roles
/// (`apply_preset`: roles it names change, the rest stay). Folder overrides
/// stay too.
pub(super) fn with_applied_preset(
    settings: &AgentSettingsView,
    preset: &BTreeMap<String, String>,
) -> AgentSettingsView {
    let mut updated = settings.clone();
    updated.model_roles = apply_preset(&settings.model_roles, preset);
    updated
}

fn model_name(catalog: Option<&ModelsCatalog>, selector: &str) -> String {
    let (provider, model_id) = selector.split_once('/').unwrap_or(("", selector));
    catalog
        .and_then(|catalog| {
            catalog
                .models
                .iter()
                .find(|model| model.provider == provider && model.model_id == model_id)
        })
        .map_or_else(|| selector.to_owned(), |model| model.name.clone())
}

fn inherited_text(inherits: &str) -> String {
    if inherits == "automatic selection" {
        "Automatic".to_owned()
    } else {
        format!("Same as {inherits}")
    }
}

#[cfg(test)]
#[path = "role_switch_tests.rs"]
mod tests;
