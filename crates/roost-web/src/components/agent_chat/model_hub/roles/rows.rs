//! The Roles view's rows: each visible role, then its fallback entries (the chain
//! after the leader), with the tab filter and the chat/kind separator applied.
//! Pure over `AgentSettingsView`, so the list the keys walk is the list the
//! tests check. Mounted by `roles::view`.

use roost_protocol::wire::agent_chat::AgentSettingsView;

use crate::components::settings::agent_roles::ROLES;
use crate::components::settings::model_chain::parse_chain;
use crate::components::settings::model_scope::{ModelScope, role_value};

/// The filter over the roles list, as OMP's All / Chat / Kinds tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RoleTab {
    All,
    Chat,
    Kinds,
}

impl RoleTab {
    pub(super) const ALL: [RoleTab; 3] = [RoleTab::All, RoleTab::Chat, RoleTab::Kinds];

    pub(super) fn label(self) -> &'static str {
        match self {
            RoleTab::All => "All",
            RoleTab::Chat => "Chat",
            RoleTab::Kinds => "Kinds",
        }
    }

    pub(super) fn includes(self, role: &str) -> bool {
        match self {
            RoleTab::All => true,
            RoleTab::Chat => !is_kind_role(role),
            RoleTab::Kinds => is_kind_role(role),
        }
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

/// Web search is the one non-chat role Roost runs; OMP files such roles under Kinds.
fn is_kind_role(role: &str) -> bool {
    role == "web"
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
    Separator,
    NewFallback,
}

/// A role's colour family, mapped from OMP's `model-roles.ts` tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RoleTone {
    Ok,
    Warn,
    Accent,
    Muted,
    Dim,
}

/// The tag shown before a role: Roost's title for the role, OMP's colour for it.
pub(super) fn role_tag(role: &str) -> (&'static str, RoleTone) {
    let tone = match role {
        "default" | "web" => RoleTone::Ok,
        "smol" => RoleTone::Warn,
        "slow" | "advisor" => RoleTone::Accent,
        "tiny" => RoleTone::Dim,
        _ => RoleTone::Muted,
    };
    (role_title(role), tone)
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

/// The role ids the tab shows, in `ROLES` order.
pub(super) fn visible_roles(tab: RoleTab) -> Vec<&'static str> {
    ROLES
        .iter()
        .map(|(id, ..)| *id)
        .filter(|role| tab.includes(role))
        .collect()
}

/// The chat roles the models strip assigns, in `ROLES` order.
pub(in crate::components::agent_chat::model_hub) fn chat_role_ids() -> Vec<&'static str> {
    visible_roles(RoleTab::Chat)
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

/// The rows for `tab`: each chat role then kind role with their fallbacks,
/// a separator between the two groups when both show, then `NewFallback`.
pub(super) fn build_rows(settings: &AgentSettingsView, tab: RoleTab) -> Vec<RolesRow> {
    let roles = visible_roles(tab);
    let chat: Vec<&'static str> = roles
        .iter()
        .copied()
        .filter(|role| !is_kind_role(role))
        .collect();
    let kinds: Vec<&'static str> = roles
        .iter()
        .copied()
        .filter(|role| is_kind_role(role))
        .collect();
    let mut rows = Vec::new();
    push_role_group(&mut rows, settings, &chat);
    if !chat.is_empty() && !kinds.is_empty() {
        rows.push(RolesRow::Separator);
    }
    push_role_group(&mut rows, settings, &kinds);
    rows.push(RolesRow::NewFallback);
    rows
}

fn push_role_group(rows: &mut Vec<RolesRow>, settings: &AgentSettingsView, roles: &[&'static str]) {
    for &role in roles {
        rows.push(RolesRow::Role { role });
        for (position, selector) in chain_of(settings, role).into_iter().enumerate().skip(1) {
            rows.push(RolesRow::Fallback {
                role,
                index: position,
                selector,
            });
        }
    }
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
