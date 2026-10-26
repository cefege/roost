//! Port of `packages/ai/src/providers/baseten.ts` @ pi `c1019d920` / `4181f66`.
//!
//! Baseten is an OpenAI-compatible provider serving hosted open models
//! (DeepSeek, Kimi, GLM, Nemotron, etc.) at `https://inference.baseten.co/v1`.
//!
//! The `thinkingFormat: "baseten"` toggle logic, `chatTemplateArgs` compat
//! field, and `status === "deprecated"` model filtering are baked into the
//! vendored catalog by the upstream generator's `processBasetenModels()`
//! (`generate-models.ts:1131-1223`); the factory just passes catalog models
//! through. The baseten thinking branch in the completions adapter
//! (`openai_completions.rs::apply_thinking_params`) consumes those fields.

use std::sync::Arc;

use crate::api::openai_completions::OpenAiCompletions;
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::generated::get_builtin_models;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};

/// `basetenProvider()`.
pub fn baseten_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "baseten".to_owned(),
        name: Some("Baseten".to_owned()),
        base_url: Some("https://inference.baseten.co/v1".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "Baseten API key",
                &["BASETEN_API_KEY"],
            ))),
            oauth: None,
        },
        models: get_builtin_models("baseten").to_vec(),
        api: ProviderApi::Single(Arc::new(OpenAiCompletions)),
        ..Default::default()
    })
}
