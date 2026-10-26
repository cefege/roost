//! Port of `packages/ai/src/providers/fireworks.ts` @ pi 0.82.1 (2efa728) —
//! T13 W4.
//!
//! Mixed-API provider: the catalog carries both `anthropic-messages` and
//! `openai-completions` models, dispatched on `model.api` (upstream api map).
//! Models come from the vendored catalog (upstream `fireworks.models.ts` is a
//! generated `flattenModelCatalog` re-export of `providers/data/fireworks.json`,
//! corrections baked in — including the Fireworks anthropic compat flags
//! `sendSessionAffinityHeaders` / `supportsEagerToolInputStreaming: false` /
//! `supportsCacheControlOnTools: false` asserted by
//! `fireworks-models.test.ts`).

use std::collections::HashMap;
use std::sync::Arc;

use crate::api::anthropic_messages::AnthropicMessages;
use crate::api::openai_completions::OpenAiCompletions;
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::generated::get_builtin_models;
use crate::models::{
    CreateProviderOptions, Provider, ProviderApi, ProviderStreams, create_provider,
};
use crate::types::ApiKind;

/// `fireworksProvider()`.
pub fn fireworks_provider() -> Arc<dyn Provider> {
    let api: HashMap<String, Arc<dyn ProviderStreams>> = HashMap::from([
        (
            ApiKind::ANTHROPIC_MESSAGES.to_owned(),
            Arc::new(AnthropicMessages) as Arc<dyn ProviderStreams>,
        ),
        (
            ApiKind::OPENAI_COMPLETIONS.to_owned(),
            Arc::new(OpenAiCompletions) as Arc<dyn ProviderStreams>,
        ),
    ]);
    create_provider(CreateProviderOptions {
        id: "fireworks".to_owned(),
        name: Some("Fireworks".to_owned()),
        base_url: Some("https://api.fireworks.ai/inference".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "Fireworks API key",
                &["FIREWORKS_API_KEY"],
            ))),
            oauth: None,
        },
        models: get_builtin_models("fireworks").to_vec(),
        api: ProviderApi::Map(api),
        ..Default::default()
    })
}
