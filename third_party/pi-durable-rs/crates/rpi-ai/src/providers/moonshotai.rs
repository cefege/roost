//! Port of `packages/ai/src/providers/moonshotai.ts` @ pi 0.82.1 (2efa728).
//!
//! Models come from the vendored catalog (`generated.rs`), which carries the
//! same data as upstream `moonshotai.models.ts` (corrections baked in — e.g.
//! `kimi-k3` pricing and `deferredToolsMode: "kimi"`).

use std::sync::Arc;

use crate::api::openai_completions::OpenAiCompletions;
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::generated::get_builtin_models;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};

/// `moonshotaiProvider()`.
pub fn moonshotai_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "moonshotai".to_owned(),
        name: Some("Moonshot AI".to_owned()),
        base_url: Some("https://api.moonshot.ai/v1".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "Moonshot AI API key",
                &["MOONSHOT_API_KEY"],
            ))),
            oauth: None,
        },
        models: get_builtin_models("moonshotai").to_vec(),
        api: ProviderApi::Single(Arc::new(OpenAiCompletions)),
        ..Default::default()
    })
}
