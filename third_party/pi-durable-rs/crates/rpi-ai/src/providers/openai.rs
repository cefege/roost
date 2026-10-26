//! Port of `packages/ai/src/providers/openai.ts` @ pi 0.82.1 (2efa728).

use std::sync::Arc;

use crate::api::openai_decisions::{OPENAI_DECISIONS_API, openai_decisions_api};
use crate::api::openai_responses::OpenAiResponses;
use crate::auth::oauth::openai_chatgpt_oauth;
use crate::auth::{Credential, ProviderAuth, env_api_key_auth};
use crate::generated::{get_builtin_all_models, get_builtin_models};
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::types::AnyModel;

/// `openaiProvider()`.
pub fn openai_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "openai".to_owned(),
        name: Some("OpenAI".to_owned()),
        base_url: Some("https://api.openai.com/v1".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "OpenAI API key",
                &["OPENAI_API_KEY"],
            ))),
            // `lazyOAuth({ name: "OpenAI (ChatGPT subscription)", ... })`
            // (`02eed88fd`); loginLabel has no `OAuthAuth` slot in rpi.
            oauth: Some(openai_chatgpt_oauth()),
        },
        models: get_builtin_models("openai").to_vec(),
        // Schema-v6 all-type baseline: chat + the `openai-decisions`
        // classifier (`gpt-6-luna`, R2.4.1).
        all_models: get_builtin_all_models("openai").to_vec(),
        classifiers: std::collections::HashMap::from([(
            OPENAI_DECISIONS_API.to_owned(),
            openai_decisions_api(),
        )]),
        // Sign in with ChatGPT tokens only reach the Responses API; the
        // Decisions API rejects them (openai.ts:26-28 @ ce8972a0e).
        filter_all_models_fn: Some(Arc::new(|models: Vec<AnyModel>, credential| {
            if matches!(credential, Some(Credential::OAuth(_))) {
                models
                    .into_iter()
                    .filter(|model| !matches!(model, AnyModel::Classifier(_)))
                    .collect()
            } else {
                models
            }
        })),
        api: ProviderApi::Single(Arc::new(OpenAiResponses)),
        ..Default::default()
    })
}
