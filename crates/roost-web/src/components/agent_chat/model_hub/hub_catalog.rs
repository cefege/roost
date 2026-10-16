//! The catalog and settings queries the models hub reads while it renders: the
//! roles-assigned count, whether a provider is signed in, the kind tabs that have
//! models, and the wrap-around index the key ladder moves a cursor with. All are
//! pure; `model_hub` calls them and `hub_browse` supplies the kind tables.

use roost_protocol::wire::agent_chat::{AgentSettingsView, ModelsCatalog};

use super::hub_browse;
use crate::components::settings::agent_roles::ROLES;

/// The roles with a non-empty saved value, and the total number of roles.
pub(super) fn roles_progress(settings: &AgentSettingsView) -> (usize, usize) {
    let assigned = ROLES
        .iter()
        .filter(|(role, _, _, _)| {
            settings
                .model_roles
                .get(*role)
                .is_some_and(|value| !value.is_empty())
        })
        .count();
    (assigned, ROLES.len())
}

pub(super) fn provider_configured(catalog: &ModelsCatalog, provider_id: &str) -> bool {
    catalog
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .is_some_and(|provider| provider.configured)
}

/// The kind tabs that have at least one model, in OMP's tab order.
pub(super) fn kinds_present(catalog: &ModelsCatalog) -> Vec<String> {
    hub_browse::MODEL_KIND_TABS
        .iter()
        .filter(|kind| {
            catalog
                .models
                .iter()
                .any(|entry| hub_browse::kind_of(entry) == **kind)
                || **kind == "chat"
                || **kind == "search"
        })
        .map(|kind| (*kind).to_owned())
        .collect()
}

pub(super) fn wrap_index(position: usize, step: i32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let len = len as i64;
    ((position as i64 + i64::from(step)).rem_euclid(len)) as usize
}
