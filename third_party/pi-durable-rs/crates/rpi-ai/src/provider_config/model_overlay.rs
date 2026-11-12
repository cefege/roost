//! Model compatibility and metadata overlay merge rules.
//! Used by models.json provider composition, without application state.
//! Extracted from revpidev/rpi 7ee970aad1d825b30b9413ce251c98e610214e2b.

use std::collections::BTreeMap;
use crate::models_json::ModelsJsonModelOverride;
use crate::types::{Model, ModelCompat, ModelCost, ModelCostRates};

pub(super) fn merge_compat(base: Option<&ModelCompat>, override_: &ModelCompat) -> ModelCompat {
    let base_value = base.and_then(|base| serde_json::to_value(base).ok());
    let override_value = serde_json::to_value(override_).ok();
    let (Some(base_value), Some(override_value)) = (base_value, override_value) else {
        return override_.clone();
    };
    let (Some(base_map), Some(override_map)) = (base_value.as_object(), override_value.as_object())
    else {
        return override_.clone();
    };
    let mut merged = base_map.clone();
    for (key, value) in override_map {
        merged.insert(key.clone(), value.clone());
    }
    for key in [
        "openRouterRouting",
        "vercelGatewayRouting",
        "chatTemplateKwargs",
        "chatTemplateArgs",
    ] {
        let base_nested = base_map.get(key).and_then(serde_json::Value::as_object);
        let override_nested = override_map.get(key).and_then(serde_json::Value::as_object);
        if base_nested.is_some() || override_nested.is_some() {
            let mut nested = base_nested.cloned().unwrap_or_default();
            if let Some(override_nested) = override_nested {
                for (key, value) in override_nested {
                    nested.insert(key.clone(), value.clone());
                }
            }
            merged.insert(key.to_owned(), serde_json::Value::Object(nested));
        }
    }
    serde_json::from_value(serde_json::Value::Object(merged)).unwrap_or_else(|_| override_.clone())
}

fn merge_input_limits(
    base: Option<&crate::types::ModelInputLimits>,
    override_: &crate::types::ModelInputLimits,
) -> crate::types::ModelInputLimits {
    let images = match &override_.images {
        Some(override_images) => {
            let base_images = base.and_then(|limits| limits.images);
            let resize = match &override_images.resize {
                Some(override_resize) => match base_images.and_then(|images| images.resize) {
                    Some(base_resize) => Some(crate::types::ModelImageResizeOptions {
                        max_width: override_resize.max_width.or(base_resize.max_width),
                        max_height: override_resize.max_height.or(base_resize.max_height),
                        max_bytes: override_resize.max_bytes.or(base_resize.max_bytes),
                        jpeg_quality: override_resize.jpeg_quality.or(base_resize.jpeg_quality),
                    }),
                    None => Some(*override_resize),
                },
                None => base_images.and_then(|images| images.resize),
            };
            Some(crate::types::ModelImageInputLimits {
                max_per_message: override_images
                    .max_per_message
                    .or(base_images.and_then(|images| images.max_per_message)),
                max_per_request: override_images
                    .max_per_request
                    .or(base_images.and_then(|images| images.max_per_request)),
                resize,
            })
        }
        None => base.and_then(|limits| limits.images),
    };
    crate::types::ModelInputLimits {
        max_request_bytes: override_
            .max_request_bytes
            .or(base.and_then(|limits| limits.max_request_bytes)),
        images,
    }
}

pub(super) fn apply_model_override(mut model: Model, override_: &ModelsJsonModelOverride) -> Model {
    if let Some(name) = &override_.name {
        model.name = name.clone();
    }
    if let Some(reasoning) = override_.reasoning {
        model.reasoning = reasoning;
    }
    if let Some(thinking_level_map) = &override_.thinking_level_map {
        model.thinking_level_map = Some(match model.thinking_level_map.take() {
            Some(mut base) => {
                base.extend(thinking_level_map.clone());
                base
            }
            None => thinking_level_map.clone(),
        });
    }
    if let Some(input) = &override_.input {
        model.input = input.clone();
    }
    if let Some(override_limits) = &override_.input_limits {
        model.input_limits = Some(merge_input_limits(
            model.input_limits.as_ref(),
            override_limits,
        ));
    }
    if let Some(cost) = &override_.cost {
        model.cost = ModelCost {
            rates: ModelCostRates {
                input: cost.input.unwrap_or(model.cost.rates.input),
                output: cost.output.unwrap_or(model.cost.rates.output),
                cache_read: cost.cache_read.unwrap_or(model.cost.rates.cache_read),
                cache_write: cost.cache_write.unwrap_or(model.cost.rates.cache_write),
            },
            tiers: cost.tiers.clone().or_else(|| model.cost.tiers.take()),
        };
    }
    if let Some(override_prompt_cache) = &override_.prompt_cache {
        model.prompt_cache = Some(match model.prompt_cache.take() {
            Some(mut base) => {
                base.short = override_prompt_cache.short.or(base.short);
                base.long = override_prompt_cache.long.or(base.long);
                base
            }
            None => crate::types::ModelPromptCache {
                short: override_prompt_cache.short,
                long: override_prompt_cache.long,
            },
        });
    }
    if let Some(context_window) = override_.context_window {
        model.context_window = context_window as u32;
    }
    if let Some(max_tokens) = override_.max_tokens {
        model.max_tokens = max_tokens as u32;
    }
    if let Some(override_sampling) = &override_.sampling_params {
        model.sampling_params = Some(match model.sampling_params.take() {
            Some(mut base) => {
                base.extend(override_sampling.clone());
                base
            }
            None => override_sampling.clone(),
        });
    }
    if let Some(override_levels) = &override_.sampling_params_by_thinking_level {
        let mut merged = model
            .sampling_params_by_thinking_level
            .take()
            .unwrap_or_default();
        for (level, params) in override_levels {
            merged.entry(*level).or_default().extend(params.clone());
        }
        model.sampling_params_by_thinking_level = Some(merged);
    }
    if let Some(compat) = &override_.compat {
        model.compat = Some(merge_compat(model.compat.as_ref(), compat));
    }
    if let Some(override_headers) = &override_.headers {
        let mut headers: BTreeMap<String, String> = override_headers.clone().into_iter().collect();
        if let Some(existing) = model.headers.take() {
            headers.extend(existing);
        }
        model.headers = Some(headers);
    }
    model
