//! What the composer's role switcher shows and saves, as pure functions of the
//! agent settings: each model role's running model for this chat's folder,
//! where it comes from, and the settings after a new model leads a role.
//! The component is `role_switcher`; scope rules are `settings::model_scope`.

use roost_protocol::wire::agent_chat::{AgentSettingsView, ModelsCatalog};

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
    /// The model that leads the role's chain, by name, or what it inherits.
    pub(super) model: String,
    /// The leading selector without its thinking suffix, for the picker's mark.
    pub(super) selector: Option<String>,
    /// This folder overrides the all-folders chain for the role.
    pub(super) folder_override: bool,
}

/// Every chat model role, in the settings pane's order. Web search picks
/// engines, not models, and stays in Settings.
pub(super) fn role_lines(
    settings: &AgentSettingsView,
    folder: &ModelScope,
    catalog: Option<&ModelsCatalog>,
) -> Vec<RoleLine> {
    let levels = catalog.map_or(&[][..], |catalog| catalog.thinking_levels.as_slice());
    ROLES
        .iter()
        .filter(|(role, ..)| *role != "web")
        .map(|&(role, title, _, inherits)| {
            let leader = role_value(settings, folder, role)
                .and_then(|value| parse_chain(&value).into_iter().next());
            let (model, selector) = match leader {
                Some(leader) => {
                    let (selector, level) = split_thinking(&leader, levels);
                    let name = model_name(catalog, selector);
                    let model = match level {
                        Some(level) => format!("{name} · {level}"),
                        None => name,
                    };
                    (model, Some(selector.to_owned()))
                }
                None => (inherited_text(inherits), None),
            };
            RoleLine {
                role,
                title,
                model,
                selector,
                folder_override: role_source(settings, folder, role) == RoleSource::Folder,
            }
        })
        .collect()
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
