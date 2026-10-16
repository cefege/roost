//! The thinking step after a role assignment: "Chat's level", `auto`, then the
//! levels the model accepts, one chip each for the role just assigned. It also
//! decides which explicit level an assignment keeps. Ported from OMP's
//! thinking strip; `role_thinking` is the settings-page equivalent.

use std::iter::once;

use roost_protocol::wire::agent_chat::{AgentSettingsView, ModelsCatalog};

use super::model_role_strip::OpenStrip;
use super::roles::chain_in;
use super::strip_chips::{StripAction, StripChip};
use crate::components::agent_chat::model_controls::thinking_label;
use crate::components::agent_chat::role_switch::thinking_choices;
use crate::components::settings::model_chain::split_thinking;
use crate::components::settings::model_scope::ModelScope;

/// The thinking options for `selector`: `None` is "Chat's level", which drops
/// the role's suffix, then each choice the model takes.
fn thinking_options(catalog: &ModelsCatalog, selector: &str) -> Vec<Option<String>> {
    once(None)
        .chain(thinking_choices(catalog, selector).into_iter().map(Some))
        .collect()
}

/// The explicit thinking level `role` takes in `scope` when `selector` accepts
/// it. Assigning the model keeps that level; a model that rejects it drops it.
pub(super) fn kept_thinking_level(
    settings: &AgentSettingsView,
    scope: &ModelScope,
    role: &str,
    selector: &str,
    catalog: &ModelsCatalog,
) -> Option<String> {
    let leader = chain_in(settings, scope, role).into_iter().next()?;
    let (_, level) = split_thinking(&leader, &catalog.thinking_levels);
    let level = level?;
    thinking_choices(catalog, selector)
        .iter()
        .any(|choice| choice == level)
        .then(|| level.to_owned())
}

/// The thinking chips for `role` in `scope`, each applying its level to the
/// role's leading model. The chip matching the role's current level is selected.
pub(super) fn thinking_chips(
    settings: &AgentSettingsView,
    catalog: &ModelsCatalog,
    selector: &str,
    scope: &ModelScope,
    role: &'static str,
) -> Vec<StripChip> {
    let current = chain_in(settings, scope, role)
        .into_iter()
        .next()
        .and_then(|leader| {
            split_thinking(&leader, &catalog.thinking_levels)
                .1
                .map(str::to_owned)
        });
    thinking_options(catalog, selector)
        .into_iter()
        .map(|level| {
            let assigned = level == current;
            StripChip {
                label: level
                    .as_deref()
                    .map_or_else(|| "Chat's level".to_owned(), thinking_label),
                assigned,
                action: StripAction::Think { role, level },
            }
        })
        .collect()
}

/// The thinking step that follows `role` taking `selector` in `scope`, or
/// `None` when the model takes no thinking level. The cursor starts on the
/// level the assignment kept, else on "Chat's level".
pub(super) fn thinking_step_after_assign(
    settings_before: &AgentSettingsView,
    catalog: &ModelsCatalog,
    selector: &str,
    scope: &ModelScope,
    role: &'static str,
) -> Option<OpenStrip> {
    if thinking_choices(catalog, selector).is_empty() {
        return None;
    }
    let kept = kept_thinking_level(settings_before, scope, role, selector, catalog);
    let cursor = thinking_options(catalog, selector)
        .iter()
        .position(|level| level.as_deref() == kept.as_deref())
        .unwrap_or(0);
    Some(OpenStrip {
        selector: selector.to_owned(),
        cursor,
        scope: scope.clone(),
        thinking_role: Some(role),
    })
}

#[cfg(test)]
mod tests {
    use roost_protocol::wire::agent_chat::ModelEntry;

    use super::super::strip_chips::apply_settings;
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

    /// Two reasoning models and one that does not reason, with catalog-wide
    /// levels `low` and `high`.
    fn catalog() -> ModelsCatalog {
        let entry = |provider: &str, model_id: &str, reasoning: bool| ModelEntry {
            provider: provider.to_owned(),
            model_id: model_id.to_owned(),
            name: model_id.to_owned(),
            available: true,
            reasoning,
            ..ModelEntry::default()
        };
        ModelsCatalog {
            models: vec![
                entry("p", "a", true),
                entry("q", "m", true),
                entry("q", "plain", false),
            ],
            providers: Vec::new(),
            thinking_levels: vec!["low".to_owned(), "high".to_owned()],
            default_model: None,
            recent_models: Vec::new(),
        }
    }

    #[test]
    fn assigning_keeps_the_explicit_level_when_the_new_model_accepts_it() {
        let next = apply_settings(
            &settings_with(&[("smol", "p/a:high")]),
            &StripAction::Assign("smol"),
            "q/m",
            &ModelScope::Global,
            &catalog(),
        )
        .expect("a role chip writes settings");
        assert_eq!(
            chain_in(&next, &ModelScope::Global, "smol")
                .first()
                .map(String::as_str),
            Some("q/m:high")
        );
    }

    #[test]
    fn assigning_a_model_that_takes_no_level_drops_the_level() {
        let settings = settings_with(&[("smol", "p/a:high")]);
        let next = apply_settings(
            &settings,
            &StripAction::Assign("smol"),
            "q/plain",
            &ModelScope::Global,
            &catalog(),
        )
        .expect("a role chip writes settings");
        assert_eq!(
            chain_in(&next, &ModelScope::Global, "smol")
                .first()
                .map(String::as_str),
            Some("q/plain")
        );
        assert!(
            thinking_step_after_assign(
                &settings,
                &catalog(),
                "q/plain",
                &ModelScope::Global,
                "smol"
            )
            .is_none()
        );
    }

    #[test]
    fn the_step_starts_on_the_kept_level_and_selects_it() {
        let catalog = catalog();
        let settings = settings_with(&[("smol", "p/a:high")]);
        let step =
            thinking_step_after_assign(&settings, &catalog, "q/m", &ModelScope::Global, "smol")
                .expect("a reasoning model opens the thinking step");
        // Options are "Chat's level", auto, low, high: the kept level is the last one.
        assert_eq!(step.cursor, 3);
        assert_eq!(step.thinking_role, Some("smol"));
        let after = apply_settings(
            &settings,
            &StripAction::Assign("smol"),
            "q/m",
            &ModelScope::Global,
            &catalog,
        )
        .expect("a role chip writes settings");
        let chips = thinking_chips(&after, &catalog, "q/m", &ModelScope::Global, "smol");
        let selected: Vec<&StripChip> = chips.iter().filter(|chip| chip.assigned).collect();
        assert_eq!(selected.len(), 1);
        assert_eq!(
            selected[0].action,
            StripAction::Think {
                role: "smol",
                level: Some("high".to_owned()),
            }
        );
    }

    #[test]
    fn the_step_starts_on_chats_level_when_the_role_had_no_level() {
        let step = thinking_step_after_assign(
            &settings_with(&[("smol", "p/a")]),
            &catalog(),
            "q/m",
            &ModelScope::Global,
            "smol",
        )
        .expect("a reasoning model opens the thinking step");
        assert_eq!(step.cursor, 0);
    }
}
