//! Converts Roost catalog entries into Pi chat and classifier model records.
//! Called while `durable::model` registers providers in Pi's model collection.
//! Catalog metadata comes from `roost-llm`; classifier inputs stay text-only.

use roost_llm::{ModelInfo, WireApi};
use rpi_ai::types::{
    ApiKind, ClassifierModel, InputModality, Model, ModelCost, ModelCostRates, ModelThinkingLevel,
};
pub(super) fn pi_model(info: &ModelInfo) -> Model {
    let api = api_name(info.api);
    let thinking_level_map = if info.thinking_level_map.is_empty() {
        None
    } else {
        Some(
            info.thinking_level_map
                .iter()
                .filter_map(|(level, mapped)| {
                    let level = match level.as_str() {
                        "off" => ModelThinkingLevel::Off,
                        "minimal" => ModelThinkingLevel::Minimal,
                        "low" => ModelThinkingLevel::Low,
                        "medium" => ModelThinkingLevel::Medium,
                        "high" => ModelThinkingLevel::High,
                        "xhigh" => ModelThinkingLevel::Xhigh,
                        "max" => ModelThinkingLevel::Max,
                        _ => return None,
                    };
                    Some((level, mapped.clone()))
                })
                .collect(),
        )
    };
    Model {
        id: info.id.clone(),
        name: info.name.clone(),
        api: api.into(),
        provider: info.provider.clone(),
        base_url: info.base_url.clone(),
        reasoning: info.reasoning,
        thinking_level_map,
        input: if info.images {
            vec![
                rpi_ai::types::InputModality::Text,
                rpi_ai::types::InputModality::Image,
            ]
        } else {
            vec![rpi_ai::types::InputModality::Text]
        },
        input_limits: None,
        prompt_cache: None,
        cost: ModelCost {
            rates: ModelCostRates {
                input: info.cost.input,
                output: info.cost.output,
                cache_read: info.cost.cache_read,
                cache_write: info.cost.cache_write,
            },
            tiers: None,
        },
        context_window: info.context_window.min(u64::from(u32::MAX)) as u32,
        max_tokens: info.max_tokens.min(u64::from(u32::MAX)) as u32,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        headers: None,
        compat: None,
    }
}

pub(super) fn pi_classifier(info: &ModelInfo) -> ClassifierModel {
    ClassifierModel {
        model_type: rpi_ai::types::ModelType::Classifier,
        id: info.id.clone(),
        name: info.name.clone(),
        api: ApiKind(api_name(info.api).to_owned()),
        provider: info.provider.clone(),
        base_url: info.base_url.clone(),
        input: vec![InputModality::Text],
        input_limits: None,
        cost: ModelCost {
            rates: ModelCostRates {
                input: info.cost.input,
                output: info.cost.output,
                cache_read: info.cost.cache_read,
                cache_write: info.cost.cache_write,
            },
            tiers: None,
        },
        context_window: info.context_window.min(u64::from(u32::MAX)) as u32,
        headers: None,
    }
}

fn api_name(api: WireApi) -> &'static str {
    match api {
        WireApi::AnthropicMessages => "anthropic-messages",
        WireApi::OpenAiCodexResponses => "openai-codex-responses",
        WireApi::OpenAiCompletions => "openai-completions",
        WireApi::TypesafeSystemOne => "typesafe-system-one",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifier_catalog_entries_are_typed_and_text_only() {
        let info = ModelInfo {
            provider: "openrouter".to_owned(),
            id: "jev-1".to_owned(),
            name: "JEV 1".to_owned(),
            api: WireApi::TypesafeSystemOne,
            base_url: "https://example.test/v1".to_owned(),
            reasoning: false,
            images: true,
            cost: Default::default(),
            context_window: 8_192,
            max_tokens: 512,
            thinking_level_map: Default::default(),
            kind: roost_llm::ModelKind::Classifier,
            compat: serde_json::Value::Null,
            intelligence: None,
            tokens_per_second: None,
            runner_kind: "judge".to_owned(),
            priority: None,
            pricing_status: None,
        };
        let classifier = pi_classifier(&info);
        assert_eq!(classifier.model_type, rpi_ai::types::ModelType::Classifier);
        assert_eq!(classifier.api, ApiKind("typesafe-system-one".to_owned()));
        assert_eq!(classifier.provider, "openrouter");
        assert_eq!(classifier.input, vec![InputModality::Text]);
    }
}
