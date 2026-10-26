//! Port of `packages/ai/src/providers/moonshotai-cn.ts` @ pi 0.82.1
//! (2efa728). Independent factory from `moonshotai` (separate region and
//! base URL; same env key upstream), mirroring the upstream file split.
//!
//! Models come from the vendored catalog (`generated.rs`), which carries the
//! same data as upstream `moonshotai-cn.models.ts` (corrections baked in —
//! e.g. `kimi-k3` pricing and `deferredToolsMode: "kimi"`).

use std::sync::Arc;

use crate::api::openai_completions::OpenAiCompletions;
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::generated::get_builtin_models;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};

/// `moonshotaiCnProvider()`.
pub fn moonshotai_cn_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "moonshotai-cn".to_owned(),
        name: Some("Moonshot AI CN".to_owned()),
        base_url: Some("https://api.moonshot.cn/v1".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "Moonshot AI API key",
                &["MOONSHOT_API_KEY"],
            ))),
            oauth: None,
        },
        models: get_builtin_models("moonshotai-cn").to_vec(),
        api: ProviderApi::Single(Arc::new(OpenAiCompletions)),
        ..Default::default()
    })
}
