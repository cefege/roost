//! The Roles view's edits. Each takes the saved settings and returns the settings
//! to save, all-folders scope only; the view hands the result to `save_settings`.
//! Chain indexes are positions in a role's stored chain: 0 is the leader, 1.. the
//! fallbacks. Pure, so every rule is tested without the RPC.

use roost_protocol::wire::agent_chat::{
    AgentSettingsView, ModelsCatalog, active_preset, presets_with_builtins,
};

use super::rows::chain_of;
use crate::components::agent_chat::role_switch::{thinking_choices, with_role_leader};
use crate::components::settings::model_chain::{join_chain, move_entry, push_entry};
use crate::components::settings::model_scope::{ModelScope, with_role_value};

/// Writes `chain` for `role`; an empty chain clears the role, so it inherits.
fn save_chain(settings: &AgentSettingsView, role: &str, chain: &[String]) -> AgentSettingsView {
    let value = (!chain.is_empty()).then(|| join_chain(chain));
    with_role_value(settings, &ModelScope::Global, role, value)
}

/// Makes `selector` the role's leader; the fallbacks stay (role_switch's rule).
pub(super) fn assign_leader(
    settings: &AgentSettingsView,
    role: &str,
    selector: &str,
) -> AgentSettingsView {
    with_role_leader(settings, &ModelScope::Global, role, selector)
}

/// Puts `selector` at chain `index`. A selector already elsewhere in the chain
/// is removed, so the chain never names a model twice. `None` when the index
/// is past the chain's end.
pub(super) fn assign_entry(
    settings: &AgentSettingsView,
    role: &str,
    index: usize,
    selector: &str,
) -> Option<AgentSettingsView> {
    if index == 0 {
        return Some(assign_leader(settings, role, selector));
    }
    let mut chain = chain_of(settings, role);
    if index >= chain.len() {
        return None;
    }
    let mut next: Vec<String> = Vec::with_capacity(chain.len());
    for (position, entry) in chain.drain(..).enumerate() {
        if position == index {
            next.push(selector.to_owned());
        } else if entry != selector {
            next.push(entry);
        }
    }
    Some(save_chain(settings, role, &next))
}

/// Appends a fallback. A role with no leader takes it as the leader instead,
/// since a chain cannot start with a fallback. An entry already present is a no-op.
pub(super) fn add_fallback(
    settings: &AgentSettingsView,
    role: &str,
    selector: &str,
) -> AgentSettingsView {
    let mut chain = chain_of(settings, role);
    if chain.is_empty() {
        return assign_leader(settings, role, selector);
    }
    if push_entry(&mut chain, selector) {
        return save_chain(settings, role, &chain);
    }
    settings.clone()
}

/// Removes a fallback at `index` (≥ 1). `None` for the leader or past the end.
pub(super) fn remove_fallback(
    settings: &AgentSettingsView,
    role: &str,
    index: usize,
) -> Option<AgentSettingsView> {
    let mut chain = chain_of(settings, role);
    if index == 0 || index >= chain.len() {
        return None;
    }
    chain.remove(index);
    Some(save_chain(settings, role, &chain))
}

/// Clears a role entirely (its leader and fallbacks), so it inherits again.
pub(super) fn clear_role(settings: &AgentSettingsView, role: &str) -> AgentSettingsView {
    clear_role_in(settings, &ModelScope::Global, role)
}

/// Clears `role` in `scope`: a folder's clear makes it inherit the all-folders chain.
pub(in crate::components::agent_chat::model_hub) fn clear_role_in(
    settings: &AgentSettingsView,
    scope: &ModelScope,
    role: &str,
) -> AgentSettingsView {
    with_role_value(settings, scope, role, None)
}

/// Moves the fallback at `index` by `step`. The leader never moves, and no
/// fallback moves into the leader's slot. `None` when the move is not possible.
pub(super) fn move_fallback(
    settings: &AgentSettingsView,
    role: &str,
    index: usize,
    step: isize,
) -> Option<AgentSettingsView> {
    let mut chain = chain_of(settings, role);
    let target = index.checked_add_signed(step)?;
    if index == 0 || target == 0 {
        return None;
    }
    if !move_entry(&mut chain, index, step) {
        return None;
    }
    Some(save_chain(settings, role, &chain))
}

/// Writes thinking `level` onto the chain entry at `index`: `None` drops the
/// suffix so the chat's own level applies. Unknown entries are left as they are.
pub(super) fn set_entry_thinking(
    settings: &AgentSettingsView,
    catalog: &ModelsCatalog,
    role: &str,
    index: usize,
    level: Option<&str>,
) -> Option<AgentSettingsView> {
    let mut chain = chain_of(settings, role);
    let entry = chain.get(index)?.clone();
    let base = split_entry(catalog, &entry).0.to_owned();
    chain[index] = match level {
        Some(level) => format!("{base}:{level}"),
        None => base,
    };
    Some(save_chain(settings, role, &chain))
}

/// The thinking level a saved entry carries, `None` when it inherits. Splits
/// only a suffix the catalog names for that model, so a colon in an id survives.
pub(in crate::components::agent_chat::model_hub) fn split_entry<'a>(
    catalog: &ModelsCatalog,
    entry: &'a str,
) -> (&'a str, Option<&'a str>) {
    if let Some((base, level)) = entry.rsplit_once(':')
        && thinking_choices(catalog, base)
            .iter()
            .any(|choice| choice == level)
    {
        return (base, Some(level));
    }
    (entry, None)
}

/// The next thinking level in the cycle `None` → each of `choices` → `None`.
/// `choices` is `thinking_choices` for the model: `auto`, then its levels.
pub(super) fn next_thinking(current: Option<&str>, choices: &[String]) -> Option<String> {
    let mut cycle: Vec<Option<&str>> = vec![None];
    cycle.extend(choices.iter().map(|choice| Some(choice.as_str())));
    let position = cycle.iter().position(|slot| *slot == current).unwrap_or(0);
    cycle[(position + 1) % cycle.len()].map(str::to_owned)
}

/// The preset names the picker offers: built-in and saved presets together, in the
/// key order of `presets_with_builtins`, which is sorted by name.
pub(super) fn preset_names(settings: &AgentSettingsView) -> Vec<String> {
    presets_with_builtins(&settings.model_presets)
        .keys()
        .cloned()
        .collect()
}

/// The preset the setup matches, if any.
pub(super) fn active_preset_name(settings: &AgentSettingsView) -> Option<String> {
    let presets = presets_with_builtins(&settings.model_presets);
    active_preset(&presets, &settings.model_roles).map(str::to_owned)
}

/// The name of the preset `delta` steps from the active one (wrapping; from a
/// custom setup, +1 gives the first and -1 the last).
pub(super) fn cycle_preset(settings: &AgentSettingsView, delta: isize) -> Option<String> {
    let names = preset_names(settings);
    if names.is_empty() {
        return None;
    }
    let count = names.len() as isize;
    let current = active_preset_name(settings)
        .and_then(|active| names.iter().position(|name| *name == active));
    let next = match current {
        Some(index) => (index as isize + delta).rem_euclid(count) as usize,
        None if delta > 0 => 0,
        None => names.len() - 1,
    };
    Some(names[next].clone())
}
