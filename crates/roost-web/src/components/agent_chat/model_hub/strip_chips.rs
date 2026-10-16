//! The model row's role strip as pure data: the chips, whether each already
//! holds this model in the chosen scope, the settings each chip writes, and the
//! keys that drive the strip. `model_role_strip` renders it and `model_hub_actions`
//! applies it. Ported from OMP `model-hub.ts`'s strip chips, limited to Roost's roles.

use roost_protocol::wire::agent_chat::{AgentSettingsView, ModelsCatalog};

use super::hub_keys::HubKey;
use super::model_role_strip::OpenStrip;
use super::roles::{chain_in, chat_role_ids, clear_role_in, role_title, split_entry};
use super::strip_thinking::{kept_thinking_level, thinking_chips};
use crate::components::agent_chat::role_switch::{with_role_leader, with_role_thinking};
use crate::components::settings::agent_roles::role_options;
use crate::components::settings::model_scope::{ModelScope, RoleSource, role_source};

/// What a chip does when activated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum StripAction {
    /// Make the row's model the leader of this chat role.
    Assign(&'static str),
    /// Clear this chat role in the chosen scope, because it already leads with the row's model.
    Unassign(&'static str),
    /// Set the thinking level of this chat role's leading model; `None` is "Chat's level".
    Think {
        role: &'static str,
        level: Option<String>,
    },
    /// Switch this chat's model to the row's model.
    ThisChat,
}

impl StripAction {
    /// The chat role this chip edits, or `None` for the This-chat chip.
    pub(super) fn role(&self) -> Option<&'static str> {
        match self {
            Self::Assign(role) | Self::Unassign(role) | Self::Think { role, .. } => Some(*role),
            Self::ThisChat => None,
        }
    }
}

/// One chip in the strip. `assigned` drives the chip's selected (✓) state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StripChip {
    pub label: String,
    pub assigned: bool,
    pub action: StripAction,
}

/// The strip's chips: "This chat" first, so Enter on a row and Enter again
/// still switches the chat as it did before the strip, then one per chat role
/// that accepts the model. A role is assigned when `scope` leads it with the
/// model; the chip clears the role only where `scope` holds that leader itself.
pub(super) fn strip_chips(
    settings: &AgentSettingsView,
    catalog: &ModelsCatalog,
    selector: &str,
    current: Option<&str>,
    scope: &ModelScope,
) -> Vec<StripChip> {
    let mut chips = vec![StripChip {
        label: "This chat".to_owned(),
        assigned: current == Some(selector),
        action: StripAction::ThisChat,
    }];
    chips.extend(
        chat_role_ids()
            .into_iter()
            .filter(|role| {
                role_options(catalog, role)
                    .iter()
                    .any(|option| option.selector == selector)
            })
            .map(|role| {
                let assigned = chain_in(settings, scope, role)
                    .first()
                    .is_some_and(|leader| split_entry(catalog, leader).0 == selector);
                let owns_leader = *scope == ModelScope::Global
                    || role_source(settings, scope, role) == RoleSource::Folder;
                StripChip {
                    label: role_title(role).to_owned(),
                    assigned,
                    action: if assigned && owns_leader {
                        StripAction::Unassign(role)
                    } else {
                        StripAction::Assign(role)
                    },
                }
            }),
    );
    chips
}

/// The chips the open strip shows: the role's thinking levels once a role is
/// assigned, else the role chips for the row's model.
pub(super) fn strip_view_chips(
    settings: &AgentSettingsView,
    catalog: &ModelsCatalog,
    open_strip: &OpenStrip,
    current: Option<&str>,
) -> Vec<StripChip> {
    match open_strip.thinking_role {
        Some(role) => thinking_chips(
            settings,
            catalog,
            &open_strip.selector,
            &open_strip.scope,
            role,
        ),
        None => strip_chips(
            settings,
            catalog,
            &open_strip.selector,
            current,
            &open_strip.scope,
        ),
    }
}

