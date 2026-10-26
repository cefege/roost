//! Port of `packages/coding-agent/src/core/model-config.ts` @ pi 0.82.1
//! (2efa728) — user `models.json` loading (placed in `rpi-ai` per the T03
//! scope split).
//!
//! Intentional differences (upstream deviations):
//! - Schema validation uses serde plus a small manual pass, not TypeBox;
//!   error wording and paths differ (registered as D-006), but the three
//!   failure classes (load / parse / schema) and their message prefixes are
//!   preserved verbatim.
//! - The upstream per-API compat union is validated as the flat
//!   [`ModelCompat`] struct (the same merged struct the wire types use), which
//!   is more permissive than the TypeBox union.
//! - `deepFreeze`/`structuredClone` are JS-isms; Rust values are immutable
//!   while borrowed, so no equivalent is needed.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::types::{InputModality, ModelCompat, ModelCost, ModelCostTier, ThinkingLevelMap};

// ---------------------------------------------------------------------------
// Ordered map
// ---------------------------------------------------------------------------

/// Insertion-ordered string-keyed map, serialized as a JSON object. Upstream
/// `Record`/`Map` key order is observable (`getProviderIds`), so a `HashMap`
/// will not do.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderedMap<T>(Vec<(String, T)>);

impl<T> Default for OrderedMap<T> {
    fn default() -> Self {
        OrderedMap(Vec::new())
    }
}

impl<T> OrderedMap<T> {
    pub fn get(&self, key: &str) -> Option<&T> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.0.iter().map(|(k, _)| k)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &T)> {
        self.0.iter().map(|(k, v)| (k, v))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.0.iter().map(|(_, v)| v)
    }

    /// Upsert: an existing key keeps its position (JS `Map.set` semantics).
    pub fn insert(&mut self, key: String, value: T) -> Option<T> {
        for (k, v) in &mut self.0 {
            if k == &key {
                return Some(std::mem::replace(v, value));
            }
        }
        self.0.push((key, value));
        None
    }

    pub fn remove(&mut self, key: &str) -> Option<T> {
        let position = self.0.iter().position(|(k, _)| k == key)?;
        Some(self.0.remove(position).1)
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }
}

impl<T: Serialize> Serialize for OrderedMap<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for OrderedMap<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor<T>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Visitor<T> {
            type Value = OrderedMap<T>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a JSON object")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut access: A,
            ) -> Result<Self::Value, A::Error> {
                let mut entries = Vec::with_capacity(access.size_hint().unwrap_or(0));
                while let Some((key, value)) = access.next_entry::<String, T>()? {
                    entries.push((key, value));
                }
                Ok(OrderedMap(entries))
            }
        }
        deserializer.deserialize_map(Visitor(std::marker::PhantomData))
    }
}

// ---------------------------------------------------------------------------
// JSONC
// ---------------------------------------------------------------------------

/// `stripJsonComments`: removes `//` line comments and trailing commas, both
/// only outside string literals.
pub fn strip_json_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => {
                    // Escaped char: copy verbatim, never ends the string.
                    if let Some(next) = chars.next() {
                        out.push(next);
                    }
                }
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                // Line comment: drop through (not including) the newline.
                for next in chars.by_ref() {
                    if next == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ',' => {
                // Trailing comma: drop when only whitespace follows before a
                // closing `}`/`]`. Whitespace is preserved either way.
                let mut lookahead = String::new();
                let is_trailing = loop {
                    match chars.peek() {
                        Some(next) if next.is_whitespace() => {
                            lookahead.push(*next);
                            chars.next();
                        }
                        Some('}') | Some(']') => break true,
                        _ => break false,
                    }
                };
                if !is_trailing {
                    out.push(',');
                }
                out.push_str(&lookahead);
            }
            _ => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

/// `ModelOverrideSchema.cost` — every rate optional (partial override).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJsonModelOverrideCost {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
    pub tiers: Option<Vec<ModelCostTier>>,
}

/// `ModelPromptCacheSchema` (#9668, c596d09d9; model-config.ts:139-142):
/// best-effort prompt cache lifetime in seconds per retention tier.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJsonPromptCache {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub long: Option<u32>,
}

