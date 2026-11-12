//! Live composition of base catalogs and models.json model overlays.
//! Used by models.json provider composition, without application state.
//! Extracted from revpidev/rpi 7ee970aad1d825b30b9413ce251c98e610214e2b.

use std::sync::{Arc, Mutex};
use crate::models::Provider;
use crate::models_json::ModelsJsonProvider;
use crate::types::{ApiKind, Model};
use super::model_overlay::{apply_model_override, merge_compat};
use super::model_definition::json_model_to_model;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

fn find_model_defaults<'a>(
    models: &'a [Model],
    model_id: &str,
    api: Option<&str>,
) -> Option<&'a Model> {
    if let Some(model) = models.iter().find(|model| model.id == model_id) {
        return Some(model);
    }
    if let Some(api) = api.filter(|api| !api.is_empty()) {
        let kind = ApiKind::from(api);
        if let Some(model) = models.iter().find(|model| model.api == kind) {
            return Some(model);
        }
    }
    models
        .iter()
        .find(|model| model.api == ApiKind::from(ApiKind::OPENAI_COMPLETIONS))
        .or_else(|| models.first())
}

fn apply_models_json(
    provider_id: &str,
    base_models: Vec<Model>,
    config: Option<&ModelsJsonProvider>,
) -> Result<Vec<Model>, String> {
    let Some(config) = config else {
        return Ok(base_models);
    };
    if config.oauth.is_some() && config.base_url.is_none() {
        return Err(format!(
            "Provider {provider_id}: \"baseUrl\" is required when \"oauth\" is set."
        ));
    }
    let has_overrides = config
        .model_overrides
        .as_ref()
        .is_some_and(|overrides| !overrides.is_empty());
    let has_models = config
        .models
        .as_ref()
        .is_some_and(|models| !models.is_empty());
    if !has_models
        && config.base_url.is_none()
        && config.headers.is_none()
        && config.compat.is_none()
        && !has_overrides
        && config.api_key.is_none()
        && config.oauth.is_none()
        && config.auth_header.is_none()
    {
        return Err(format!(
            "Provider {provider_id}: must specify \"baseUrl\", \"headers\", \"compat\", \"modelOverrides\", or \"models\"."
        ));
    }
    let radius_oauth = config.oauth.as_deref() == Some("radius");
    let mut models: Vec<Model> = base_models
        .into_iter()
        .map(|mut model| {
            if !radius_oauth
                && let Some(base_url) = config.base_url.as_ref().filter(|url| !url.is_empty())
            {
                model.base_url = base_url.clone();
            }
            if let Some(compat) = config.compat.as_ref() {
                model.compat = Some(merge_compat(model.compat.as_ref(), compat));
            }
            model
        })
        .collect();
    for definition in config.models.iter().flatten() {
        let api = definition.api.clone().or_else(|| config.api.clone());
        let defaults = find_model_defaults(&models, &definition.id, api.as_deref());
        let model = json_model_to_model(
            provider_id,
            &config.api,
            config.base_url.as_deref(),
            config.compat.as_ref(),
            definition.clone(),
            defaults,
        )?;
        match models.iter().position(|model| model.id == definition.id) {
            Some(index) => models[index] = model,
            None => models.push(model),
        }
    }
    Ok(models)
}
fn apply_model_overrides(models: Vec<Model>, config: Option<&ModelsJsonProvider>) -> Vec<Model> {
    let Some(overrides) = config.and_then(|config| config.model_overrides.as_ref()) else {
        return models;
    };
    models
        .into_iter()
        .map(|model| match overrides.get(&model.id) {
            Some(override_) => apply_model_override(model, override_),
            None => model,
        })
        .collect()
}

pub(super) struct ComposedModels {
    provider_id: String,
    base: Option<Arc<dyn Provider>>,
    config: Option<ModelsJsonProvider>,
    last_valid: std::sync::Mutex<Vec<Model>>,
}

impl ComposedModels {
    pub(super) fn new(
        provider_id: &str,
        base: Option<Arc<dyn Provider>>,
        config: Option<ModelsJsonProvider>,
        ) -> Self {
        Self {
            provider_id: provider_id.to_owned(),
            base,
            config,
            last_valid: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn compose(&self) -> Result<Vec<Model>, String> {
        let base_models = self
            .base
            .as_ref()
            .map(|base| base.get_models())
            .unwrap_or_default();
        let models = apply_models_json(&self.provider_id, base_models, self.config.as_ref())?;
        Ok(apply_model_overrides(models, self.config.as_ref()))
    }

    pub(super) fn compose_eager(&self) -> Result<Vec<Model>, String> {
        let models = self.compose()?;
        *lock(&self.last_valid) = models.clone();
        Ok(models)
    }

    pub(super) fn get_models(&self) -> Vec<Model> {
        match self.compose_eager() {
            Ok(models) => models,
            Err(_) => lock(&self.last_valid).clone(),
        }
    }
}
