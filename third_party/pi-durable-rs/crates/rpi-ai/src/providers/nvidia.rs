//! Port of `packages/ai/src/providers/nvidia.ts` @ pi 0.82.1 (2efa728) — T13
//! W4.
//!
//! Models come from the vendored catalog (upstream `nvidia.models.ts` is a
//! generated `flattenModelCatalog` re-export of `providers/data/nvidia.json`,
//! corrections baked in).

use std::sync::Arc;

use crate::api::openai_completions::OpenAiCompletions;
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::generated::get_builtin_models;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};

/// `nvidiaProvider()`.
pub fn nvidia_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "nvidia".to_owned(),
        name: Some("NVIDIA".to_owned()),
        base_url: Some("https://integrate.api.nvidia.com/v1".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "NVIDIA API key",
                &["NVIDIA_API_KEY"],
            ))),
            oauth: None,
        },
        models: get_builtin_models("nvidia").to_vec(),
        api: ProviderApi::Single(Arc::new(OpenAiCompletions)),
        ..Default::default()
    })
}
