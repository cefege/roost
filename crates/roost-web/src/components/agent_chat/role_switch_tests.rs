//! The role switcher's rows and saves: which model a role runs for this
//! folder, where an edit lands, and how a new leader rewrites the chain.
//! Compiled by role_switch under cfg(test).

use std::collections::BTreeMap;

use roost_protocol::wire::agent_chat::{AgentSettingsView, FolderModelRoles};

use super::{initial_scope, role_lines, with_role_leader};
use crate::components::settings::model_scope::ModelScope;

fn folder() -> ModelScope {
    ModelScope::Folder {
        worker_fp: "fp".to_owned(),
        cwd: "/code/roost".to_owned(),
    }
}

fn settings(global: &[(&str, &str)], folder_roles: &[(&str, &str)]) -> AgentSettingsView {
    let mut settings = AgentSettingsView::default();
    for (role, chain) in global {
        settings
            .model_roles
            .insert((*role).to_owned(), (*chain).to_owned());
    }
    if !folder_roles.is_empty() {
        settings.folder_model_roles.push(FolderModelRoles {
            worker_fp: "fp".to_owned(),
            cwd: "/code/roost".to_owned(),
            model_roles: folder_roles
                .iter()
                .map(|(role, chain)| ((*role).to_owned(), (*chain).to_owned()))
                .collect::<BTreeMap<_, _>>(),
        });
    }
    settings
}

#[test]
fn rows_show_the_folder_override_over_all_folders_and_what_unset_roles_inherit() {
    let current = settings(&[("smol", "a/fast")], &[("smol", "b/faster,a/fast")]);
    let lines = role_lines(&current, &folder(), None);
    let smol = lines.iter().find(|line| line.role == "smol");
    assert_eq!(
        smol.map(|line| (line.model.as_str(), line.folder_override)),
        Some(("b/faster", true))
    );
    let slow = lines.iter().find(|line| line.role == "slow");
    assert_eq!(
        slow.map(|line| line.model.as_str()),
        Some("Same as Default model")
    );
    assert!(lines.iter().all(|line| line.role != "web"));
}

#[test]
fn an_edit_starts_where_the_role_is_set_for_this_folder() {
    let current = settings(&[("smol", "a/fast")], &[("plan", "c/plan")]);
    assert_eq!(initial_scope(&current, &folder(), "plan"), folder());
    assert_eq!(
        initial_scope(&current, &folder(), "smol"),
        ModelScope::Global
    );
}

#[test]
fn a_new_leader_replaces_the_first_model_and_keeps_the_fallbacks() {
    let current = settings(&[("slow", "a/deep,b/backup")], &[]);
    let updated = with_role_leader(&current, &ModelScope::Global, "slow", "c/deeper");
    assert_eq!(
        updated.model_roles.get("slow").map(String::as_str),
        Some("c/deeper,b/backup")
    );
    let promoted = with_role_leader(&current, &ModelScope::Global, "slow", "b/backup");
    assert_eq!(
        promoted.model_roles.get("slow").map(String::as_str),
        Some("b/backup")
    );
}

#[test]
fn a_folder_edit_overrides_only_that_folder() {
    let current = settings(&[("task", "a/work,b/spare")], &[]);
    let updated = with_role_leader(&current, &folder(), "task", "c/local");
    assert_eq!(
        updated.model_roles.get("task").map(String::as_str),
        Some("a/work,b/spare")
    );
    let row = updated.folder_model_roles.first();
    assert_eq!(
        row.and_then(|row| row.model_roles.get("task"))
            .map(String::as_str),
        Some("c/local,b/spare")
    );
}