/// `ModelOverrideSchema`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJsonModelOverride {
    pub name: Option<String>,
    pub reasoning: Option<bool>,
    pub thinking_level_map: Option<ThinkingLevelMap>,
    pub input: Option<Vec<InputModality>>,
    /// `ModelOverrideSchema.inputLimits` (#9631): merged over the catalog
    /// value (`mergeInputLimits`, provider-composer.ts:110-130). Same wire
    /// shape as the catalog type (model-config.ts:143-158).
    pub input_limits: Option<crate::types::ModelInputLimits>,
    pub cost: Option<ModelsJsonModelOverrideCost>,
    /// `ModelOverrideSchema.promptCache` (#9668): merged per tier with the
    /// catalog value.
    pub prompt_cache: Option<ModelsJsonPromptCache>,
    pub context_window: Option<f64>,
    pub max_tokens: Option<f64>,
    /// 25a2c8dcf (#7568): default sampling parameters for this model.
    pub sampling_params: Option<serde_json::Map<String, Value>>,
    /// `ModelOverrideSchema.samplingParamsByThinkingLevel` (v1.0.2, #9776;
    /// R2.3.1): merged per level and per key over the catalog value.
    pub sampling_params_by_thinking_level: Option<crate::types::SamplingParamsByThinkingLevel>,
    pub headers: Option<HashMap<String, String>>,
    pub compat: Option<ModelCompat>,
}

/// `ModelDefinitionSchema`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJsonModel {
    pub id: String,
    pub name: Option<String>,
    pub api: Option<String>,
    pub base_url: Option<String>,
    pub reasoning: Option<bool>,
    pub thinking_level_map: Option<ThinkingLevelMap>,
    pub input: Option<Vec<InputModality>>,
    /// `ModelDefinitionSchema.inputLimits` (#9631): carried verbatim onto
    /// the runtime model (`modelFromJson`, provider-composer.ts:163).
    pub input_limits: Option<crate::types::ModelInputLimits>,
    pub cost: Option<ModelCost>,
    /// `ModelDefinitionSchema.promptCache` (#9668): unset disables cache
    /// warming for the model.
    pub prompt_cache: Option<ModelsJsonPromptCache>,
    pub context_window: Option<f64>,
    pub max_tokens: Option<f64>,
    /// 25a2c8dcf (#7568): default sampling parameters for this model.
    pub sampling_params: Option<serde_json::Map<String, Value>>,
    /// `ModelDefinitionSchema.samplingParamsByThinkingLevel` (v1.0.2,
    /// #9776; R2.3.1): carried verbatim onto the runtime model.
    pub sampling_params_by_thinking_level: Option<crate::types::SamplingParamsByThinkingLevel>,
    pub headers: Option<HashMap<String, String>>,
    pub compat: Option<ModelCompat>,
}

/// `ProviderConfigSchema`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJsonProvider {
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub api: Option<String>,
    pub oauth: Option<String>,
    pub headers: Option<HashMap<String, String>>,
    pub compat: Option<ModelCompat>,
    pub auth_header: Option<bool>,
    pub models: Option<Vec<ModelsJsonModel>>,
    /// Insertion-ordered (upstream `Record<string, ModelOverrideSchema>` keeps
    /// key insertion order).
    pub model_overrides: Option<OrderedMap<ModelsJsonModelOverride>>,
}

/// `ModelsConfigSchema`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelsJson {
    /// Insertion-ordered (upstream `Map` preserves key insertion order in
    /// `getProviderIds`).
    pub providers: OrderedMap<ModelsJsonProvider>,
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn check_min_length(errors: &mut Vec<String>, path: &str, value: &Option<String>) {
    if let Some(value) = value
        && value.is_empty()
    {
        errors.push(format!("  - {path}: must NOT have fewer than 1 characters"));
    }
}

/// The manual validation pass: `minLength: 1` string fields and the `oauth`
/// literal. Type/shape errors are already caught by serde.
fn validate(config: &ModelsJson) -> Vec<String> {
    let mut errors = Vec::new();
    for (provider_id, provider) in config.providers.iter() {
        let base = format!("providers.{provider_id}");
        check_min_length(&mut errors, &format!("{base}.name"), &provider.name);
        check_min_length(&mut errors, &format!("{base}.baseUrl"), &provider.base_url);
        check_min_length(&mut errors, &format!("{base}.apiKey"), &provider.api_key);
        check_min_length(&mut errors, &format!("{base}.api"), &provider.api);
        if let Some(oauth) = &provider.oauth
            && oauth != "radius"
        {
            errors.push(format!("  - {base}.oauth: must be equal to constant"));
        }
        for (index, model) in provider.models.as_deref().unwrap_or(&[]).iter().enumerate() {
            let base = format!("{base}.models.{index}");
            if model.id.is_empty() {
                errors.push(format!(
                    "  - {base}.id: must NOT have fewer than 1 characters"
                ));
            }
            check_min_length(&mut errors, &format!("{base}.name"), &model.name);
            check_min_length(&mut errors, &format!("{base}.api"), &model.api);
            check_min_length(&mut errors, &format!("{base}.baseUrl"), &model.base_url);
        }
        if let Some(model_overrides) = &provider.model_overrides {
            for (model_id, model_override) in model_overrides.iter() {
                check_min_length(
                    &mut errors,
                    &format!("{base}.modelOverrides.{model_id}.name"),
                    &model_override.name,
                );
            }
        }
    }
    errors
}

