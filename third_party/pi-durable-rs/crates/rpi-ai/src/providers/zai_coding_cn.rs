//! Port of `packages/ai/src/providers/zai-coding-cn.ts` @ pi 0.82.1
//! (2efa728). Independent factory from `zai` (separate region, base URL, env
//! key), mirroring the upstream file split.
//!
//! Models come from the vendored catalog (`generated.rs`), which carries the
//! same data as upstream `zai-coding-cn.models.ts` (corrections baked in —
//! e.g. `zaiToolStream`).

use std::sync::Arc;

use crate::api::openai_completions::OpenAiCompletions;
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::generated::get_builtin_models;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};

/// `zaiCodingCnProvider()`.
pub fn zai_coding_cn_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "zai-coding-cn".to_owned(),
        name: Some("Z.AI Coding CN".to_owned()),
        base_url: Some("https://open.bigmodel.cn/api/coding/paas/v4".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "Z.AI Coding CN API key",
                &["ZAI_CODING_CN_API_KEY"],
            ))),
            oauth: None,
        },
        models: get_builtin_models("zai-coding-cn").to_vec(),
        api: ProviderApi::Single(Arc::new(OpenAiCompletions)),
        ..Default::default()
    })
}
