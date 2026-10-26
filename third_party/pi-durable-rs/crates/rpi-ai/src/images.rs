//! Image generation subsystem — port of the image halves of
//! `packages/ai/src/` @ pi 0.82.1 (2efa728): `images.ts` (dispatch, this
//! file), `images-api-registry.ts` ([`images_api_registry`]),
//! `images-models.ts` (ImagesModels / ImagesProvider / createImagesModels /
//! createImagesProvider, [`images_models`]), `image-models.ts` (catalog
//! accessors, [`image_models`]), `image-models.generated.ts` (catalog,
//! [`generated`]), `providers/images/register-builtins.ts` +
//! `providers/openrouter-images.ts` + the `builtinImagesProviders` /
//! `builtinImagesModels` halves of `providers/all.ts` ([`providers`]).
//!
//! Landing notes and intentional differences (registry as a `Mutex` map,
//! no import-time side effects, dispatch errors as `Result`, `ApiKind`
//! newtype for `ImagesApi`, …) are registered as deviation D-037.

use std::sync::OnceLock;

use crate::types::{AssistantImages, ImagesContext, ImagesModel, ImagesOptions};

pub mod generated;
pub mod image_models;
pub mod images_api_registry;
pub mod images_models;
pub mod providers;

pub use image_models::{get_image_model, get_image_models, get_image_providers};
pub use images_api_registry::{
    ImagesApiProvider, ImagesFunction, RegisteredImagesApiProvider, get_images_api_provider,
    register_images_api_provider,
};
pub use images_models::{
    CreateImagesProviderOptions, ImagesModels, ImagesProvider, ProviderImages,
    create_images_models, create_images_provider,
};
pub use providers::{builtin_images_models, builtin_images_providers, openrouter_images};

/// The `images.ts` import side effect (`import "./providers/images/
/// register-builtins.ts"`) registers the built-in api providers; Rust has no
/// import-time side effects, so registration runs once before the first
/// dispatch. A provider registered by the caller beforehand is left in place
/// (upstream: the builtin registers at import, user registration after that
/// replaces it — net behavior is the same).
fn ensure_builtin_registered() {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        if get_images_api_provider(crate::types::ImagesApiKind::OPENROUTER_IMAGES).is_none() {
            providers::register_builtins::register_builtin_images_api_providers();
        }
    });
}

/// `generateImages` (images.ts) — resolve the api provider for `model.api`
/// and dispatch. Upstream throws `No API provider registered for api: ...`
/// on a registry miss; the Rust port returns that as `Err` (the
/// never-reject contract lives at the [`ImagesModels`] and adapter levels).
pub async fn generate_images(
    model: &ImagesModel,
    context: &ImagesContext,
    options: Option<&ImagesOptions>,
) -> Result<AssistantImages, String> {
    ensure_builtin_registered();
    let provider = get_images_api_provider(model.api.as_str())
        .ok_or_else(|| format!("No API provider registered for api: {}", model.api))?;
    Ok((provider.generate_images)(model, context, options).await)
}
