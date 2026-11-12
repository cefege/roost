//! Custom model definitions and inherited API defaults.
//! Used by models.json provider composition, without application state.
//! Extracted from revpidev/rpi 7ee970aad1d825b30b9413ce251c98e610214e2b.

use crate::models_json::ModelsJsonModel;
use crate::types::{ApiKind, Model, ModelCompat};
use super::model_overlay::merge_compat;

fn default_input() -> Vec<crate::types::InputModality> {
    vec![crate::types::InputModality::Text]
}

pub(super) fn json_model_to_model(
    provider_id: &str,
    provider_api: &Option<String>,
    provider_base_url: Option<&str>,
    provider_compat: Option<&ModelCompat>,
    model: ModelsJsonModel,
    defaults: Option<&Model>,
) -> Result<Model, String> {
    let api = model
        .api
        .clone()
        .or_else(|| provider_api.clone())
        .or_else(|| defaults.map(|defaults| defaults.api.as_str().to_owned()))
        .filter(|api| !api.is_empty());
    let Some(api) = api else {
        return Err(format!(
            "Provider {provider_id}, model {}: no \"api\" specified. Set at provider or model level.",
            model.id
        ));
    };
    let base_url = model
        .base_url
        .clone()
        .or_else(|| provider_base_url.map(str::to_owned))
        .or_else(|| defaults.map(|defaults| defaults.base_url.clone()))
        .filter(|url| !url.is_empty());
    let Some(base_url) = base_url else {
        return Err(format!(
            "Provider {provider_id}: \"baseUrl\" is required when defining custom models."
        ));
    };
    if let Some(context_window) = model.context_window
        && context_window <= 0.0
    {
        return Err(format!(
            "Provider {provider_id}, model {}: invalid contextWindow",
            model.id
        ));
    }
    if let Some(max_tokens) = model.max_tokens
        && max_tokens <= 0.0
    {
        return Err(format!(
            "Provider {provider_id}, model {}: invalid maxTokens",
            model.id
        ));
    }
    Ok(Model {
        name: model.name.clone().unwrap_or_else(|| model.id.clone()),
        api: ApiKind::from(api),
        provider: provider_id.to_owned(),
        base_url,
        reasoning: model.reasoning.unwrap_or(false),
        thinking_level_map: model.thinking_level_map.clone(),
        input: model.input.clone().unwrap_or_else(default_input),
        input_limits: model.input_limits,
        cost: model.cost.clone().unwrap_or_default(),
        prompt_cache: model
            .prompt_cache
            .map(|cache| crate::types::ModelPromptCache {
                short: cache.short,
                long: cache.long,
            }),
        context_window: model.context_window.unwrap_or(128000.0) as u32,
        max_tokens: model.max_tokens.unwrap_or(16384.0) as u32,
        sampling_params: model.sampling_params.clone(),
        sampling_params_by_thinking_level: model.sampling_params_by_thinking_level.clone(),
        headers: model
            .headers
            .as_ref()
            .map(|h| h.iter().map(|(k, v)| (k.clone(), v.clone())).collect()),
        compat: match model.compat.clone() {
            Some(compat) => Some(merge_compat(provider_compat, &compat)),
            None => provider_compat.cloned(),
        },
        id: model.id,
    })
}
