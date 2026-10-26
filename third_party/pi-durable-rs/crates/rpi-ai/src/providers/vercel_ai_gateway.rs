//! Port of `packages/ai/src/providers/vercel-ai-gateway.ts` @ pi 0.82.1
//! (2efa728) — Vercel AI Gateway, Anthropic Messages transport.

use std::sync::Arc;

use crate::api::anthropic_messages::AnthropicMessages;
use crate::api::typesafe_system_one::{TYPESAFE_SYSTEM_ONE_API, typesafe_system_one_api};
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::generated::{get_builtin_all_models, get_builtin_models};
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};

/// `vercelAIGatewayProvider()`.
pub fn vercel_ai_gateway_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "vercel-ai-gateway".to_owned(),
        name: Some("Vercel AI Gateway".to_owned()),
        base_url: Some("https://ai-gateway.vercel.sh".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "Vercel AI Gateway API key",
                &["AI_GATEWAY_API_KEY"],
            ))),
            oauth: None,
        },
        models: get_builtin_models("vercel-ai-gateway").to_vec(),
        all_models: get_builtin_all_models("vercel-ai-gateway").to_vec(),
        classifiers: std::collections::HashMap::from([(
            TYPESAFE_SYSTEM_ONE_API.to_owned(),
            typesafe_system_one_api(),
        )]),
        api: ProviderApi::Single(Arc::new(AnthropicMessages)),
        ..Default::default()
    })
}
