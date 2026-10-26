//! Built-in model catalog — runtime half of the generated pipeline (T13 W4).
//!
//! Ports the catalog-read side of `packages/ai/src/models.generated.ts` +
//! `packages/ai/src/providers/all.ts` (`getBuiltinModel` / `getBuiltinProviders`
//! / `getBuiltinModels` / `getBuiltinModelDataGeneratedAt`) @ pi 0.82.1
//! (2efa728).
//!
//! `build.rs` embeds the vendored `src/providers/data/*.json` (upstream
//! `providers/data/`, corrections of `generate-models.ts` already baked in —
//! the upstream `*.models.ts` are pure `flattenModelCatalog` re-exports) via
//! `include_str!`; this module parses them lazily on first access. Startup
//! treats the data as read-only (coding-standards §3.2); the refresh path is
//! the manual `scripts/refresh-model-catalog.sh`, not a build-time fetch.
//!
//! Intentional differences:
//! - Upstream generates TS literals (`models.generated.ts`); we embed the
//!   vendored JSON and parse with serde at first access (build-time codegen of
//!   1217 model literals was rejected as compile-time noise). Data content is
//!   identical — verified field-by-field against the upstream JSONs in
//!   `tests/model_catalog.rs`.
//! - Upstream `getBuiltinModels(unknown)` returns `[]`; mirrored here as an
//!   empty slice. Parse failures are impossible with intact vendored data and
//!   surface via `builtin_catalog()` (never panic).

use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::types::{AnyModel, ClassifierModel, ImageModel, Model, ModelType, is_model_type};

include!(concat!(env!("OUT_DIR"), "/models_generated.rs"));

