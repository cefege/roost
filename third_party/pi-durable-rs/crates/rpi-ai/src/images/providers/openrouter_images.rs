//! Port of `packages/ai/src/providers/openrouter-images.ts` @ pi 0.82.1
//! (2efa728).
//!
//! The `openrouter` image-generation provider factory. Upstream's
//! `openrouterImagesApi()` (from `api/openrouter-images.lazy.ts`) loads the
//! adapter module lazily; Rust links statically, so it hands out the static
//! adapter directly (same pattern as every other adapter's `.lazy.ts`).

use std::sync::Arc;

use crate::api::openrouter_images::openrouter_images_api;
use crate::auth::oauth::openrouter_oauth;
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::images::image_models::get_image_models;
use crate::images::images_models::{
    CreateImagesProviderOptions, ImagesProvider, create_images_provider,
};

/// `openrouterImagesProvider()`.
pub fn openrouter_images_provider() -> Arc<dyn ImagesProvider> {
    create_images_provider(CreateImagesProviderOptions {
        id: "openrouter".to_owned(),
        name: Some("OpenRouter".to_owned()),
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "OpenRouter API key",
                &["OPENROUTER_API_KEY"],
            ))),
            oauth: Some(openrouter_oauth()),
        },
        models: get_image_models("openrouter"),
        refresh_models: None,
        api: openrouter_images_api(),
    })
}