/// The settings after a chip's edit in `scope`. `None` for the This-chat chip,
/// which changes the chat's model rather than saved roles. Assigning keeps the
/// role's fallbacks and, when the model accepts it, its thinking level;
/// unassigning clears the whole role in that scope.
pub(super) fn apply_settings(
    settings: &AgentSettingsView,
    action: &StripAction,
    selector: &str,
    scope: &ModelScope,
    catalog: &ModelsCatalog,
) -> Option<AgentSettingsView> {
    match action {
        StripAction::Assign(role) => {
            let assigned = with_role_leader(settings, scope, role, selector);
            Some(
                match kept_thinking_level(settings, scope, role, selector, catalog) {
                    Some(level) => with_role_thinking(
                        &assigned,
                        scope,
                        role,
                        Some(level.as_str()),
                        &catalog.thinking_levels,
                    ),
                    None => assigned,
                },
            )
        }
        StripAction::Unassign(role) => Some(clear_role_in(settings, scope, role)),
        StripAction::Think { role, level } => Some(with_role_thinking(
            settings,
            scope,
            role,
            level.as_deref(),
            &catalog.thinking_levels,
        )),
        StripAction::ThisChat => None,
    }
}

/// A key the open strip consumes: a cursor step, applying the chip under the
/// cursor, or closing the strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StripKey {
    Move(i32),
    Apply,
    Close,
}

