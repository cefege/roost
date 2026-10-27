//! Pure role-editor tests cover chains, fallback edits and presets.
//! The settings transformations are exercised independently of the wasm writer.
//! Session pin preservation is verified through the actual model hub.

use roost_protocol::wire::agent_chat::{AgentSettingsView, ModelsCatalog};

use super::edits::{
    add_fallback, assign_entry, clear_role, cycle_preset, move_fallback, next_thinking,
    preset_names, remove_fallback, split_entry,
};
use super::rows::{LeaderValue, RoleTab, RolesRow, build_rows, chain_of, leader_value};

fn settings_with(roles: &[(&str, &str)]) -> AgentSettingsView {
    let mut settings = AgentSettingsView::default();
    for (role, value) in roles {
        settings
            .model_roles
            .insert((*role).to_owned(), (*value).to_owned());
    }
    settings
}

fn empty_catalog() -> ModelsCatalog {
    ModelsCatalog {
        models: Vec::new(),
        providers: Vec::new(),
        thinking_levels: Vec::new(),
        default_model: None,
        recent_models: Vec::new(),
    }
}

#[test]
fn main_tab_shows_only_default_and_fast_with_their_fallbacks() {
    let settings = settings_with(&[("default", "a/x,b/y"), ("task", "t/m"), ("web", "w/s")]);
    let rows = build_rows(&settings, RoleTab::Main);
    let roles: Vec<&str> = rows
        .iter()
        .filter_map(|row| match row {
            RolesRow::Role { role } => Some(*role),
            _ => None,
        })
        .collect();

    assert_eq!(roles, ["default", "smol"]);
    assert!(rows.contains(&RolesRow::Fallback {
        role: "default",
        index: 1,
        selector: "b/y".to_owned(),
    }));
    assert_eq!(rows.last(), Some(&RolesRow::NewFallback));
}

#[test]
fn advanced_tab_holds_every_other_model_role_but_never_search_engines() {
    let rows = build_rows(&AgentSettingsView::default(), RoleTab::Advanced);

    for role in ["plan", "task", "slow", "tiny", "judge", "advisor"] {
        assert!(rows.contains(&RolesRow::Role { role }), "{role}");
    }
    for role in ["default", "smol", "web"] {
        assert!(!rows.contains(&RolesRow::Role { role }), "{role}");
    }
}

#[test]
fn a_shortcut_opens_the_tab_that_lists_its_role() {
    assert_eq!(RoleTab::of("smol"), RoleTab::Main);
    assert_eq!(RoleTab::of("task"), RoleTab::Advanced);
}

#[test]
fn leader_value_distinguishes_assigned_auto_and_inherited() {
    let settings = settings_with(&[("smol", "q/m")]);

    assert_eq!(
        leader_value(&settings, "smol"),
        LeaderValue::Assigned("q/m".to_owned())
    );
    assert_eq!(leader_value(&settings, "default"), LeaderValue::Auto);
    assert_eq!(leader_value(&settings, "task"), LeaderValue::Unset);
}

#[test]
fn assign_entry_moves_a_selector_already_in_the_chain() {
    let settings = settings_with(&[("default", "a/x,b/y,c/z")]);
    let next = assign_entry(&settings, "default", 1, "c/z").expect("index 1 exists");

    assert_eq!(
        chain_of(&next, "default"),
        vec!["a/x".to_owned(), "c/z".to_owned()]
    );
}

#[test]
fn assign_entry_past_the_chain_end_is_none() {
    let settings = settings_with(&[("default", "a/x")]);

    assert!(assign_entry(&settings, "default", 3, "c/z").is_none());
}

#[test]
fn add_fallback_appends_and_ignores_an_existing_entry() {
    let settings = settings_with(&[("default", "a/x")]);
    let added = add_fallback(&settings, "default", "b/y");
    let again = add_fallback(&added, "default", "a/x");

    assert_eq!(
        chain_of(&added, "default"),
        vec!["a/x".to_owned(), "b/y".to_owned()]
    );
    assert_eq!(
        chain_of(&again, "default"),
        vec!["a/x".to_owned(), "b/y".to_owned()]
    );
}

#[test]
fn add_fallback_to_an_unset_role_makes_it_the_leader() {
    let next = add_fallback(&AgentSettingsView::default(), "smol", "q/m");

    assert_eq!(chain_of(&next, "smol"), vec!["q/m".to_owned()]);
}

#[test]
fn remove_fallback_never_removes_the_leader() {
    let settings = settings_with(&[("default", "a/x,b/y")]);

    assert!(remove_fallback(&settings, "default", 0).is_none());
    let next = remove_fallback(&settings, "default", 1).expect("index 1 exists");
    assert_eq!(chain_of(&next, "default"), vec!["a/x".to_owned()]);
}

#[test]
fn clear_role_drops_the_saved_value() {
    let settings = settings_with(&[("default", "a/x")]);
    let next = clear_role(&settings, "default");

    assert!(!next.model_roles.contains_key("default"));
}

#[test]
fn move_fallback_swaps_within_fallbacks_only() {
    let settings = settings_with(&[("default", "a/x,b/y,c/z")]);
    let down = move_fallback(&settings, "default", 1, 1).expect("can move down");

    assert_eq!(
        chain_of(&down, "default"),
        vec!["a/x".to_owned(), "c/z".to_owned(), "b/y".to_owned()]
    );
    assert!(move_fallback(&settings, "default", 1, -1).is_none());
    assert!(move_fallback(&settings, "default", 0, 1).is_none());
}

#[test]
fn next_thinking_cycles_none_then_each_choice() {
    let choices = vec!["low".to_owned(), "high".to_owned()];

    assert_eq!(next_thinking(None, &choices), Some("low".to_owned()));
    assert_eq!(
        next_thinking(Some("low"), &choices),
        Some("high".to_owned())
    );
    assert_eq!(next_thinking(Some("high"), &choices), None);
}

#[test]
fn split_entry_keeps_a_colon_that_is_not_a_known_level() {
    let catalog = empty_catalog();

    assert_eq!(
        split_entry(&catalog, "org/model:v2"),
        ("org/model:v2", None)
    );
}

#[test]
fn cycle_preset_from_a_custom_setup_starts_at_the_ends() {
    let settings = settings_with(&[("default", "zz/qq")]);
    let names = preset_names(&settings);
    let first = cycle_preset(&settings, 1).expect("builtins exist");
    let last = cycle_preset(&settings, -1).expect("builtins exist");

    assert_eq!(Some(&first), names.first());
    assert_eq!(Some(&last), names.last());
}

#[test]
fn preset_names_follow_the_map_key_order_sorted_by_name() {
    let names = preset_names(&AgentSettingsView::default());
    let mut sorted = names.clone();
    sorted.sort();

    assert!(!names.is_empty());
    assert_eq!(names, sorted);
}
