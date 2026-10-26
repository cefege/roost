//! Port of `packages/ai/src/providers/kimi-coding.ts` @ pi 0.82.1 (2efa728).
//!
//! Models come from the vendored catalog (`generated.rs`), which carries the
//! same data as upstream `kimi-coding.models.ts` (corrections baked in —
//! e.g. implied subscription pricing on `k3` / `kimi-for-coding-highspeed`).
//!
//! Upstream registers `lazyOAuth({ name: "Kimi Code (subscription)",
//! loginLabel: "Sign in with Kimi Code", load: loadKimiCodingOAuth })`; rpi
//! wires the flow directly (T13 W5, deviation D-029 closed) as
//! [`crate::auth::oauth::kimi_coding`] — `lazyOAuth` is a browser
//! bundle-splitting trick without a rpi counterpart (see `auth/helpers.rs`).

use std::sync::Arc;

use crate::api::anthropic_messages::AnthropicMessages;
use crate::auth::oauth::kimi_coding_oauth;
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::generated::get_builtin_models;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};

/// `kimiCodingProvider()`.
pub fn kimi_coding_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "kimi-coding".to_owned(),
        name: Some("Kimi For Coding".to_owned()),
        base_url: Some("https://api.kimi.com/coding".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "Kimi API key",
                &["KIMI_API_KEY"],
            ))),
            oauth: Some(kimi_coding_oauth()),
        },
        models: get_builtin_models("kimi-coding").to_vec(),
        api: ProviderApi::Single(Arc::new(AnthropicMessages)),
        ..Default::default()
    })
}