// ---------------------------------------------------------------------------
// ModelConfig
// ---------------------------------------------------------------------------

/// `ModelConfig` — one immutable load of models.json. Loading never fails
/// hard: a missing path or missing file yields an empty config; load/parse/
/// schema problems yield an empty config plus [`ModelConfig::error`].
///
/// `diagnostics` carries non-fatal rpi-own findings (Azure rename legacy
/// keys, R2.2.2): the config still loads and the finding is surfaced through
/// `ModelRuntime::get_error()`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelConfig {
    providers: OrderedMap<ModelsJsonProvider>,
    error: Option<String>,
    diagnostics: Vec<String>,
}

impl ModelConfig {
    fn empty() -> Self {
        Self::default()
    }

    fn with_error(error: String) -> Self {
        Self {
            providers: OrderedMap::default(),
            error: Some(error),
            diagnostics: Vec::new(),
        }
    }

    /// `ModelConfig.load`.
    pub async fn load(models_json_path: Option<&Path>) -> Self {
        let Some(path) = models_json_path else {
            return Self::empty();
        };
        let content = match tokio::fs::read_to_string(path).await {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Self::empty(),
            Err(error) => {
                return Self::with_error(format!(
                    "Failed to load models.json: {error}\n\nFile: {}",
                    path.display()
                ));
            }
        };
        let stripped = strip_json_comments(&content);
        let parsed: Value = match serde_json::from_str(&stripped) {
            Ok(parsed) => parsed,
            Err(error) => {
                return Self::with_error(format!(
                    "Failed to parse models.json: {error}\n\nFile: {}",
                    path.display()
                ));
            }
        };
        // R2.2.2 (V17-01): legacy Azure provider keys are reported with the
        // file and key position; the document still loads (no rewrite, no
        // alias).
        let diagnostics = crate::azure_provider_rename::models_json_diagnostics(
            &path.display().to_string(),
            &parsed,
        );
        let config: ModelsJson = match serde_json::from_value(parsed) {
            Ok(config) => config,
            Err(error) => {
                return Self::with_error(format!(
                    "Invalid models.json schema:\n  - {error}\n\nFile: {}",
                    path.display()
                ));
            }
        };
        let errors = validate(&config);
        if !errors.is_empty() {
            return Self::with_error(format!(
                "Invalid models.json schema:\n{}\n\nFile: {}",
                errors.join("\n"),
                path.display()
            ));
        }
        Self {
            providers: config.providers,
            error: None,
            diagnostics,
        }
    }

    /// `getProvider`.
    pub fn get_provider(&self, provider_id: &str) -> Option<&ModelsJsonProvider> {
        self.providers.get(provider_id)
    }

    /// `getProviderIds` — insertion order preserved.
    pub fn provider_ids(&self) -> Vec<&str> {
        self.providers.keys().map(String::as_str).collect()
    }

