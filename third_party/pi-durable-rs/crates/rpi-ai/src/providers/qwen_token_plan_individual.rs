//! Port of `packages/ai/src/providers/qwen-token-plan-individual.ts` @ pi
//! `c03d78bdc` / `4181f66`.
//!
//! The Individual variant reuses the international Token Plan endpoint and
//! `QWEN_TOKEN_PLAN_API_KEY` but ships a narrower 7-model catalog. The model
//! whitelist (`QWEN_TOKEN_PLAN_INDIVIDUAL_MODEL_IDS` in `generate-models.ts`)
//! is enforced upstream by `assertExactModelIds()` during generation, so the
//! vendored catalog is authoritative here.

use std::sync::Arc;

use crate::api::openai_completions::OpenAiCompletions;
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::generated::get_builtin_models;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};

/// `qwenTokenPlanIndividualProvider()`.
pub fn qwen_token_plan_individual_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "qwen-token-plan-individual".to_owned(),
        name: Some("Qwen Token Plan Individual".to_owned()),
        base_url: Some(
            "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1".to_owned(),
        ),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "Qwen Token Plan Individual API key",
                &["QWEN_TOKEN_PLAN_API_KEY"],
            ))),
            oauth: None,
        },
        models: get_builtin_models("qwen-token-plan-individual").to_vec(),
        api: ProviderApi::Single(Arc::new(OpenAiCompletions)),
        ..Default::default()
    })
}
