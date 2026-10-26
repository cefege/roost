//! Port of `packages/ai/src/providers/qwen-token-plan.ts` @ pi 0.82.1
//! (2efa728).
//!
//! Models come from the vendored catalog (`generated.rs`), which carries the
//! same data as upstream `qwen-token-plan.models.ts`: text models only, the
//! image-generation models (`qwen-image-*`, `wan2.7-image*`) are omitted
//! upstream in `generate-models.ts` (see `qwen-token-plan-models.test.ts`).

use std::sync::Arc;

use crate::api::openai_completions::OpenAiCompletions;
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::generated::get_builtin_models;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};

/// `qwenTokenPlanProvider()`.
pub fn qwen_token_plan_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "qwen-token-plan".to_owned(),
        name: Some("Qwen Token Plan".to_owned()),
        base_url: Some(
            "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1".to_owned(),
        ),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "Qwen Token Plan API key",
                &["QWEN_TOKEN_PLAN_API_KEY"],
            ))),
            oauth: None,
        },
        models: get_builtin_models("qwen-token-plan").to_vec(),
        api: ProviderApi::Single(Arc::new(OpenAiCompletions)),
        ..Default::default()
    })
}
