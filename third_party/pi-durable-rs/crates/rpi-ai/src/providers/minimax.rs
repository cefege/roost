//! Port of `packages/ai/src/providers/minimax.ts` @ pi 0.82.1 (2efa728).
//!
//! Models come from the vendored catalog (`generated.rs`), which carries the
//! same data as upstream `minimax.models.ts`.

use std::sync::Arc;

use crate::api::anthropic_messages::AnthropicMessages;
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::generated::get_builtin_models;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};

/// `minimaxProvider()`.
pub fn minimax_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "minimax".to_owned(),
        name: Some("MiniMax".to_owned()),
        base_url: Some("https://api.minimax.io/anthropic".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "MiniMax API key",
                &["MINIMAX_API_KEY"],
            ))),
            oauth: None,
        },
        models: get_builtin_models("minimax").to_vec(),
        api: ProviderApi::Single(Arc::new(AnthropicMessages)),
        ..Default::default()
    })
}
