//! Worker-local Rpi provider snapshots shared by the runtime and operator CLI.
//! Each load checks local auth/config before publishing a fresh collection.
//! In-flight turns retain their collection; no coordinator credential path exists.

use std::{path::PathBuf, sync::Arc};

use roost_proto::RpiModel;
use rpi_ai::{
    AnyModel, InputModality, Model, ModelType,
    auth::{AuthContext, DefaultAuthContext, FileCredentialStore},
    models::{CreateModelsOptions, Models, get_supported_thinking_levels},
    models_json::ModelConfig,
    provider_config::compose_configured_provider,
    providers::builtin_models,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    #[error("RPI auth.json is unreadable or invalid; check this worker's rpi/auth.json")]
    AuthenticationStorage,
    #[error("RPI models.json is unreadable or invalid; check this worker's rpi/models.json")]
    Configuration,
    #[error("RPI model availability failed; check this worker with roost rpi models")]
    Availability,
    #[error("Selected RPI model is unavailable; configure this worker with roost rpi login <provider>")]
    ModelUnavailable,
}

#[derive(Clone)]
pub struct ProviderLoader {
    directory: PathBuf,
    auth_context: Arc<dyn AuthContext>,
}

pub struct ProviderSnapshot {
    pub models: Models,
}

impl ProviderLoader {
    pub fn new(worker_data: PathBuf) -> Self {
        Self {
            directory: worker_data.join("rpi"),
            auth_context: Arc::new(DefaultAuthContext),
        }
    }

    pub async fn load(&self) -> Result<ProviderSnapshot, ProviderError> {
        let credentials = Arc::new(FileCredentialStore::new(self.directory.join("auth.json")));
        credentials.reload_checked().await.map_err(|_| ProviderError::AuthenticationStorage)?;
        let config = ModelConfig::load(Some(&self.directory.join("models.json"))).await;
        if config.error().is_some() {
            return Err(ProviderError::Configuration);
        }
        let models = builtin_models(Some(CreateModelsOptions {
            credentials: Some(credentials),
            auth_context: Some(self.auth_context.clone()),
            models_store: None,
        }));
        for provider_id in config.provider_ids() {
            let Some(provider_config) = config.get_provider(provider_id) else {
                return Err(ProviderError::Configuration);
            };
            let base = models.get_provider(provider_id);
            let provider = compose_configured_provider(provider_id, base.as_ref(), provider_config)
                .map_err(|_| ProviderError::Configuration)?;
            models.set_provider(provider);
        }
        Ok(ProviderSnapshot { models })
    }
}

impl ProviderSnapshot {
    pub async fn available_models(&self) -> Result<Vec<RpiModel>, ProviderError> {
        let available = self.models.get_available_of_type(ModelType::Chat, None).await
            .map_err(|_| ProviderError::Availability)?;
        Ok(available.into_iter().filter_map(|model| {
            let AnyModel::Chat(model) = model else { return None; };
            let thinking_levels = get_supported_thinking_levels(&model).into_iter()
                .map(|level| level.as_str().to_owned()).collect();
            Some(RpiModel {
                supports_images: model.input.contains(&InputModality::Image),
                provider: model.provider,
                model_id: model.id,
                name: model.name,
                thinking_levels,
                ..Default::default()
            })
        }).collect())
    }

    pub async fn selected_model(&self, provider: &str, model_id: &str) -> Result<Model, ProviderError> {
        self.models.get_available_of_type(ModelType::Chat, Some(provider)).await
            .map_err(|_| ProviderError::Availability)?
            .into_iter().find_map(|model| match model {
                AnyModel::Chat(model) if model.provider == provider && model.id == model_id => Some(model),
                _ => None,
            }).ok_or(ProviderError::ModelUnavailable)
    }
}

#[cfg(test)]
mod tests;
