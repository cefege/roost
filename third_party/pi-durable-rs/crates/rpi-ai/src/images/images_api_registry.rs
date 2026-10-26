//! Port of `packages/ai/src/images-api-registry.ts` @ pi 0.82.1 (2efa728).
//!
//! Registry of image-generation API implementations keyed by `model.api`.
//! Upstream registers the openrouter-images implementation at module load via
//! the `providers/images/register-builtins.ts` import side effect; Rust has
//! no import-time side effects, so registration is explicit (see
//! [`super::providers::register_builtins`] and the lazy ensure hook in
//! [`super::generate_images`]).
//!
//! Intentional differences:
//! - The generic `<TApi, TOptions>` typing collapses; `ImagesFunction` takes
//!   the untyped `ImagesModel`/`ImagesOptions`.
//! - Upstream dispatch throws on a mismatched api; the Rust wrap encodes the
//!   same invariant as an error `AssistantImages` (`stopReason: "error"`),
//!   matching the `createLazyLoadErrorImages` pattern of the builtin
//!   registration (never-throw at the api-function level).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};

use crate::types::{AssistantImages, ImagesContext, ImagesModel, ImagesOptions};

/// The registry function shape: `(model, context, options) => Promise<AssistantImages>`.
pub type ImagesFunction = Arc<
    dyn Fn(
            &ImagesModel,
            &ImagesContext,
            Option<&ImagesOptions>,
        ) -> Pin<Box<dyn Future<Output = AssistantImages> + Send + 'static>>
        + Send
        + Sync,
>;

/// `ImagesApiProvider` — one registered api implementation.
pub struct ImagesApiProvider {
    pub api: String,
    pub generate_images: ImagesFunction,
}

/// `RegisteredImagesApiProvider` (the internal registry value): the wrapped
/// function plus the optional source id.
pub struct RegisteredImagesApiProvider {
    pub generate_images: ImagesFunction,
    pub source_id: Option<String>,
}

fn registry() -> &'static Mutex<HashMap<String, RegisteredImagesApiProvider>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, RegisteredImagesApiProvider>>> =
        OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `wrapGenerateImages`: runtime invariant guard that the model's api matches
/// the registered implementation's api. Unreachable through the dispatch path
/// (lookup is keyed by `model.api`), kept for fidelity; the error surfaces as
/// an error result, not a throw.
fn wrap_generate_images(api: &str, generate_images: ImagesFunction) -> ImagesFunction {
    let api = api.to_owned();
    Arc::new(move |model, context, options| {
        if model.api.as_str() != api {
            let message = format!("Mismatched api: {} expected {}", model.api, api);
            let api_kind = model.api.clone();
            let provider = model.provider.clone();
            let id = model.id.clone();
            return Box::pin(async move {
                AssistantImages {
                    api: api_kind,
                    provider,
                    model: id,
                    output: Vec::new(),
                    response_id: None,
                    usage: None,
                    stop_reason: crate::types::ImagesStopReason::Error,
                    error_message: Some(message),
                    timestamp: super::images_models::now_ms(),
                }
            });
        }
        generate_images(model, context, options)
    })
}

/// `registerImagesApiProvider` — upsert by `provider.api`.
pub fn register_images_api_provider(provider: ImagesApiProvider, source_id: Option<&str>) {
    registry().lock().unwrap_or_else(|e| e.into_inner()).insert(
        provider.api.clone(),
        RegisteredImagesApiProvider {
            generate_images: wrap_generate_images(&provider.api, provider.generate_images),
            source_id: source_id.map(str::to_owned),
        },
    );
}

/// `getImagesApiProvider(api)` — the registered implementation, if any.
pub fn get_images_api_provider(api: &str) -> Option<RegisteredImagesApiProvider> {
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(api)
        .map(|entry| RegisteredImagesApiProvider {
            generate_images: entry.generate_images.clone(),
            source_id: entry.source_id.clone(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::images::images_models::now_ms;
    use crate::types::{
        ImagesApiKind, ImagesContext, ImagesInputContent, ImagesOutputModality, ImagesStopReason,
        InputModality, ModelCost, ModelCostRates, TextContent,
    };

    fn model(api: &str) -> ImagesModel {
        ImagesModel {
            id: "m".to_owned(),
            name: "m".to_owned(),
            api: ImagesApiKind::from(api),
            provider: "p".to_owned(),
            base_url: "https://example.test/v1".to_owned(),
            input: vec![InputModality::Text],
            output: vec![ImagesOutputModality::Image],
            cost: ModelCost {
                rates: ModelCostRates {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                },
                tiers: None,
            },
            headers: None,
        }
    }

    fn context() -> ImagesContext {
        ImagesContext {
            input: vec![ImagesInputContent::Text(TextContent {
                text: "a red circle".to_owned(),
                text_signature: None,
            })],
        }
    }

    fn ok_fn() -> ImagesFunction {
        Arc::new(
            |_model: &ImagesModel,
             _context: &ImagesContext,
             _options: Option<&ImagesOptions>|
             -> Pin<Box<dyn Future<Output = AssistantImages> + Send + 'static>> {
                Box::pin(async move {
                    AssistantImages {
                        api: ImagesApiKind::from("test"),
                        provider: "p".to_owned(),
                        model: "m".to_owned(),
                        output: Vec::new(),
                        response_id: None,
                        usage: None,
                        stop_reason: ImagesStopReason::Stop,
                        error_message: None,
                        timestamp: now_ms(),
                    }
                })
            },
        )
    }

    #[test]
    fn register_and_get_roundtrip() {
        register_images_api_provider(
            ImagesApiProvider {
                api: "test-api".to_owned(),
                generate_images: ok_fn(),
            },
            Some("source"),
        );
        let entry = get_images_api_provider("test-api").expect("registered");
        assert_eq!(entry.source_id.as_deref(), Some("source"));
        assert!(get_images_api_provider("missing").is_none());
    }

    #[test]
    fn wrap_mismatch_returns_error_result() {
        // `wrapGenerateImages` invariant: a model whose api differs from the
        // registered api gets an error result (upstream throws).
        register_images_api_provider(
            ImagesApiProvider {
                api: "expected-api".to_owned(),
                generate_images: ok_fn(),
            },
            None,
        );
        let entry = get_images_api_provider("expected-api").expect("registered");
        let model = model("other-api");
        let result = (entry.generate_images)(&model, &context(), None);
        let output = futures::executor::block_on(result);
        assert_eq!(output.stop_reason, ImagesStopReason::Error);
        assert_eq!(
            output.error_message.as_deref(),
            Some("Mismatched api: other-api expected expected-api")
        );
    }
}
