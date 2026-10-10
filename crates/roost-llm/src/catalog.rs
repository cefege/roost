//! Ported from pi-ai 1.1.0 dist/providers/data/*.json and dist/models.js (MIT).
//! The model catalog: the four providers' JSON compiled in, parsed once, and
//! the thinking-level rules (`supported_thinking_levels`, `clamp_thinking_level`).
//! Read by the provider clients, the account pool and roost-agent's role resolver.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::message::Usage;

/// Every thinking level a model can be asked for, cheapest first. `auto` is
/// not here: roost-agent resolves it to one of these through the judge.
pub const THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

const CATALOG_SOURCES: [&str; 4] = [
    include_str!("../catalog/anthropic.json"),
    include_str!("../catalog/openai-codex.json"),
    include_str!("../catalog/openrouter.json"),
    include_str!("../catalog/typesafe.json"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WireApi {
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
    #[serde(rename = "openai-codex-responses")]
    OpenAiCodexResponses,
    #[serde(rename = "openai-completions")]
    OpenAiCompletions,
    #[serde(rename = "typesafe-system-one")]
    TypesafeSystemOne,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    Chat,
    Classifier,
}

/// USD per million tokens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub provider: String,
    pub id: String,
    pub name: String,
    pub api: WireApi,
    pub base_url: String,
    pub reasoning: bool,
    pub images: bool,
    pub cost: Cost,
    pub context_window: u64,
    pub max_tokens: u64,
    /// Level → provider value. `None` marks a level the model rejects; an
    /// absent `xhigh`/`max` is unsupported, an absent lower level is identity.
    pub thinking_level_map: BTreeMap<String, Option<String>>,
    pub kind: ModelKind,
    /// The catalog's `compat` flags (`forceAdaptiveThinking`, `thinkingFormat`, …).
    pub compat: serde_json::Value,
}

impl ModelInfo {
    /// The levels this model accepts, in `THINKING_LEVELS` order.
    pub fn supported_thinking_levels(&self) -> Vec<&'static str> {
        if !self.reasoning {
            return vec!["off"];
        }
        THINKING_LEVELS
            .into_iter()
            .filter(|level| match self.thinking_level_map.get(*level) {
                Some(None) => false,
                Some(Some(_)) => true,
                None => !matches!(*level, "xhigh" | "max"),
            })
            .collect()
    }

    /// The nearest supported level: the requested one, else the next higher,
    /// else the next lower.
    pub fn clamp_thinking_level(&self, level: &str) -> &'static str {
        let available = self.supported_thinking_levels();
        let fallback = available.first().copied().unwrap_or("off");
        let Some(requested) = THINKING_LEVELS.iter().position(|known| *known == level) else {
            return fallback;
        };
        THINKING_LEVELS[requested..]
            .iter()
            .chain(THINKING_LEVELS[..requested].iter().rev())
            .find(|candidate| available.contains(candidate))
            .copied()
            .unwrap_or(fallback)
    }

    /// The provider's value for a (clamped) level: the mapped value, or the
    /// level name itself when the map does not rename it.
    pub fn provider_thinking_value(&self, level: &str) -> Option<String> {
        match self.thinking_level_map.get(level) {
            Some(mapped) => mapped.clone(),
            None if level == "off" => None,
            None => Some(level.to_owned()),
        }
    }

    pub fn compat_flag(&self, name: &str) -> bool {
        self.compat
            .get(name)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }

    pub fn compat_str(&self, name: &str) -> Option<&str> {
        self.compat.get(name).and_then(serde_json::Value::as_str)
    }

    /// The USD cost of one call's usage.
    pub fn cost_usd(&self, usage: &Usage) -> f64 {
        let per_token = |rate: f64, tokens: u64| rate * tokens as f64 / 1_000_000.0;
        per_token(self.cost.input, usage.input)
            + per_token(self.cost.output, usage.output)
            + per_token(self.cost.cache_read, usage.cache_read)
            + per_token(self.cost.cache_write, usage.cache_write)
    }
}

/// The parsed catalog of every supported model.
#[derive(Debug, Clone)]
pub struct Catalog {
    models: Vec<ModelInfo>,
}

impl Catalog {
    /// The compiled-in catalog. Entries whose `api` is not one of the four
    /// supported wire APIs (OpenRouter's image models) are skipped.
    pub fn builtin() -> Self {
        let mut models = Vec::new();
        for source in CATALOG_SOURCES {
            models.extend(parse_catalog_source(source));
        }
        Self { models }
    }

