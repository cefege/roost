//! OMP-only settings scopes, their canonical resolver, and worker events.
//! Coordinator persistence and browser controls share these wire shapes.
//! Model values retain the provider prefix; no credentials cross this seam.

use super::ModelRef;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum OmpSessionState {
    Ready,
    Failed { error: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum OmpSettingsScope {
    Defaults,
    Machine { worker_fp: String },
    Folder { worker_fp: String, cwd: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OmpSettingValues {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub thinking_level: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OmpScopedSettings {
    pub scope: OmpSettingsScope,
    pub values: OmpSettingValues,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OmpChoice {
    pub value: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OmpWorkerCatalog {
    pub worker_fp: String,
    pub models: Vec<OmpChoice>,
    pub thinking_levels: Vec<OmpChoice>,
    pub updated_ms: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OmpSettingsView {
    pub scopes: Vec<OmpScopedSettings>,
    pub catalogs: Vec<OmpWorkerCatalog>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OmpSettingSource {
    Conversation,
    Folder,
    Machine,
    Defaults,
    Omp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OmpEffectiveValue {
    pub value: Option<String>,
    pub source: OmpSettingSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OmpEffectiveSettings {
    pub model: OmpEffectiveValue,
    pub thinking_level: OmpEffectiveValue,
}

pub fn resolve_omp_settings(
    scopes: &[OmpScopedSettings],
    worker_fp: &str,
    cwd: &str,
    conversation: &OmpSettingValues,
) -> OmpEffectiveSettings {
    let resolve = |field: fn(&OmpSettingValues) -> &Option<String>| {
        if let Some(value) = field(conversation)
            .as_ref()
            .filter(|value| !value.is_empty())
        {
            return OmpEffectiveValue {
                value: Some(value.clone()),
                source: OmpSettingSource::Conversation,
            };
        }
        for (rank, source) in [
            (3, OmpSettingSource::Folder),
            (2, OmpSettingSource::Machine),
            (1, OmpSettingSource::Defaults),
        ] {
            for setting in scopes {
                let scope_rank = match &setting.scope {
                    OmpSettingsScope::Defaults => 1,
                    OmpSettingsScope::Machine {
                        worker_fp: candidate,
                    } if candidate == worker_fp => 2,
                    OmpSettingsScope::Folder {
                        worker_fp: candidate,
                        cwd: folder,
                    } if candidate == worker_fp && folder == cwd => 3,
                    _ => 0,
                };
                if scope_rank == rank
                    && let Some(value) = field(&setting.values)
                        .as_ref()
                        .filter(|value| !value.is_empty())
                {
                    return OmpEffectiveValue {
                        value: Some(value.clone()),
                        source,
                    };
                }
            }
        }
        OmpEffectiveValue {
            value: None,
            source: OmpSettingSource::Omp,
        }
    };
    OmpEffectiveSettings {
        model: resolve(|values| &values.model),
        thinking_level: resolve(|values| &values.thinking_level),
    }
}

pub fn split_omp_model(value: &str) -> Option<ModelRef> {
    let (provider, model_id) = value.split_once('/')?;
    if provider.is_empty() || model_id.is_empty() {
        return None;
    }
    Some(ModelRef {
        provider: provider.to_owned(),
        model_id: model_id.to_owned(),
    })
}

pub fn join_omp_model(model: &ModelRef) -> String {
    format!("{}/{}", model.provider, model.model_id)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OmpWorkerEvent {
    Update {
        update: serde_json::Value,
    },
    SessionReady {
        acp_session_id: String,
        config_options: serde_json::Value,
    },
    TurnStarted {
        request_id: String,
    },
    TurnEnded {
        request_id: String,
        stop_reason: String,
        error: Option<String>,
        outstanding: u32,
    },
    Notice {
        level: String,
        title: String,
        body: String,
    },
    ProcessExited {
        error: Option<String>,
    },
    SessionFailed {
        error: String,
    },
}

#[cfg(test)]
mod tests;
