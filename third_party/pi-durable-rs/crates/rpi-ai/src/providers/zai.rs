//! Port of `packages/ai/src/providers/zai.ts` @ pi 0.82.1 (2efa728).
//!
//! Models come from the vendored catalog (`generated.rs`), which carries the
//! same data as upstream `zai.models.ts` (`flattenModelCatalog` re-export of
//! `providers/data/zai.json`, corrections baked in — e.g. `zaiToolStream`).

use std::sync::Arc;

use crate::api::openai_completions::OpenAiCompletions;
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::generated::get_builtin_models;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};

/// `zaiProvider()`.
pub fn zai_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "zai".to_owned(),
        name: Some("Z.AI".to_owned()),
        base_url: Some("https://api.z.ai/api/coding/paas/v4".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth("Z.AI API key", &["ZAI_API_KEY"]))),
            oauth: None,
        },
        models: get_builtin_models("zai").to_vec(),
        api: ProviderApi::Single(Arc::new(OpenAiCompletions)),
        ..Default::default()
    })
}
