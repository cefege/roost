//! Port of `packages/ai/src/providers/xiaomi-token-plan-ams.ts` @ pi 0.82.1
//! (2efa728) — T13 W4.
//!
//! Xiaomi MiMo token-plan Amsterdam endpoint. Models come from the vendored
//! catalog (upstream `xiaomi-token-plan-ams.models.ts` is a generated
//! `flattenModelCatalog` re-export of
//! `providers/data/xiaomi-token-plan-ams.json`, corrections baked in).

use std::sync::Arc;

use crate::api::openai_completions::OpenAiCompletions;
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::generated::get_builtin_models;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};

/// `xiaomiTokenPlanAmsProvider()`.
pub fn xiaomi_token_plan_ams_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "xiaomi-token-plan-ams".to_owned(),
        name: Some("Xiaomi Token Plan AMS".to_owned()),
        base_url: Some("https://token-plan-ams.xiaomimimo.com/v1".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "Xiaomi Token Plan AMS API key",
                &["XIAOMI_TOKEN_PLAN_AMS_API_KEY"],
            ))),
            oauth: None,
        },
        models: get_builtin_models("xiaomi-token-plan-ams").to_vec(),
        api: ProviderApi::Single(Arc::new(OpenAiCompletions)),
        ..Default::default()
    })
}