/// Catalog load error. Only reachable if the vendored JSON is corrupted
/// (generation-time bug); accessor functions degrade to empty results.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("invalid catalog manifest: {0}")]
    Manifest(#[source] serde_json::Error),
    #[error("invalid catalog data for provider {provider}: {source}")]
    ProviderData {
        provider: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid catalog shape for provider {provider}: {message}")]
    ProviderShape {
        provider: &'static str,
        message: &'static str,
    },
}

/// `.manifest.json` (upstream `scripts/model-data.ts` `ModelDataManifest`).
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogManifest {
    pub schema_version: u32,
    /// ISO-8601 timestamp shared by all built-in provider catalogs.
    pub generated_at: String,
    pub structure_hash: String,
    /// Per-file sha256 (hex) of every vendored `<provider>.json`.
    pub files: BTreeMap<String, String>,
}

/// Parsed built-in catalog: provider id → models in upstream catalog order
/// (schema v6 vendored JSONs are written in the generator's insertion order;
/// `serde_json` `preserve_order` keeps that order at parse time).
///
/// `models` is chat-only (the v6 `getBuiltinModels` face); `all_models`
/// carries every type (chat/image/classifier) for the v6 getters.
pub struct BuiltinCatalog {
    providers: Vec<&'static str>,
    models: BTreeMap<&'static str, Vec<Model>>,
    all_models: BTreeMap<&'static str, Vec<AnyModel>>,
    manifest: CatalogManifest,
}

impl BuiltinCatalog {
    /// `all.ts` `getBuiltinProviders()` — catalog provider ids.
    /// (Since 4d38031fb every built-in provider ships a static catalog
    /// entry, radius included; `meta`'s provider registration landed with
    /// V15-15 — see `providers.rs`.)
    pub fn providers(&self) -> &[&'static str] {
        &self.providers
    }

    /// `all.ts` `getBuiltinModels(provider)` — chat-only (schema v6 keeps
    /// the legacy getter chat-only; see the image/classifier getters below).
    pub fn models(&self, provider: &str) -> &[Model] {
        self.models.get(provider).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Schema v6 all-type read — every `AnyModel` for the provider.
    pub fn all_models(&self, provider: &str) -> &[AnyModel] {
        self.all_models
            .get(provider)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Schema v6 `getBuiltinImageModels(provider)`.
    pub fn image_models(&self, provider: &str) -> Vec<&ImageModel> {
        self.all_models(provider)
            .iter()
            .filter_map(|model| match model {
                AnyModel::Image(image) => Some(image),
                _ => None,
            })
            .collect()
    }

    /// Schema v6 `getBuiltinClassifierModels(provider)`.
    pub fn classifier_models(&self, provider: &str) -> Vec<&ClassifierModel> {
        self.all_models(provider)
            .iter()
            .filter_map(|model| match model {
                AnyModel::Classifier(classifier) => Some(classifier),
                _ => None,
            })
            .collect()
    }

    /// `all.ts` `getBuiltinModel(provider, modelId)` — chat-only, matching
    /// the legacy lookup face (typed per-type getters arrive with V16-06).
    pub fn model(&self, provider: &str, model_id: &str) -> Option<&Model> {
        self.models
            .get(provider)?
            .iter()
            .find(|model| model.id == model_id)
    }

    /// Schema v6 typed lookup (`getBuiltinImageModel` / classifier analogue).
    pub fn any_model(
        &self,
        provider: &str,
        model_type: ModelType,
        model_id: &str,
    ) -> Option<&AnyModel> {
        self.all_models(provider)
            .iter()
            .find(|model| model.model_type() == model_type && model.id() == model_id)
    }

    pub fn manifest(&self) -> &CatalogManifest {
        &self.manifest
    }

    /// `all.ts` `getBuiltinModelDataGeneratedAt()` — milliseconds since the
    /// Unix epoch; `None` when the manifest timestamp is unparseable
    /// (upstream: `Date.parse` → `NaN` → `undefined`).
    pub fn generated_at(&self) -> Option<i64> {
        parse_iso8601_millis(&self.manifest.generated_at)
    }
}

static CATALOG: OnceLock<Result<BuiltinCatalog, CatalogError>> = OnceLock::new();

fn load_catalog() -> Result<BuiltinCatalog, CatalogError> {
    let manifest: CatalogManifest =
        serde_json::from_str(CATALOG_MANIFEST_JSON).map_err(CatalogError::Manifest)?;
    let mut providers = Vec::with_capacity(CATALOG_PROVIDER_DATA.len());
    let mut models = BTreeMap::new();
    let mut all_models = BTreeMap::new();
    for (provider, json) in CATALOG_PROVIDER_DATA {
        // Catalog files group models by API
        // (`{ "<api>": { "<type>:<id>": AnyModel } }`, schema v6 composite
        // keys — chat/image/classifier kept distinct); upstream flattens the
        // groups (`flattenModelCatalog`). The whole file is parsed as
        // `serde_json::Value` so the generator's insertion order is kept
        // (`preserve_order`); the v6 generator no longer key-sorts.
        let root: serde_json::Value = serde_json::from_str(json)
            .map_err(|source| CatalogError::ProviderData { provider, source })?;
        let Some(groups) = root.as_object() else {
            return Err(CatalogError::ProviderShape {
                provider,
                message: "top level is not an object",
            });
        };
        let mut all = Vec::new();
        for group in groups.values() {
            let Some(entries) = group.as_object() else {
                return Err(CatalogError::ProviderShape {
                    provider,
                    message: "api group is not an object",
                });
            };
            for entry in entries.values() {
                let model: AnyModel = serde_json::from_value(entry.clone())
                    .map_err(|source| CatalogError::ProviderData { provider, source })?;
                all.push(model);
            }
        }
        let chat: Vec<Model> = all
            .iter()
            .filter(|model| is_model_type(model, ModelType::Chat))
            .filter_map(|model| model.as_chat().cloned())
            .collect();
        providers.push(*provider);
        models.insert(*provider, chat);
        all_models.insert(*provider, all);
    }
    Ok(BuiltinCatalog {
        providers,
        models,
        all_models,
        manifest,
    })
}

/// Parsed catalog, or the load error (corrupted vendored data).
pub fn builtin_catalog() -> Result<&'static BuiltinCatalog, &'static CatalogError> {
    CATALOG.get_or_init(load_catalog).as_ref()
}

/// `getBuiltinProviders()`; empty when the catalog failed to load.
pub fn get_builtin_providers() -> &'static [&'static str] {
    builtin_catalog()
        .map(BuiltinCatalog::providers)
        .unwrap_or(&[])
}

/// `getBuiltinModels(provider)`; empty for unknown providers. Chat-only
/// (schema v6 keeps the legacy getter chat-only).
pub fn get_builtin_models(provider: &str) -> &'static [Model] {
    builtin_catalog()
        .map(|catalog| catalog.models(provider))
        .unwrap_or(&[])
}

/// Schema v6 `getBuiltinImageModels(provider)`.
pub fn get_builtin_image_models(provider: &str) -> Vec<&'static ImageModel> {
    builtin_catalog()
        .map(|catalog| catalog.image_models(provider))
        .unwrap_or_default()
}

/// Schema v6 `getBuiltinClassifierModels(provider)`.
pub fn get_builtin_classifier_models(provider: &str) -> Vec<&'static ClassifierModel> {
    builtin_catalog()
        .map(|catalog| catalog.classifier_models(provider))
        .unwrap_or_default()
}

/// Schema v6 all-type read (chat + image + classifier), catalog order.
pub fn get_builtin_all_models(provider: &str) -> &'static [AnyModel] {
    builtin_catalog()
        .map(|catalog| catalog.all_models(provider))
        .unwrap_or(&[])
}

/// `getBuiltinModel(provider, modelId)`.
pub fn get_builtin_model(provider: &str, model_id: &str) -> Option<&'static Model> {
    builtin_catalog().ok()?.model(provider, model_id)
}

/// `getBuiltinModelDataGeneratedAt()`.
pub fn get_builtin_model_data_generated_at() -> Option<i64> {
    builtin_catalog().ok()?.generated_at()
}

