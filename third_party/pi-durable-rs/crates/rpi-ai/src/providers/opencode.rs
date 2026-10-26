//! Port of `packages/ai/src/providers/opencode.ts` @ pi 0.82.1 (2efa728) —
//! OpenCode Zen: mixed 4-API dispatch on `model.api` (`anthropic-messages` /
//! `google-generative-ai` / `openai-completions` / `openai-responses`).

use std::collections::HashMap;
use std::sync::Arc;

use crate::api::anthropic_messages::AnthropicMessages;
use crate::api::google_generative_ai::GoogleGenerativeAi;
use crate::api::openai_completions::OpenAiCompletions;
use crate::api::openai_responses::OpenAiResponses;
use crate::api::session_affinity::with_opencode_session_header;
use crate::api::typesafe_system_one::{TYPESAFE_SYSTEM_ONE_API, typesafe_system_one_api};
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::generated::{get_builtin_all_models, get_builtin_models};
use crate::models::{
    CreateProviderOptions, Provider, ProviderApi, ProviderStreams, create_provider,
};
use crate::types::ApiKind;

/// `opencodeProvider()`.
pub fn opencode_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "opencode".to_owned(),
        name: Some("OpenCode Zen".to_owned()),
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "OpenCode API key",
                &["OPENCODE_API_KEY"],
            ))),
            oauth: None,
        },
        models: get_builtin_models("opencode").to_vec(),
        all_models: get_builtin_all_models("opencode").to_vec(),
        classifiers: HashMap::from([(
            TYPESAFE_SYSTEM_ONE_API.to_owned(),
            typesafe_system_one_api(),
        )]),
        api: ProviderApi::Map(api_map()),
        ..Default::default()
    })
}

/// Mixed-API dispatch table (opencode.ts:17-22). #9326 (561a2e066): every
/// adapter is wrapped with the `x-opencode-session` header decorator
/// (upstream `withOpenCodeSessionHeader`).
fn api_map() -> HashMap<String, Arc<dyn ProviderStreams>> {
    HashMap::from([
        (
            ApiKind::ANTHROPIC_MESSAGES.to_owned(),
            with_opencode_session_header(Arc::new(AnthropicMessages) as Arc<dyn ProviderStreams>),
        ),
        (
            ApiKind::GOOGLE_GENERATIVE_AI.to_owned(),
            with_opencode_session_header(Arc::new(GoogleGenerativeAi) as Arc<dyn ProviderStreams>),
        ),
        (
            ApiKind::OPENAI_COMPLETIONS.to_owned(),
            with_opencode_session_header(Arc::new(OpenAiCompletions) as Arc<dyn ProviderStreams>),
        ),
        (
            ApiKind::OPENAI_RESPONSES.to_owned(),
            with_opencode_session_header(Arc::new(OpenAiResponses) as Arc<dyn ProviderStreams>),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_map_covers_the_four_catalog_apis() {
        let map = api_map();
        for key in [
            ApiKind::ANTHROPIC_MESSAGES,
            ApiKind::GOOGLE_GENERATIVE_AI,
            ApiKind::OPENAI_COMPLETIONS,
            ApiKind::OPENAI_RESPONSES,
        ] {
            assert!(map.contains_key(key), "missing dispatch key {key}");
        }
        assert_eq!(map.len(), 4);
        for model in get_builtin_models("opencode") {
            assert!(
                map.contains_key(model.api.as_str()),
                "catalog model {} has undispatched api {}",
                model.id,
                model.api
            );
        }
    }

    /// #9326 (561a2e066): provider dispatch wraps every adapter with the
    /// `x-opencode-session` decorator — e2e through the real api map.
    #[tokio::test]
    async fn api_map_dispatch_sends_opencode_session_header() {
        let (base_url, raw_rx) = crate::api::session_affinity::tests::capture_server().await;
        let model: crate::types::Model = serde_json::from_value(serde_json::json!({
            "id": "claude-sonnet-4.6", "name": "m", "api": "anthropic-messages",
            "provider": "opencode", "baseUrl": base_url, "reasoning": false, "input": ["text"],
            "cost": {"input": 1.0, "output": 1.0, "cacheRead": 0.1, "cacheWrite": 1.0},
            "contextWindow": 1000, "maxTokens": 100
        }))
        .expect("model");
        let streams = api_map()
            .get(ApiKind::ANTHROPIC_MESSAGES)
            .expect("dispatch entry")
            .clone();
        let mut options = crate::types::StreamOptions {
            session_id: Some("oc-provider-sess".to_owned()),
            ..Default::default()
        };
        options.request.api_key = Some("test-key".to_owned());
        let stream = streams.stream(
            &model,
            &crate::utils::transcript::normalize_context(&crate::types::Context::default()),
            Some(options),
        );
        let _ = stream.result().await;
        let raw = tokio::time::timeout(std::time::Duration::from_secs(5), raw_rx)
            .await
            .expect("captured")
            .expect("server sent");
        assert!(
            crate::api::session_affinity::tests::captured_has_header(
                &raw,
                "x-opencode-session",
                "oc-provider-sess"
            ),
            "x-opencode-session must be sent via provider dispatch; raw:\n{raw}"
        );
    }
}