    pub fn models(&self) -> &[ModelInfo] {
        &self.models
    }

    pub fn get(&self, provider: &str, id: &str) -> Option<&ModelInfo> {
        self.models
            .iter()
            .find(|model| model.provider == provider && model.id == id)
    }

    pub fn chat_models<'a>(&'a self, provider: &'a str) -> impl Iterator<Item = &'a ModelInfo> {
        self.models
            .iter()
            .filter(move |model| model.provider == provider && model.kind == ModelKind::Chat)
    }

    /// Provider ids in catalog order, each once.
    pub fn providers(&self) -> Vec<&str> {
        let mut providers: Vec<&str> = Vec::new();
        for model in &self.models {
            if !providers.contains(&model.provider.as_str()) {
                providers.push(&model.provider);
            }
        }
        providers
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawModel {
    id: String,
    name: String,
    api: String,
    provider: String,
    base_url: String,
    #[serde(default)]
    reasoning: bool,
    #[serde(default)]
    input: Vec<String>,
    #[serde(default)]
    cost: RawCost,
    #[serde(default)]
    context_window: u64,
    #[serde(default)]
    max_tokens: u64,
    #[serde(default)]
    thinking_level_map: BTreeMap<String, Option<String>>,
    #[serde(default)]
    compat: serde_json::Value,
    #[serde(rename = "type", default)]
    model_type: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCost {
    #[serde(default)]
    input: f64,
    #[serde(default)]
    output: f64,
    #[serde(default)]
    cache_read: f64,
    #[serde(default)]
    cache_write: f64,
}

fn parse_catalog_source(source: &str) -> Vec<ModelInfo> {
    let parsed: BTreeMap<String, BTreeMap<String, serde_json::Value>> =
        match serde_json::from_str(source) {
            Ok(parsed) => parsed,
            Err(error) => {
                tracing::error!(%error, "model catalog JSON does not parse");
                return Vec::new();
            }
        };
    let mut models = Vec::new();
    for entries in parsed.into_values() {
        for (key, value) in entries {
            let raw: RawModel = match serde_json::from_value(value) {
                Ok(raw) => raw,
                Err(error) => {
                    tracing::warn!(%key, %error, "skipping malformed catalog entry");
                    continue;
                }
            };
            let api: WireApi = match serde_json::from_value(serde_json::Value::String(raw.api)) {
                Ok(api) => api,
                Err(_) => continue,
            };
            let kind = match raw.model_type.as_deref() {
                Some("classifier") => ModelKind::Classifier,
                _ if api == WireApi::TypesafeSystemOne => ModelKind::Classifier,
                _ => ModelKind::Chat,
            };
            models.push(ModelInfo {
                provider: raw.provider,
                id: raw.id,
                name: raw.name,
                api,
                base_url: raw.base_url,
                reasoning: raw.reasoning,
                images: raw.input.iter().any(|input| input == "image"),
                cost: Cost {
                    input: raw.cost.input,
                    output: raw.cost.output,
                    cache_read: raw.cost.cache_read,
                    cache_write: raw.cost.cache_write,
                },
                context_window: raw.context_window,
                max_tokens: raw.max_tokens,
                thinking_level_map: raw.thinking_level_map,
                kind,
                compat: raw.compat,
            });
        }
    }
    models
}
#[cfg(test)]
mod tests {
    use super::{Catalog, WireApi, parse_catalog_source};

    #[test]
    fn unsupported_openrouter_image_models_are_skipped() {
        let source = r#"{"openrouter-images":{"image:test":{"id":"image:test","name":"image","api":"openrouter-images","provider":"openrouter","baseUrl":"https://example.invalid"}}}"#;
        assert!(parse_catalog_source(source).is_empty());
        assert!(
            Catalog::builtin()
                .models()
                .iter()
                .all(|model| model.api != WireApi::TypesafeSystemOne
                    || model.kind == super::ModelKind::Classifier)
        );
    }

    #[test]
    fn codex_model_exposes_its_catalog_thinking_levels() {
        let catalog = Catalog::builtin();
        let model = catalog
            .get("openai-codex", "gpt-5.3-codex-spark")
            .expect("pinned model catalog entry exists");
        assert_eq!(model.api, WireApi::OpenAiCodexResponses);
        assert!(model.supported_thinking_levels().contains(&"xhigh"));
        assert!(model.supported_thinking_levels().contains(&"minimal"));
        assert_eq!(
            model.provider_thinking_value("minimal").as_deref(),
            Some("low")
        );
    }
}
