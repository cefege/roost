//! OMP settings scope identities and machine-specific catalog choices.
//! The pane derives known folders from OMP conversations and persisted scopes.
//! Inheritance delegates to the protocol resolver with only less-specific rows.

use crate::{components::ui::SelectOption, pump::Pump};
use roost_protocol::wire::agent_chat::*;
use std::collections::BTreeMap;

pub(super) fn scope_choices(
    pump: &Pump,
    view: &OmpSettingsView,
) -> Vec<(OmpSettingsScope, String)> {
    let core = pump.core();
    let core = core.borrow();
    let store = core.store();
    let labels: BTreeMap<String, String> = store
        .workers
        .values()
        .map(|worker| (worker.fp.to_string(), worker.label.clone()))
        .collect();
    let label = |fp: &str| {
        labels
            .get(fp)
            .cloned()
            .unwrap_or_else(|| fp.chars().take(8).collect())
    };
    let mut result = vec![(OmpSettingsScope::Defaults, "Your defaults".into())];
    let mut machines: Vec<_> = labels.iter().collect();
    machines.sort_by(|left, right| left.1.cmp(right.1));
    for (fp, label) in machines {
        result.push((
            OmpSettingsScope::Machine {
                worker_fp: fp.clone(),
            },
            format!("Machine: {label}"),
        ));
    }
    let mut folders = BTreeMap::new();
    for conversation in store
        .agent_chat
        .conversations
        .values()
        .filter(|conversation| !conversation.is_native())
    {
        folders.insert(
            (conversation.worker_fp.clone(), conversation.cwd.clone()),
            label(&conversation.worker_fp),
        );
    }
    for row in &view.scopes {
        if let OmpSettingsScope::Folder { worker_fp, cwd } = &row.scope {
            folders.insert((worker_fp.clone(), cwd.clone()), label(worker_fp));
        }
    }
    for ((worker_fp, cwd), label) in folders {
        result.push((
            OmpSettingsScope::Folder {
                worker_fp,
                cwd: cwd.clone(),
            },
            format!("Folder: {label} · {cwd}"),
        ));
    }
    result
}

pub(super) fn scope_id(scope: &OmpSettingsScope) -> String {
    serde_json::to_string(scope).unwrap_or_default()
}

pub(super) fn inherited(view: &OmpSettingsView, scope: &OmpSettingsScope) -> OmpEffectiveSettings {
    let rank = |scope: &OmpSettingsScope| match scope {
        OmpSettingsScope::Defaults => 0,
        OmpSettingsScope::Machine { .. } => 1,
        OmpSettingsScope::Folder { .. } => 2,
    };
    let rows: Vec<_> = view
        .scopes
        .iter()
        .filter(|row| rank(&row.scope) < rank(scope))
        .cloned()
        .collect();
    let (worker, cwd) = match scope {
        OmpSettingsScope::Defaults => ("", ""),
        OmpSettingsScope::Machine { worker_fp } => (worker_fp.as_str(), ""),
        OmpSettingsScope::Folder { worker_fp, cwd } => (worker_fp.as_str(), cwd.as_str()),
    };
    resolve_omp_settings(&rows, worker, cwd, &OmpSettingValues::default())
}

pub(super) fn choices(
    view: &OmpSettingsView,
    scope: &OmpSettingsScope,
    model: bool,
    saved: Option<&str>,
) -> Vec<SelectOption> {
    let worker = match scope {
        OmpSettingsScope::Defaults => None,
        OmpSettingsScope::Machine { worker_fp } | OmpSettingsScope::Folder { worker_fp, .. } => {
            Some(worker_fp)
        }
    };
    let mut choices = BTreeMap::new();
    for catalog in view
        .catalogs
        .iter()
        .filter(|catalog| worker.is_none_or(|fp| fp == &catalog.worker_fp))
    {
        for choice in if model {
            &catalog.models
        } else {
            &catalog.thinking_levels
        } {
            choices.entry(choice.value.clone()).or_insert_with(|| {
                crate::components::agent_chat::omp_controls::choice_label(choice)
            });
        }
    }
    if let Some(saved) = saved.filter(|saved| !saved.is_empty()) {
        choices.entry(saved.into()).or_insert_with(|| saved.into());
    }
    std::iter::once(SelectOption::new("", "Inherit"))
        .chain(
            choices
                .into_iter()
                .map(|(value, name)| SelectOption::new(value, name)),
        )
        .collect()
}
