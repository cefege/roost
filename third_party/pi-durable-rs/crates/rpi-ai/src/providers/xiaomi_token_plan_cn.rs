//! Port of `packages/ai/src/providers/xiaomi-token-plan-cn.ts` @ pi 0.82.1
//! (2efa728) — T13 W4.
//!
//! Xiaomi MiMo token-plan China endpoint. Models come from the vendored
//! catalog (upstream `xiaomi-token-plan-cn.models.ts` is a generated
//! `flattenModelCatalog` re-export of
//! `providers/data/xiaomi-token-plan-cn.json`, corrections baked in).

use std::sync::Arc;

use crate::api::openai_completions::OpenAiCompletions;
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::generated::get_builtin_models;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};

/// `xiaomiTokenPlanCnProvider()`.
pub fn xiaomi_token_plan_cn_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "xiaomi-token-plan-cn".to_owned(),
        name: Some("Xiaomi Token Plan CN".to_owned()),
        base_url: Some("https://token-plan-cn.xiaomimimo.com/v1".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "Xiaomi Token Plan CN API key",
                &["XIAOMI_TOKEN_PLAN_CN_API_KEY"],
            ))),
            oauth: None,
        },
        models: get_builtin_models("xiaomi-token-plan-cn").to_vec(),
        api: ProviderApi::Single(Arc::new(OpenAiCompletions)),
        ..Default::default()
    })
}