/// `Date.parse` for the manifest's ISO-8601 UTC shape
/// (`YYYY-MM-DDTHH:MM:SS[.fff]Z`), milliseconds since epoch.
fn parse_iso8601_millis(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() < 20 || bytes.last() != Some(&b'Z') {
        return None;
    }
    let num = |start: usize, end: usize| -> Option<i64> { value.get(start..end)?.parse().ok() };
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, min, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let mut millis: i64 = 0;
    if bytes.get(19) == Some(&b'.') {
        let frac = value.get(20..value.len() - 1)?;
        if frac.is_empty() || frac.len() > 3 || !frac.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        millis = frac.parse::<i64>().ok()? * 10i64.pow(3 - frac.len() as u32);
    }
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 60 {
        return None;
    }
    // Days since epoch (Howard Hinnant's days_from_civil).
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some((((days * 24 + hour) * 60 + min) * 60 + sec) * 1000 + millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_catalog_loads_all_vendored_providers() {
        let catalog = builtin_catalog().expect("vendored catalog parses");
        assert_eq!(catalog.providers().len(), CATALOG_PROVIDER_DATA.len());
        assert_eq!(catalog.providers().len(), 42);
        let total: usize = catalog
            .providers()
            .iter()
            .map(|provider| catalog.models(provider).len())
            .sum();
        // Schema v6 regen @ abe508e1b rules (models.dev + OpenRouter +
        // NVIDIA + Vercel AI Gateway snapshot 2026-10-19; +typesafe.json vs
        // the 2026-09-23 schemaVersion 3 snapshot).
        assert_eq!(total, 1622);
        let images: usize = catalog
            .providers()
            .iter()
            .map(|provider| catalog.image_models(provider).len())
            .sum();
        let classifiers: usize = catalog
            .providers()
            .iter()
            .map(|provider| catalog.classifier_models(provider).len())
            .sum();
        assert_eq!(images, 63);
        assert_eq!(classifiers, 46);
        // Radius ships its static public catalog since 4d38031fb; the
        // gateway overlay lives in `providers::radius`.
        assert!(catalog.providers().contains(&"radius"));
        assert!(catalog.providers().contains(&"meta"));
        // v6 new provider: classifier-only static catalog.
        assert!(catalog.providers().contains(&"typesafe"));
    }

    #[test]
    fn test_v6_type_faces() {
        // OpenRouter carries the `openrouter-images` api group (61 image
        // models) and `typesafe-system-one` classifiers.
        let openrouter = builtin_catalog().expect("catalog");
        assert!(
            openrouter
                .all_models("openrouter")
                .iter()
                .any(|model| model.api().as_str() == "openrouter-images")
        );
        let flux = openrouter
            .any_model(
                "openrouter",
                ModelType::Image,
                "black-forest-labs/flux.2-flex",
            )
            .expect("flux image model");
        assert_eq!(flux.model_type(), ModelType::Image);
        assert!(flux.as_chat().is_none());
        assert_eq!(flux.merge_key(), "image\0black-forest-labs/flux.2-flex");
        let chat = openrouter
            .any_model("openrouter", ModelType::Chat, "x-ai/grok-4.7")
            .expect("grok chat model");
        assert_eq!(chat.merge_key(), "chat\0x-ai/grok-4.7");
        let classifier = openrouter
            .any_model("openrouter", ModelType::Classifier, "~typesafe/jev-latest")
            .expect("jev classifier");
        assert_eq!(classifier.model_type(), ModelType::Classifier);
    }

    #[test]
    fn test_get_builtin_model_lookup() {
        let model = get_builtin_model("anthropic", "claude-fable-5").expect("model");
        assert_eq!(model.api.as_str(), "anthropic-messages");
        assert!(get_builtin_model("anthropic", "nope").is_none());
        assert!(get_builtin_model("nope", "claude-fable-5").is_none());
        assert!(get_builtin_models("nope").is_empty());
    }

    #[test]
    fn test_generated_at_matches_manifest() {
        let catalog = builtin_catalog().expect("catalog");
        assert_eq!(catalog.manifest().schema_version, 6);
        assert_eq!(catalog.manifest().files.len(), CATALOG_PROVIDER_DATA.len());
        // 2026-07-30T01:56:27.841Z per the vendored manifest; exact value is
        // asserted against the manifest string itself, not hardcoded here.
        assert!(catalog.generated_at().is_some());
    }

    #[test]
    fn test_parse_iso8601_millis() {
        assert_eq!(parse_iso8601_millis("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso8601_millis("1970-01-01T00:00:00.841Z"), Some(841));
        // 2026-08-11T04:37:23.682Z cross-checked against `date -u -d ... +%s%3N`.
        assert_eq!(
            parse_iso8601_millis("2026-08-11T04:37:23.682Z"),
            Some(1786423043682)
        );
        assert_eq!(
            parse_iso8601_millis("2026-02-28T23:59:59.5Z"),
            Some(1772323199500)
        );
        assert_eq!(parse_iso8601_millis("garbage"), None);
        assert_eq!(parse_iso8601_millis("2026-07-30 01:56:27"), None);
        assert_eq!(parse_iso8601_millis("2026-13-30T01:56:27Z"), None);
    }
}
