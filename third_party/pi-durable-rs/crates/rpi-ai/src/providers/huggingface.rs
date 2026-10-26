//! Port of `packages/ai/src/providers/huggingface.ts` @ pi 0.82.1 (2efa728) —
//! T13 W4.
//!
//! Models come from the vendored catalog (upstream `huggingface.models.ts` is
//! a generated `flattenModelCatalog` re-export of
//! `providers/data/huggingface.json`, corrections baked in).

use std::sync::Arc;

use crate::api::openai_completions::OpenAiCompletions;
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::generated::get_builtin_models;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};

/// `huggingfaceProvider()`.
pub fn huggingface_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "huggingface".to_owned(),
        name: Some("Hugging Face".to_owned()),
        base_url: Some("https://router.huggingface.co/v1".to_owned()),
        headers: None,
        auth: ProviderAuth {
            // Upstream labels the env-var auth "Hugging Face token".
            api_key: Some(Arc::new(env_api_key_auth(
                "Hugging Face token",
                &["HF_TOKEN"],
            ))),
            oauth: None,
        },
        models: get_builtin_models("huggingface").to_vec(),
        api: ProviderApi::Single(Arc::new(OpenAiCompletions)),
        ..Default::default()
    })
}
