//! Port of `packages/ai/src/providers/groq.ts` @ pi 0.82.1 (2efa728) — T13 W4.
//!
//! Models come from the vendored catalog (upstream `groq.models.ts` is a
//! generated `flattenModelCatalog` re-export of `providers/data/groq.json`,
//! corrections baked in).

use std::sync::Arc;

use crate::api::openai_completions::OpenAiCompletions;
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::generated::get_builtin_models;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};

/// `groqProvider()`.
pub fn groq_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "groq".to_owned(),
        name: Some("Groq".to_owned()),
        base_url: Some("https://api.groq.com/openai/v1".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "Groq API key",
                &["GROQ_API_KEY"],
            ))),
            oauth: None,
        },
        models: get_builtin_models("groq").to_vec(),
        api: ProviderApi::Single(Arc::new(OpenAiCompletions)),
        ..Default::default()
    })
}
