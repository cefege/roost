//! Port of `packages/ai/src/providers/qwen-token-plan-cn.ts` @ pi 0.82.1
//! (2efa728). Independent factory from `qwen-token-plan` (separate region,
//! base URL, env key), mirroring the upstream file split.
//!
//! Models come from the vendored catalog (`generated.rs`), which carries the
//! same data as upstream `qwen-token-plan-cn.models.ts`: text models only,
//! the image-generation models are omitted upstream (see
//! `qwen-token-plan-models.test.ts`).

use std::sync::Arc;

use crate::api::openai_completions::OpenAiCompletions;
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::generated::get_builtin_models;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};

/// `qwenTokenPlanCnProvider()`.
pub fn qwen_token_plan_cn_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "qwen-token-plan-cn".to_owned(),
        name: Some("Qwen Token Plan CN".to_owned()),
        base_url: Some(
            "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1".to_owned(),
        ),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "Qwen Token Plan CN API key",
                &["QWEN_TOKEN_PLAN_CN_API_KEY"],
            ))),
            oauth: None,
        },
        models: get_builtin_models("qwen-token-plan-cn").to_vec(),
        api: ProviderApi::Single(Arc::new(OpenAiCompletions)),
        ..Default::default()
    })
}
