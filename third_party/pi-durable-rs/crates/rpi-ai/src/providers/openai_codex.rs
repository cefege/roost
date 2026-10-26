//! Port of `packages/ai/src/providers/openai-codex.ts` @ pi 0.82.1 (2efa728).
//!
//! Upstream auth is OAuth-only (`lazyOAuth({ name: "OpenAI (ChatGPT
//! Plus/Pro)", load: loadOpenAICodexOAuth })`) — no api-key channel. The W4
//! empty-`ProviderAuth` placeholder (deviation D-030) was closed in T13 W5:
//! the real flow lives in [`crate::auth::oauth::openai_codex`] and is wired
//! into the factory's `oauth` slot here.

use std::sync::Arc;

use crate::api::openai_codex_responses::OpenAiCodexResponses;
use crate::auth::ProviderAuth;
use crate::auth::oauth::openai_codex_oauth;
use crate::generated::get_builtin_models;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};

/// `openaiCodexProvider()`.
pub fn openai_codex_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "openai-codex".to_owned(),
        name: Some("OpenAI Codex".to_owned()),
        base_url: Some("https://chatgpt.com/backend-api".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: None,
            oauth: Some(openai_codex_oauth()),
        },
        models: get_builtin_models("openai-codex").to_vec(),
        api: ProviderApi::Single(Arc::new(OpenAiCodexResponses)),
        ..Default::default()
    })
}
