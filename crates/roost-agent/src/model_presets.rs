//! `/modelpreset`: save, list, switch and delete named sets of all-folders
//! model roles, and the checks every saved preset must pass.

use std::collections::BTreeMap;

use roost_protocol::wire::agent_chat::{
    ModelPresets, PRESET_NAME_HINT, active_preset, valid_preset_name,
};

use crate::error::AgentError;
use crate::records::{AgentSettings, ConversationRecord, Role};
use crate::roles;
use crate::runtime::AgentRuntime;

const USAGE: &str = "Usage: /modelpreset [list | save <name> | switch <name> | delete <name>]";

/// Every preset has a valid name and sets at least one known role to a
/// well-formed selector chain.
pub(crate) fn validate_presets(presets: &ModelPresets) -> Result<(), AgentError> {
    for (name, preset) in presets {
        if !valid_preset_name(name) {
            return Err(invalid(format!("preset {name:?}: {PRESET_NAME_HINT}")));
        }
        if preset.is_empty() {
            return Err(invalid(format!("preset {name} sets no roles")));
        }
        for (key, chain) in preset {
            let Some(role) = Role::from_name(key) else {
                return Err(invalid(format!("preset {name}: unknown role {key}")));
            };
            roles::validate_role_selector(role, chain)
                .map_err(|error| invalid(format!("preset {name}: role {key}: {error}")))?;
        }
    }
    Ok(())
}

pub(crate) async fn command(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    args: &str,
) -> Result<(), AgentError> {
    let words: Vec<&str> = args.split_whitespace().collect();
    let mut settings = runtime.settings().await?;
    let (title, body) = match words.as_slice() {
        [] | ["list"] => ("Model presets", list(&settings)),
        ["save", name] => {
            if !valid_preset_name(name) {
                return Err(invalid(format!(
                    "Invalid preset name {name:?}: {PRESET_NAME_HINT}"
                )));
            }
            let roles = global_roles(&settings);
            if roles.is_empty() {
                return Err(invalid(
                    "No all-folders roles are set; set them in Settings → Models before saving a preset".into(),
                ));
            }
            let count = roles.len();
            settings.model_presets.insert((*name).to_owned(), roles);
            runtime.set_settings(settings).await?;
            (
                "Model preset saved",
                format!("Saved `{name}` with {count} roles."),
            )
        }
        ["switch", name] => {
            let preset = settings
                .model_presets
                .get(*name)
                .ok_or_else(|| invalid(format!("Preset not found: {name}")))?;
            settings.model_roles = preset
                .iter()
                .filter_map(|(key, chain)| Role::from_name(key).map(|role| (role, chain.clone())))
                .collect();
            let overridden = folder_overrides(&settings, record);
            runtime.set_settings(settings).await?;
            let mut body = format!("Switched to preset `{name}`.");
            if !overridden.is_empty() {
                body.push_str(&format!(
                    " This folder still overrides: {}.",
                    overridden.join(", ")
                ));
            }
            ("Model preset applied", body)
        }
        ["delete", name] => {
            if settings.model_presets.remove(*name).is_none() {
                return Err(invalid(format!("Preset not found: {name}")));
            }
            runtime.set_settings(settings).await?;
            ("Model preset deleted", format!("Deleted `{name}`."))
        }
        _ => return Err(invalid(USAGE.into())),
    };
    runtime.notice(&record.id, "info", title, &body).await;
    Ok(())
}

fn list(settings: &AgentSettings) -> String {
    if settings.model_presets.is_empty() {
        return "No model presets saved. Use `/modelpreset save <name>` to create one.".into();
    }
    let active = active_preset(&settings.model_presets, &global_roles(settings));
    let mut table = String::from("| Preset | Roles |\n|---|---|\n");
    for (name, preset) in &settings.model_presets {
        let marker = if active == Some(name.as_str()) {
            " (active)"
        } else {
            ""
        };
        let roles: Vec<String> = preset
            .iter()
            .map(|(role, chain)| format!("{role}: {chain}"))
            .collect();
        table.push_str(&format!("| {name}{marker} | {} |\n", roles.join("; ")));
    }
    table
}

/// The all-folders roles as preset strings, blank chains dropped.
fn global_roles(settings: &AgentSettings) -> BTreeMap<String, String> {
    settings
        .model_roles
        .iter()
        .filter(|(_, chain)| !chain.trim().is_empty())
        .map(|(role, chain)| (role.as_str().to_owned(), chain.clone()))
        .collect()
}

/// Roles this conversation's folder row sets, which a preset does not touch.
fn folder_overrides(settings: &AgentSettings, record: &ConversationRecord) -> Vec<String> {
    settings
        .folder_model_roles
        .iter()
        .find(|row| row.worker_fp == record.worker_fp && row.cwd == record.cwd)
        .map(|row| {
            row.model_roles
                .iter()
                .filter(|(_, chain)| !chain.trim().is_empty())
                .map(|(role, _)| role.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn invalid(message: String) -> AgentError {
    AgentError::InvalidArgument(message)
}
