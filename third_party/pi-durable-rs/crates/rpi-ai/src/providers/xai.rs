//! Port of `packages/ai/src/providers/xai.ts` @ pi `9841914` (v0.85.0+).
//!
//! Single-API provider: `openai-responses` only (`70e878d4c` / #8124,
//! "route xAI models through Responses"). Upstream signature is
//! `Provider<"openai-responses">` with `api: openAIResponsesApi()` — a
//! `Single` api ignores `model.api`, so catalog entries still recorded as
//! `openai-completions` (grok-4.3 until the V14-09 catalog regen) stream
//! through the Responses adapter, exactly like upstream. Models come from
//! the vendored catalog (upstream `xai.models.ts` is a generated
//! `flattenModelCatalog` re-export of `providers/data/xai.json`).
//!
//! Upstream also wires `oauth: lazyOAuth({ name: "xAI (Grok/X subscription)",
//! loginLabel: "Sign in with SuperGrok or X Premium", load: loadXaiOAuth })`;
//! rpi wires the flow directly (T13 W5, deviation D-031 closed) as
//! [`crate::auth::oauth::xai`].

use std::sync::Arc;

use crate::api::openai_responses::OpenAiResponses;
use crate::auth::oauth::xai_oauth;
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::generated::get_builtin_models;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};

/// `xaiProvider()`.
pub fn xai_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "xai".to_owned(),
        name: Some("xAI".to_owned()),
        base_url: Some("https://api.x.ai/v1".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth("xAI API key", &["XAI_API_KEY"]))),
            oauth: Some(xai_oauth()),
        },
        models: get_builtin_models("xai").to_vec(),
        api: xai_provider_api(),
        ..Default::default()
    })
}

/// `api: openAIResponsesApi()` — single Responses implementation, dispatched
/// for every model regardless of the catalog `api` field (models.ts:792
/// `single ?? byApi?.[model.api]`).
fn xai_provider_api() -> ProviderApi {
    ProviderApi::Single(Arc::new(OpenAiResponses))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `70e878d4c` (#8124): the provider is narrowed to a single
    /// `openai-responses` api — no `openai-completions` arm remains.
    #[test]
    fn xai_provider_api_is_single_openai_responses() {
        let ProviderApi::Single(_) = xai_provider_api() else {
            panic!("expected single-api provider");
        };
        // No runtime reference to an openai-completions xai adapter: the
        // factory no longer constructs `OpenAiCompletions` at all (compile
        // error if reintroduced without updating this test).
    }
}
