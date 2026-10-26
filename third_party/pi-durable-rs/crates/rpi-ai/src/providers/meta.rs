//! Port of `packages/ai/src/providers/meta.ts` @ pi 0.86.1 (`b73412a37`,
//! #9096) — Meta Model API provider. Models come from the vendored catalog
//! (`generated.rs`, regenerated at `19451accd` in V15-03; upstream
//! `meta.models.ts` is a generated `flattenModelCatalog` re-export of
//! `providers/data/meta.json` — 5 entries, default `muse-spark-1.3`).
//!
//! Upstream registers `lazyOAuth({ name: "Meta (Muse subscription)",
//! isSubscription: true, loginLabel: "Sign in with Meta", load:
//! loadMetaOAuth })`; rpi wires the flow directly (T13 W5 precedent,
//! D-029/D-031 closed) as [`crate::auth::oauth::meta`].

use std::sync::Arc;

use crate::api::openai_responses::OpenAiResponses;
use crate::auth::oauth::meta_oauth;
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::generated::get_builtin_models;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};

/// `metaProvider()`.
pub fn meta_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "meta".to_owned(),
        name: Some("Meta".to_owned()),
        base_url: Some("https://api.meta.ai/v1".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "Meta Model API key",
                &["META_API_KEY"],
            ))),
            oauth: Some(meta_oauth()),
        },
        models: get_builtin_models("meta").to_vec(),
        api: ProviderApi::Single(Arc::new(OpenAiResponses)),
        ..Default::default()
    })
}
