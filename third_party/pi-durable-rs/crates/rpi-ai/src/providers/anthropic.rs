//! Port of `packages/ai/src/providers/anthropic.ts` @ pi 0.82.1 (2efa728) —
//! the `anthropicProvider()` wiring. The non-standard api-key auth half was
//! ported earlier as [`crate::auth::anthropic_api_key_auth`]; the OAuth half
//! is [`crate::auth::anthropic_oauth`] (upstream wraps it in `lazyOAuth`,
//! which rpi deliberately does not port — see `auth/helpers.rs`).

use std::sync::Arc;

use crate::api::anthropic_messages::AnthropicMessages;
use crate::auth::{ProviderAuth, anthropic_api_key_auth, anthropic_oauth};
use crate::generated::get_builtin_models;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};

/// `anthropicProvider()`.
pub fn anthropic_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "anthropic".to_owned(),
        name: Some("Anthropic".to_owned()),
        base_url: Some("https://api.anthropic.com".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(anthropic_api_key_auth())),
            oauth: Some(anthropic_oauth()),
        },
        models: get_builtin_models("anthropic").to_vec(),
        api: ProviderApi::Single(Arc::new(AnthropicMessages)),
        ..Default::default()
    })
}
