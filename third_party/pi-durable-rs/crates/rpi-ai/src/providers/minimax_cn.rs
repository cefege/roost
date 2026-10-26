//! Port of `packages/ai/src/providers/minimax-cn.ts` @ pi 0.82.1 (2efa728).
//! Independent factory from `minimax` (separate region, base URL, env key),
//! mirroring the upstream file split.
//!
//! Models come from the vendored catalog (`generated.rs`), which carries the
//! same data as upstream `minimax-cn.models.ts`.

use std::sync::Arc;

use crate::api::anthropic_messages::AnthropicMessages;
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::generated::get_builtin_models;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};

/// `minimaxCnProvider()`.
pub fn minimax_cn_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "minimax-cn".to_owned(),
        name: Some("MiniMax CN".to_owned()),
        base_url: Some("https://api.minimaxi.com/anthropic".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "MiniMax CN API key",
                &["MINIMAX_CN_API_KEY"],
            ))),
            oauth: None,
        },
        models: get_builtin_models("minimax-cn").to_vec(),
        api: ProviderApi::Single(Arc::new(AnthropicMessages)),
        ..Default::default()
    })
}
