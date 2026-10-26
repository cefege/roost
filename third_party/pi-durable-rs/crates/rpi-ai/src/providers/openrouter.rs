//! Port of `packages/ai/src/providers/openrouter.ts` @ pi `9841914`
//! (v0.85.0+).
//!
//! Mixed-API provider (`650e7a612` / #8454 + `4e69b0c28` / #8614):
//! `anthropic-messages` for `anthropic/*` models plus `openai-completions`
//! for everything else, dispatched on `model.api` — mirrors upstream
//! `Provider<"anthropic-messages" | "openai-completions">` with the two-entry
//! api map. The anthropic catalog entries arrive with the V14-09 vendored
//! catalog regen; until then every shipped model stays on
//! `openai-completions` and dispatch is unchanged.
//!
//! T13 W5: OAuth (`loadOpenRouterOAuth`: PKCE exchange for a permanent key,
//! no-op refresh) landed in [`crate::auth::oauth::openrouter`] and replaces
//! the W4 `PendingOAuth` placeholder (deviation D-032). The upstream
//! `loginLabel` ("Sign in with OpenRouter") has no `OAuthAuth` slot and stays
//! unported.

use std::collections::HashMap;
use std::sync::Arc;

use crate::api::anthropic_messages::AnthropicMessages;
use crate::api::openai_completions::OpenAiCompletions;
use crate::api::openrouter_images::openrouter_images_v6_api;
use crate::api::typesafe_system_one::{TYPESAFE_SYSTEM_ONE_API, typesafe_system_one_api};
use crate::auth::oauth::openrouter_oauth;
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::generated::{get_builtin_all_models, get_builtin_models};
use crate::models::{
    CreateProviderOptions, Provider, ProviderApi, ProviderStreams, create_provider,
};
use crate::types::ApiKind;

/// `openrouterProvider()`.
pub fn openrouter_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "openrouter".to_owned(),
        name: Some("OpenRouter".to_owned()),
        base_url: Some("https://openrouter.ai/api/v1".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "OpenRouter API key",
                &["OPENROUTER_API_KEY"],
            ))),
            oauth: Some(openrouter_oauth()),
        },
        models: get_builtin_models("openrouter").to_vec(),
        all_models: get_builtin_all_models("openrouter").to_vec(),
        classifiers: HashMap::from([(
            TYPESAFE_SYSTEM_ONE_API.to_owned(),
            typesafe_system_one_api(),
        )]),
        images: HashMap::from([("openrouter-images".to_owned(), openrouter_images_v6_api())]),
        api: ProviderApi::Map(api_map()),
        ..Default::default()
    })
}

/// Mixed-API dispatch table (openrouter.ts:23-26): `anthropic-messages` for
/// OpenRouter Claude (reasoning effort / mid-conversation effort replay flow
/// through the Anthropic adapter, V14-05) + `openai-completions` default.
fn api_map() -> HashMap<String, Arc<dyn ProviderStreams>> {
    HashMap::from([
        (
            ApiKind::ANTHROPIC_MESSAGES.to_owned(),
            Arc::new(AnthropicMessages) as Arc<dyn ProviderStreams>,
        ),
        (
            ApiKind::OPENAI_COMPLETIONS.to_owned(),
            Arc::new(OpenAiCompletions) as Arc<dyn ProviderStreams>,
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `650e7a612` / #8454: the provider dispatches on both apis —
    /// `anthropic-messages` entries (V14-09 catalog) never fall through to
    /// the completions adapter, and vice versa.
    #[test]
    fn api_map_covers_anthropic_and_completions() {
        let map = api_map();
        for api in [ApiKind::ANTHROPIC_MESSAGES, ApiKind::OPENAI_COMPLETIONS] {
            assert!(map.contains_key(api), "missing dispatch entry {api}");
        }
        assert_eq!(map.len(), 2, "exactly the two upstream entries");
    }
}
