//! Which saved model chain a role reads for one folder: the folder's own row
//! over the global map, role by role. A folder row is identified by the exact
//! worker fingerprint and cwd, never by label. Pure; the roles pane edits
//! through these, and the backend owns the resolution of a live session.

use std::collections::BTreeMap;

use roost_protocol::wire::agent_chat::{AgentSettingsView, FolderModelRoles};

/// Where a role edit is written: the global map, or one folder's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelScope {
    Global,
    Folder { worker_fp: String, cwd: String },
}

/// A worker folder offered by the scope picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownFolder {
    pub worker_fp: String,
    pub cwd: String,
    pub label: String,
}

/// Where a role's effective value comes from, for the row's source label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleSource {
    Folder,
    Global,
    Unset,
}

/// The fingerprint is hex; splitting only the first marker preserves the cwd,
/// including any control characters allowed by the worker's filesystem.
const SCOPE_SEPARATOR: char = '\u{1f}';

/// Distinct workspace, conversation and configured folders, sorted by label.
pub fn known_folders<'a>(
    identities: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>,
) -> Vec<KnownFolder> {
    let mut seen: BTreeMap<(String, String), String> = BTreeMap::new();
    for (worker_fp, cwd, worker_label) in identities {
        seen.entry((worker_fp.to_owned(), cwd.to_owned()))
            .or_insert_with(|| worker_label.to_owned());
    }
    let mut folders: Vec<KnownFolder> = seen
        .into_iter()
        .map(|((worker_fp, cwd), worker_label)| KnownFolder {
            label: format!("{cwd} · {worker_label}"),
            worker_fp,
            cwd,
        })
        .collect();
    folders.sort_by(|left, right| left.label.cmp(&right.label));
    folders
}

/// The picker's id for a scope; it round-trips through `scope_from_id`.
pub fn scope_id(scope: &ModelScope) -> String {
    match scope {
        ModelScope::Global => "global".to_owned(),
        ModelScope::Folder { worker_fp, cwd } => {
            format!("{worker_fp}{SCOPE_SEPARATOR}{cwd}")
        }
    }
}

/// Reads a picker id back. An unknown id falls back to the global scope.
pub fn scope_from_id(id: &str) -> ModelScope {
    match id.split_once(SCOPE_SEPARATOR) {
        Some((worker_fp, cwd)) => ModelScope::Folder {
            worker_fp: worker_fp.to_owned(),
            cwd: cwd.to_owned(),
        },
        None => ModelScope::Global,
    }
}

/// The picker's visible text for a scope.
pub fn scope_label(scope: &ModelScope, folders: &[KnownFolder]) -> String {
    match scope {
        ModelScope::Global => "All folders".to_owned(),
        ModelScope::Folder { worker_fp, cwd } => folders
            .iter()
            .find(|folder| folder.worker_fp == *worker_fp && folder.cwd == *cwd)
            .map_or_else(|| cwd.clone(), |folder| folder.label.clone()),
    }
}

/// The folder's own row for this exact scope, if it has one.
pub fn folder_row<'a>(
    settings: &'a AgentSettingsView,
    worker_fp: &str,
    cwd: &str,
) -> Option<&'a FolderModelRoles> {
    settings
        .folder_model_roles
        .iter()
        .find(|row| row.worker_fp == worker_fp && row.cwd == cwd)
}

/// The chain a role resolves to in this scope: the folder's value when it sets
/// one, otherwise the global value. Blank values count as unset.
pub fn role_value(settings: &AgentSettingsView, scope: &ModelScope, role: &str) -> Option<String> {
    if let ModelScope::Folder { worker_fp, cwd } = scope
        && let Some(value) = folder_row(settings, worker_fp, cwd)
            .and_then(|row| row.model_roles.get(role))
            .filter(|value| !value.trim().is_empty())
    {
        return Some(value.clone());
    }
    settings
        .model_roles
        .get(role)
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .or_else(|| {
            (role == "default")
                .then_some(settings.default_model.as_ref())
                .flatten()
                .map(|model| format!("{}/{}", model.provider, model.model_id))
        })
}

/// Where `role_value` took its answer from.
pub fn role_source(settings: &AgentSettingsView, scope: &ModelScope, role: &str) -> RoleSource {
    if let ModelScope::Folder { worker_fp, cwd } = scope
        && folder_row(settings, worker_fp, cwd)
            .and_then(|row| row.model_roles.get(role))
            .is_some_and(|value| !value.trim().is_empty())
    {
        return RoleSource::Folder;
    }
    if settings
        .model_roles
        .get(role)
        .is_some_and(|value| !value.trim().is_empty())
        || (role == "default" && settings.default_model.is_some())
    {
        RoleSource::Global
    } else {
        RoleSource::Unset
    }
}

/// The settings after one role is set in `scope`. `None` or a blank value
/// clears it: a global clear leaves the role unset, a folder clear makes it
/// inherit the global value. Folder rows left empty are dropped.
pub fn with_role_value(
    settings: &AgentSettingsView,
    scope: &ModelScope,
    role: &str,
    value: Option<String>,
) -> AgentSettingsView {
    let mut updated = settings.clone();
    let value = value.filter(|value| !value.trim().is_empty());
    match scope {
        ModelScope::Global => {
            set_or_remove(&mut updated.model_roles, role, value);
            if role == "default" {
                updated.default_model = None;
            }
        }
        ModelScope::Folder { worker_fp, cwd } => {
            let index = updated
                .folder_model_roles
                .iter()
                .position(|row| row.worker_fp == *worker_fp && row.cwd == *cwd);
            match (index, value) {
                (Some(index), value) => {
                    set_or_remove(
                        &mut updated.folder_model_roles[index].model_roles,
                        role,
                        value,
                    );
                }
                (None, Some(value)) => {
                    let mut model_roles = BTreeMap::new();
                    model_roles.insert(role.to_owned(), value);
                    updated.folder_model_roles.push(FolderModelRoles {
                        worker_fp: worker_fp.clone(),
                        cwd: cwd.clone(),
                        model_roles,
                    });
                }
                (None, None) => {}
            }
        }
    }
    prune_empty_folder_rows(&mut updated);
    updated
}

