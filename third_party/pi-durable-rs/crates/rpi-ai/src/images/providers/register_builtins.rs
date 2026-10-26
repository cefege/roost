//! Port of `packages/ai/src/providers/images/register-builtins.ts` @
//! pi 0.82.1 (2efa728).
//!
//! Registers the built-in image-generation api implementations. Upstream runs
//! this at module load via the `images.ts` import side effect and wraps the
//! lazy `import()` of the adapter in a try/catch that turns load failures
//! into an error `AssistantImages` (`createLazyLoadErrorImages`); Rust links
//! statically, so the adapter is always available, the error path is
//! unreachable, and registration is explicit — [`super::super::generate_images`]
//! runs this once lazily before the first dispatch.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::api::openrouter_images::openrouter_images_api;
use crate::images::images_api_registry::{
    ImagesApiProvider, ImagesFunction, register_images_api_provider,
};
use crate::types::{AssistantImages, ImagesApiKind, ImagesContext, ImagesModel, ImagesOptions};

/// `registerBuiltInImagesApiProviders()`.
pub fn register_builtin_images_api_providers() {
    let api = openrouter_images_api();
    register_images_api_provider(
        ImagesApiProvider {
            api: ImagesApiKind::OPENROUTER_IMAGES.to_owned(),
            generate_images: Arc::new(
                move |model: &ImagesModel,
                      context: &ImagesContext,
                      options: Option<&ImagesOptions>|
                      -> Pin<Box<dyn Future<Output = AssistantImages> + Send + 'static>> {
                    let api = Arc::clone(&api);
                    let model = model.clone();
                    let context = context.clone();
                    let options = options.cloned();
                    Box::pin(async move {
                        api.generate_images(&model, &context, options.as_ref()).await
                    })
                },
            ) as ImagesFunction,
        },
        None,
    );
}
