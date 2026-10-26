//! Port of `packages/ai/src/providers/xiaomi.ts` @ pi 0.82.1 (2efa728) — T13 W4.
//!
//! Xiaomi MiMo API-billing endpoint. Models come from the vendored catalog
//! (upstream `xiaomi.models.ts` is a generated `flattenModelCatalog`
//! re-export of `providers/data/xiaomi.json`, corrections baked in).

use std::sync::Arc;

use crate::api::openai_completions::OpenAiCompletions;
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::generated::get_builtin_models;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};

/// `xiaomiProvider()`.
pub fn xiaomi_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "xiaomi".to_owned(),
        name: Some("Xiaomi".to_owned()),
        base_url: Some("https://api.xiaomimimo.com/v1".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "Xiaomi API key",
                &["XIAOMI_API_KEY"],
            ))),
            oauth: None,
        },
        models: get_builtin_models("xiaomi").to_vec(),
        api: ProviderApi::Single(Arc::new(OpenAiCompletions)),
        ..Default::default()
    })
}