    /// `getError`.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// V17-01 R2.2.2: non-fatal diagnostics collected at load time.
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_json_comments() {
        assert_eq!(strip_json_comments("{}"), "{}");
        // Line comments outside strings are removed; leading whitespace and
        // the newline survive (upstream replaces the comment match with "").
        assert_eq!(
            strip_json_comments("{\n  // comment\n  \"a\": 1\n}"),
            "{\n  \n  \"a\": 1\n}"
        );
        // `//` inside strings is preserved, including escaped quotes.
        assert_eq!(
            strip_json_comments("{\"a\": \"http://x\\\"//y\"}"),
            "{\"a\": \"http://x\\\"//y\"}"
        );
        // Trailing commas before } or ] are dropped, whitespace kept.
        assert_eq!(strip_json_comments("{\"a\": 1,}"), "{\"a\": 1}");
        assert_eq!(strip_json_comments("[1, 2,\n]"), "[1, 2\n]");
        assert_eq!(
            strip_json_comments("{\"a\": [1,], \"b\": 2,}"),
            "{\"a\": [1], \"b\": 2}"
        );
        // Non-trailing commas are kept.
        assert_eq!(strip_json_comments("[1, 2]"), "[1, 2]");
        // A comma inside a string is never treated as trailing.
        assert_eq!(strip_json_comments("{\",\" : 1}"), "{\",\" : 1}");
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("rpi-models-json-{}-{name}", std::process::id()))
    }

    #[tokio::test]
    async fn test_load_missing_path_and_file() {
        let config = ModelConfig::load(None).await;
        assert_eq!(config.error(), None);
        assert_eq!(config.provider_ids(), Vec::<&str>::new());

        let config = ModelConfig::load(Some(&temp_path("enoent"))).await;
        assert_eq!(config.error(), None);
        assert_eq!(config.provider_ids(), Vec::<&str>::new());
    }

    /// V17-01 R2.2.2: a legacy Azure provider key is diagnosed with file and
    /// key position without failing the load; the new key stays silent.
    #[tokio::test]
    async fn test_load_legacy_azure_provider_diagnostic() {
        let path = temp_path("azure-legacy");
        std::fs::write(
            &path,
            r#"{"providers": {"azure-openai-responses": {"baseUrl": "https://x"}, "azure": {"baseUrl": "https://y"}}}"#,
        )
        .expect("write");
        let config = ModelConfig::load(Some(&path)).await;
        assert_eq!(config.error(), None, "legacy key must not fail the load");
        let diagnostics = config.diagnostics();
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert!(diagnostics[0].contains("providers.azure-openai-responses"));
        assert!(diagnostics[0].contains(&path.display().to_string()));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn test_load_parse_error() {
        let path = temp_path("parse");
        std::fs::write(&path, "{not json").expect("write");
        let config = ModelConfig::load(Some(&path)).await;
        let error = config.error().expect("error");
        assert!(
            error.starts_with("Failed to parse models.json: "),
            "{error}"
        );
        assert!(error.contains(&format!("File: {}", path.display())));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn test_load_schema_error() {
        let path = temp_path("schema");
        // `providers` must be an object; empty model id violates minLength.
        std::fs::write(&path, "{\"providers\": []}").expect("write");
        let config = ModelConfig::load(Some(&path)).await;
        let error = config.error().expect("error");
        assert!(
            error.starts_with("Invalid models.json schema:\n"),
            "{error}"
        );

        std::fs::write(
            &path,
            "{\"providers\": {\"openai\": {\"models\": [{\"id\": \"\"}]}}}",
        )
        .expect("write");
        let config = ModelConfig::load(Some(&path)).await;
        let error = config.error().expect("error");
        assert!(
            error.contains("providers.openai.models.0.id: must NOT have fewer than 1 characters"),
            "{error}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn test_load_valid_jsonc() {
        let path = temp_path("valid");
        std::fs::write(
            &path,
            r#"{
            // user providers
            "providers": {
                "openai": {
                    "name": "OpenAI",
                    "apiKey": "sk-test",
                    "authHeader": true,
                    "models": [
                        {
                            "id": "gpt-4o",
                            "reasoning": false,
                            "input": ["text", "image"],
                            "cost": {"input": 2.5, "output": 10.0, "cacheRead": 1.25, "cacheWrite": 2.5},
                        }
                    ],
                    "modelOverrides": {
                        "gpt-4o": {"contextWindow": 128000}
                    }
                },
                "anthropic": {"oauth": "radius"},
            }
        }"#,
        )
        .expect("write");
        let config = ModelConfig::load(Some(&path)).await;
        assert_eq!(config.error(), None);
        // Insertion order preserved.
        assert_eq!(config.provider_ids(), vec!["openai", "anthropic"]);
        let provider = config.get_provider("openai").expect("provider");
        assert_eq!(provider.name.as_deref(), Some("OpenAI"));
        assert_eq!(provider.api_key.as_deref(), Some("sk-test"));
        assert_eq!(provider.auth_header, Some(true));
        let models = provider.models.as_ref().expect("models");
        assert_eq!(models[0].id, "gpt-4o");
        assert_eq!(
            models[0].input,
            Some(vec![InputModality::Text, InputModality::Image])
        );
        let overrides = provider.model_overrides.as_ref().expect("overrides");
        assert_eq!(
            overrides.get("gpt-4o").and_then(|o| o.context_window),
            Some(128000.0)
        );
        assert_eq!(
            config
                .get_provider("anthropic")
                .and_then(|p| p.oauth.as_deref()),
            Some("radius")
        );
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn test_load_oauth_literal_validation() {
        let path = temp_path("oauth");
        std::fs::write(&path, "{\"providers\": {\"x\": {\"oauth\": \"github\"}}}").expect("write");
        let config = ModelConfig::load(Some(&path)).await;
        let error = config.error().expect("error");
        assert!(error.contains("providers.x.oauth"), "{error}");
        let _ = std::fs::remove_file(&path);
    }
}
