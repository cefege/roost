//! Port of `packages/ai/src/providers/deepseek.ts` @ pi 0.82.1 (2efa728) —
//! T13 W4.
//!
//! Models come from the vendored catalog (upstream `deepseek.models.ts` is a
//! generated `flattenModelCatalog` re-export of `providers/data/deepseek.json`,
//! corrections baked in).

use std::sync::Arc;

use crate::api::openai_completions::OpenAiCompletions;
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::generated::get_builtin_models;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};

/// `deepseekProvider()`.
pub fn deepseek_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "deepseek".to_owned(),
        name: Some("DeepSeek".to_owned()),
        base_url: Some("https://api.deepseek.com".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "DeepSeek API key",
                &["DEEPSEEK_API_KEY"],
            ))),
            oauth: None,
        },
        models: get_builtin_models("deepseek").to_vec(),
        api: ProviderApi::Single(Arc::new(OpenAiCompletions)),
        ..Default::default()
    })
}