/// Drops blank role values and folder rows that no longer override anything,
/// so the saved document carries only real overrides.
pub fn prune_empty_folder_rows(settings: &mut AgentSettingsView) {
    for row in &mut settings.folder_model_roles {
        row.model_roles.retain(|_, value| !value.trim().is_empty());
    }
    settings
        .folder_model_roles
        .retain(|row| !row.model_roles.is_empty());
}

fn set_or_remove(roles: &mut BTreeMap<String, String>, role: &str, value: Option<String>) {
    match value {
        Some(value) => {
            roles.insert(role.to_owned(), value);
        }
        None => {
            roles.remove(role);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(worker_fp: &str, cwd: &str) -> ModelScope {
        ModelScope::Folder {
            worker_fp: worker_fp.to_owned(),
            cwd: cwd.to_owned(),
        }
    }

    fn settings_with_global(role: &str, chain: &str) -> AgentSettingsView {
        let mut settings = AgentSettingsView::default();
        settings
            .model_roles
            .insert(role.to_owned(), chain.to_owned());
        settings
    }

    #[test]
    fn folder_value_overrides_global_and_absent_key_inherits() {
        let settings = with_role_value(
            &settings_with_global("slow", "p/global"),
            &folder("fp1", "/work/a"),
            "default",
            Some("p/folder".to_owned()),
        );
        let scope = folder("fp1", "/work/a");
        assert_eq!(
            role_value(&settings, &scope, "default").as_deref(),
            Some("p/folder")
        );
        assert_eq!(
            role_value(&settings, &scope, "slow").as_deref(),
            Some("p/global")
        );
        assert_eq!(role_source(&settings, &scope, "slow"), RoleSource::Global);
        assert_eq!(role_source(&settings, &scope, "task"), RoleSource::Unset);
    }

    #[test]
    fn folder_identity_is_exact_worker_and_cwd() {
        let settings = with_role_value(
            &AgentSettingsView::default(),
            &folder("fp1", "/work/a"),
            "default",
            Some("p/folder".to_owned()),
        );
        assert_eq!(
            role_value(&settings, &folder("fp2", "/work/a"), "default"),
            None
        );
        assert_eq!(
            role_value(&settings, &folder("fp1", "/work/b"), "default"),
            None
        );
        assert_eq!(
            role_value(&settings, &ModelScope::Global, "default"),
            None,
            "a folder row never leaks into the global scope"
        );
    }

    #[test]
    fn clearing_a_folder_role_inherits_and_drops_the_empty_row() {
        let scope = folder("fp1", "/work/a");
        let set = with_role_value(
            &settings_with_global("default", "p/global"),
            &scope,
            "default",
            Some("p/folder".to_owned()),
        );
        assert_eq!(set.folder_model_roles.len(), 1);

        let cleared = with_role_value(&set, &scope, "default", Some("  ".to_owned()));
        assert!(cleared.folder_model_roles.is_empty());
        assert_eq!(
            role_value(&cleared, &scope, "default").as_deref(),
            Some("p/global")
        );
    }

    #[test]
    fn clearing_a_global_role_leaves_it_unset() {
        let cleared = with_role_value(
            &settings_with_global("default", "p/global"),
            &ModelScope::Global,
            "default",
            None,
        );
        assert!(!cleared.model_roles.contains_key("default"));
        assert_eq!(
            role_source(&cleared, &ModelScope::Global, "default"),
            RoleSource::Unset
        );
    }

    #[test]
    fn saved_default_is_visible_until_global_chain_edit_replaces_or_clears_it() {
        let saved = AgentSettingsView {
            default_model: Some(roost_protocol::wire::agent_chat::ModelRef {
                provider: "p".into(),
                model_id: "saved".into(),
            }),
            ..AgentSettingsView::default()
        };
        assert_eq!(
            role_value(&saved, &folder("fp1", "/work/a"), "default").as_deref(),
            Some("p/saved")
        );
        assert_eq!(
            role_source(&saved, &ModelScope::Global, "default"),
            RoleSource::Global
        );
        let changed = with_role_value(&saved, &ModelScope::Global, "default", Some("p/new".into()));
        assert_eq!(
            role_value(&changed, &ModelScope::Global, "default").as_deref(),
            Some("p/new")
        );
        assert_eq!(changed.default_model, None);
        let cleared = with_role_value(&saved, &ModelScope::Global, "default", None);
        assert_eq!(role_value(&cleared, &ModelScope::Global, "default"), None);
        assert_eq!(cleared.default_model, None);
    }

    #[test]
    fn scope_id_round_trips() {
        let scope = folder("fp1", "/work/a b\u{1f}/c");
        assert_eq!(scope_from_id(&scope_id(&scope)), scope);
        assert_eq!(
            scope_from_id(&scope_id(&ModelScope::Global)),
            ModelScope::Global
        );
    }
}
