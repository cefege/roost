//! Port of `packages/ai/src/image-models.ts` @ pi 0.82.1 (2efa728).
//!
//! Runtime image-model registry built from the generated catalog
//! ([`super::generated::IMAGE_MODELS`]). Upstream constructs the
//! `Map<string, Map<string, ImagesModel>>` at module load; Rust builds the
//! same map once on first access (`OnceLock`).

use std::collections::HashMap;
use std::sync::OnceLock;

use super::generated::image_models;
use crate::types::ImagesModel;

fn image_model_registry() -> &'static HashMap<&'static str, HashMap<&'static str, ImagesModel>> {
    static REGISTRY: OnceLock<HashMap<&'static str, HashMap<&'static str, ImagesModel>>> =
        OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut registry = HashMap::new();
        for (provider, models) in image_models() {
            let provider_models: HashMap<&'static str, ImagesModel> = models
                .iter()
                .map(|model| (model.id.as_str(), model.clone()))
                .collect();
            registry.insert(*provider, provider_models);
        }
        registry
    })
}

/// `getImageModel(provider, modelId)` — typed catalog lookup.
pub fn get_image_model(provider: &str, model_id: &str) -> Option<ImagesModel> {
    image_model_registry()
        .get(provider)
        .and_then(|models| models.get(model_id))
        .cloned()
}

/// `getImageProviders()` — known image-generation providers (catalog keys).
pub fn get_image_providers() -> Vec<&'static str> {
    image_model_registry().keys().copied().collect()
}

/// `getImageModels(provider)` — catalog models for one provider.
pub fn get_image_models(provider: &str) -> Vec<ImagesModel> {
    image_model_registry()
        .get(provider)
        .map(|models| models.values().cloned().collect())
        .unwrap_or_default()
}
