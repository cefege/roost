//! Image-generation provider factories — the images-side counterpart of
//! `crates/rpi-ai/src/providers.rs`. Ports `packages/ai/src/providers/
//! openrouter-images.ts`, `packages/ai/src/providers/images/register-builtins.ts`
//! and the image halves of `packages/ai/src/providers/all.ts`
//! (`builtinImagesProviders` / `builtinImagesModels`).

pub mod openrouter_images;
pub mod register_builtins;

use std::sync::Arc;

use crate::images::ImagesProvider;
use crate::images::images_models::{ImagesModels, create_images_models};
use crate::models::CreateModelsOptions;

/// `builtinImagesProviders()` — every built-in image-generation provider,
/// freshly constructed.
pub fn builtin_images_providers() -> Vec<Arc<dyn ImagesProvider>> {
    vec![openrouter_images::openrouter_images_provider()]
}

/// `builtinImagesModels(options)` — an `ImagesModels` collection with every
/// built-in image-generation provider registered.
pub fn builtin_images_models(options: Option<CreateModelsOptions>) -> ImagesModels {
    let models = create_images_models(options);
    for provider in builtin_images_providers() {
        models.set_provider(provider);
    }
    models
}
