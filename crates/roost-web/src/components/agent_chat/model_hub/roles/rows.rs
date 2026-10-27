//! The Roles view's rows: each visible role, then its fallback entries (the chain
//! after the leader), with the tab filter and the chat/kind separator applied.
//! Pure over `AgentSettingsView`, so the list the keys walk is the list the
//! tests check. Mounted by `roles::view`.

use roost_protocol::wire::agent_chat::{AgentSettingsView, ModelsCatalog};

use crate::components::settings::agent_roles::{BASIC_ROLES, ROLES};
use crate::components::settings::model_chain::parse_chain;
use crate::components::settings::model_scope::{ModelScope, role_value};

/// The filter over the roles list: the default and fast models every user
/// needs, or the optional per-task overrides. Web search engines are not
/// models and are configured in Settings → Web search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RoleTab {
    Main,
    Advanced,
}

impl RoleTab {
    pub(super) const ALL: [RoleTab; 2] = [RoleTab::Main, RoleTab::Advanced];

    pub(super) fn label(self) -> &'static str {
        match self {
            RoleTab::Main => "Main",
            RoleTab::Advanced => "Advanced",
        }
    }

    /// The tab that lists `role`.
    pub(super) fn of(role: &str) -> RoleTab {
        if BASIC_ROLES.contains(&role) {
            RoleTab::Main
        } else {
            RoleTab::Advanced
        }
    }

    fn includes(self, role: &str) -> bool {
        role != "web" && RoleTab::of(role) == self
    }

    /// The tab `delta` steps away, wrapping like OMP's alt+←/→.
    pub(super) fn step(self, delta: isize) -> RoleTab {
        let index = RoleTab::ALL
            .iter()
            .position(|tab| *tab == self)
            .unwrap_or(0);
        let count = RoleTab::ALL.len() as isize;
        RoleTab::ALL[(index as isize + delta).rem_euclid(count) as usize]
    }
}

/// One line of the Roles list. `Role` is a role's leader; `Fallback` is a chain
/// entry at `index` (always ≥ 1); `NewFallback` adds one to the anchor role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RolesRow {
    Role {
        role: &'static str,
    },
    Fallback {
        role: &'static str,
        index: usize,
        selector: String,
    },
    NewFallback,
}

/// Roost's title for a role, from the `ROLES` table.
pub(in crate::components::agent_chat::model_hub) fn role_title(role: &str) -> &'static str {
    ROLES
        .iter()
        .find(|(id, ..)| *id == role)
        .map_or("", |(_, title, ..)| *title)
}

/// What an unset role inherits, from the `ROLES` table.
pub(super) fn role_inherits(role: &str) -> &'static str {
    ROLES
        .iter()
        .find(|(id, ..)| *id == role)
        .map_or("", |(.., inherits)| *inherits)
}

/// The role ids the tab shows: the main pair, then the overrides in `ROLES` order.
pub(super) fn visible_roles(tab: RoleTab) -> Vec<&'static str> {
    ROLES
        .iter()
        .map(|(id, ..)| *id)
        .filter(|role| tab.includes(role))
        .collect()
}

/// The chat roles the models strip assigns, in `ROLES` order.
pub(in crate::components::agent_chat::model_hub) fn chat_role_ids() -> Vec<&'static str> {
    ROLES
        .iter()
        .map(|(id, ..)| *id)
        .filter(|role| *role != "web")
        .collect()
}

/// A role's saved chain, the all-folders value only.
pub(in crate::components::agent_chat::model_hub) fn chain_of(
    settings: &AgentSettingsView,
    role: &str,
) -> Vec<String> {
    chain_in(settings, &ModelScope::Global, role)
}

/// A role's chain in `scope`: the folder's when it overrides the role, else the
/// all-folders chain.
pub(in crate::components::agent_chat::model_hub) fn chain_in(
    settings: &AgentSettingsView,
    scope: &ModelScope,
    role: &str,
) -> Vec<String> {
    parse_chain(&role_value(settings, scope, role).unwrap_or_default())
}

/// The rows for `tab`: each role with its fallbacks, then `NewFallback`.
pub(super) fn build_rows(settings: &AgentSettingsView, tab: RoleTab) -> Vec<RolesRow> {
    let mut rows = Vec::new();
    for role in visible_roles(tab) {
        rows.push(RolesRow::Role { role });
        for (position, selector) in chain_of(settings, role).into_iter().enumerate().skip(1) {
            rows.push(RolesRow::Fallback {
                role,
                index: position,
                selector,
            });
        }
    }
    rows.push(RolesRow::NewFallback);
    rows
}

/// What a role's leader reads as on its row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LeaderValue {
    /// A saved leader, shown verbatim.
    Assigned(String),
    /// Unset and inheriting automatic selection (Default).
    Auto,
    /// Unset and inheriting another role or the conversation's model.
    Unset,
}

pub(super) fn leader_value(settings: &AgentSettingsView, role: &str) -> LeaderValue {
    match chain_of(settings, role).into_iter().next() {
        Some(selector) => LeaderValue::Assigned(selector),
        None if role_inherits(role) == "automatic selection" => LeaderValue::Auto,
        None => LeaderValue::Unset,
    }
}

/// A role's saved leader as the This-chat view's saved-role line reads it.
pub(in crate::components::agent_chat::model_hub) fn saved_leader_label(
    settings: &AgentSettingsView,
    catalog: &ModelsCatalog,
    role: &str,
) -> String {
    match leader_value(settings, role) {
        LeaderValue::Assigned(selector) => {
            let (base, level) = super::edits::split_entry(catalog, &selector);
            let reference = base.split_once('/');
            let name = catalog
                .models
                .iter()
                .find(|model| reference == Some((model.provider.as_str(), model.model_id.as_str())))
                .map_or(base, |model| model.name.as_str());
            match level {
                Some(level) => format!("{name} ({level})"),
                None => name.to_owned(),
            }
        }
        LeaderValue::Auto => "Automatic".to_owned(),
        LeaderValue::Unset => "Not set".to_owned(),
    }
}
