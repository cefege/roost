//! Port of `packages/ai/src/providers/xiaomi-token-plan-sgp.ts` @ pi 0.82.1
//! (2efa728) — T13 W4.
//!
//! Xiaomi MiMo token-plan Singapore endpoint. Models come from the vendored
//! catalog (upstream `xiaomi-token-plan-sgp.models.ts` is a generated
//! `flattenModelCatalog` re-export of
//! `providers/data/xiaomi-token-plan-sgp.json`, corrections baked in).

use std::sync::Arc;

use crate::api::openai_completions::OpenAiCompletions;
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::generated::get_builtin_models;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};

/// `xiaomiTokenPlanSgpProvider()`.
pub fn xiaomi_token_plan_sgp_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "xiaomi-token-plan-sgp".to_owned(),
        name: Some("Xiaomi Token Plan SGP".to_owned()),
        base_url: Some("https://token-plan-sgp.xiaomimimo.com/v1".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "Xiaomi Token Plan SGP API key",
                &["XIAOMI_TOKEN_PLAN_SGP_API_KEY"],
            ))),
            oauth: None,
        },
        models: get_builtin_models("xiaomi-token-plan-sgp").to_vec(),
        api: ProviderApi::Single(Arc::new(OpenAiCompletions)),
        ..Default::default()
    })
}