/// The strip's own keys. Any other key closes the strip and then reaches the
/// hub's key ladder, so the strip never swallows a key it does not own.
pub(super) fn strip_key(key: &HubKey) -> Option<StripKey> {
    match key {
        HubKey::Left => Some(StripKey::Move(-1)),
        HubKey::Right => Some(StripKey::Move(1)),
        HubKey::Enter => Some(StripKey::Apply),
        HubKey::Escape => Some(StripKey::Close),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use roost_protocol::wire::agent_chat::ModelEntry;

    use super::*;

    fn settings_with(roles: &[(&str, &str)]) -> AgentSettingsView {
        let mut settings = AgentSettingsView::default();
        for (role, value) in roles {
            settings
                .model_roles
                .insert((*role).to_owned(), (*value).to_owned());
        }
        settings
    }

    /// A catalog of available chat models, one per selector.
    fn catalog_with(selectors: &[&str]) -> ModelsCatalog {
        let models = selectors
            .iter()
            .map(|selector| {
                let (provider, model_id) = selector.split_once('/').unwrap_or(("", selector));
                ModelEntry {
                    provider: provider.to_owned(),
                    model_id: model_id.to_owned(),
                    name: model_id.to_owned(),
                    available: true,
                    ..ModelEntry::default()
                }
            })
            .collect();
        ModelsCatalog {
            models,
            providers: Vec::new(),
            thinking_levels: Vec::new(),
            default_model: None,
            recent_models: Vec::new(),
        }
    }

    fn folder() -> ModelScope {
        ModelScope::Folder {
            worker_fp: "worker".to_owned(),
            cwd: "/work".to_owned(),
        }
    }

    #[test]
    fn this_chat_leads_then_every_chat_role_that_takes_the_model() {
        let catalog = catalog_with(&["p/a"]);
        let chips = strip_chips(
            &AgentSettingsView::default(),
            &catalog,
            "p/a",
            None,
            &ModelScope::Global,
        );
        assert_eq!(chips[0].action, StripAction::ThisChat);
        assert_eq!(chips.len(), 1 + chat_role_ids().len());
        assert!(!chips.iter().any(|chip| chip.action.role() == Some("web")));
    }

    #[test]
    fn a_model_no_chat_role_takes_gets_only_this_chat() {
        let catalog = catalog_with(&["p/a"]);
        let chips = strip_chips(
            &AgentSettingsView::default(),
            &catalog,
            "p/unknown",
            None,
            &ModelScope::Global,
        );
        assert_eq!(chips.len(), 1);
    }

    #[test]
    fn a_role_led_by_this_model_is_assigned_and_unassigns() {
        let catalog = catalog_with(&["p/a", "p/b"]);
        let settings = settings_with(&[("default", "p/a"), ("smol", "p/b")]);
        let chips = strip_chips(&settings, &catalog, "p/a", Some("p/a"), &ModelScope::Global);
        let default = chips
            .iter()
            .find(|chip| chip.action.role() == Some("default"))
            .expect("default is a chat role");
        assert!(default.assigned);
        assert_eq!(default.action, StripAction::Unassign("default"));
        let smol = chips
            .iter()
            .find(|chip| chip.action.role() == Some("smol"))
            .expect("smol is a chat role");
        assert!(!smol.assigned);
        assert_eq!(smol.action, StripAction::Assign("smol"));
        assert!(chips[0].assigned);
    }

    #[test]
    fn a_leader_with_a_thinking_suffix_counts_as_this_model() {
        let mut catalog = catalog_with(&["p/a"]);
        catalog.models[0].reasoning = true;
        catalog.models[0].thinking_levels = vec!["high".to_owned()];
        let settings = settings_with(&[("default", "p/a:high")]);
        let chips = strip_chips(&settings, &catalog, "p/a", None, &ModelScope::Global);
        let default = chips
            .iter()
            .find(|chip| chip.action.role() == Some("default"))
            .expect("default is a chat role");
        assert!(default.assigned);
    }

    #[test]
    fn an_inherited_leader_in_a_folder_assigns_rather_than_clears() {
        let catalog = catalog_with(&["p/a"]);
        let settings = settings_with(&[("default", "p/a")]);
        let chips = strip_chips(&settings, &catalog, "p/a", None, &folder());
        let default = chips
            .iter()
            .find(|chip| chip.action.role() == Some("default"))
            .expect("default is a chat role");
        assert!(default.assigned);
        assert_eq!(default.action, StripAction::Assign("default"));
    }

    #[test]
    fn this_chat_is_unassigned_when_the_chat_runs_another_model() {
        let catalog = catalog_with(&["p/a"]);
        let chips = strip_chips(
            &AgentSettingsView::default(),
            &catalog,
            "p/a",
            Some("p/z"),
            &ModelScope::Global,
        );
        assert!(!chips[0].assigned);
    }

    #[test]
    fn assign_puts_the_model_first_in_the_role() {
        let next = apply_settings(
            &settings_with(&[("smol", "q/old")]),
            &StripAction::Assign("smol"),
            "q/m",
            &ModelScope::Global,
            &catalog_with(&["q/m"]),
        )
        .expect("a role chip writes settings");
        assert_eq!(
            chain_in(&next, &ModelScope::Global, "smol")
                .first()
                .map(String::as_str),
            Some("q/m")
        );
    }

    #[test]
    fn unassign_clears_the_role() {
        let next = apply_settings(
            &settings_with(&[("default", "p/a")]),
            &StripAction::Unassign("default"),
            "p/a",
            &ModelScope::Global,
            &catalog_with(&[]),
        )
        .expect("a role chip writes settings");
        assert!(!next.model_roles.contains_key("default"));
    }

    #[test]
    fn this_chat_writes_no_settings() {
        assert!(
            apply_settings(
                &AgentSettingsView::default(),
                &StripAction::ThisChat,
                "p/a",
                &ModelScope::Global,
                &catalog_with(&[]),
            )
            .is_none()
        );
    }

    #[test]
    fn strip_keys_move_apply_and_close_and_pass_the_rest_through() {
        assert_eq!(strip_key(&HubKey::Left), Some(StripKey::Move(-1)));
        assert_eq!(strip_key(&HubKey::Up), None);
        assert_eq!(strip_key(&HubKey::Right), Some(StripKey::Move(1)));
        assert_eq!(strip_key(&HubKey::Down), None);
        assert_eq!(strip_key(&HubKey::Enter), Some(StripKey::Apply));
        assert_eq!(strip_key(&HubKey::Escape), Some(StripKey::Close));
        assert_eq!(strip_key(&HubKey::Char('a')), None);
        assert_eq!(strip_key(&HubKey::Tab), None);
    }
}
