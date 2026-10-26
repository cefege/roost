//! Port of `packages/ai/src/providers/cloudflare-stream.ts` @ pi 0.82.1
//! (2efa728) — Cloudflare endpoint placeholder materialization.
//!
//! Cloudflare catalog base URLs carry `{CLOUDFLARE_ACCOUNT_ID}` /
//! `{CLOUDFLARE_GATEWAY_ID}` placeholders; [`cloudflare_streams`] wraps an API
//! adapter so they materialize from the resolved provider env
//! (`StreamOptions::env`) before dispatch.

use std::sync::Arc;

use crate::models::ProviderStreams;
use crate::types::{Model, ProviderEnv, SimpleStreamOptions, StreamOptions, TranscriptContext};
use crate::utils::event_stream::AssistantMessageEventStream;

pub const CLOUDFLARE_ACCOUNT_ID: &str = "CLOUDFLARE_ACCOUNT_ID";
pub const CLOUDFLARE_GATEWAY_ID: &str = "CLOUDFLARE_GATEWAY_ID";

const ACCOUNT_ID_PLACEHOLDER: &str = "{CLOUDFLARE_ACCOUNT_ID}";
const GATEWAY_ID_PLACEHOLDER: &str = "{CLOUDFLARE_GATEWAY_ID}";

/// `resolveCloudflareModel` — replace the placeholders a resolved env knows;
/// unknown placeholders stay verbatim (upstream `?? "{…}"` fallback).
pub fn resolve_cloudflare_model(model: &Model, env: Option<&ProviderEnv>) -> Model {
    let Some(env) = env else {
        return model.clone();
    };
    let account_id = env
        .get(CLOUDFLARE_ACCOUNT_ID)
        .map(String::as_str)
        .unwrap_or(ACCOUNT_ID_PLACEHOLDER);
    let gateway_id = env
        .get(CLOUDFLARE_GATEWAY_ID)
        .map(String::as_str)
        .unwrap_or(GATEWAY_ID_PLACEHOLDER);
    let base_url = model
        .base_url
        .replace(ACCOUNT_ID_PLACEHOLDER, account_id)
        .replace(GATEWAY_ID_PLACEHOLDER, gateway_id);
    if base_url == model.base_url {
        model.clone()
    } else {
        Model {
            base_url,
            ..model.clone()
        }
    }
}

/// `cloudflareStreams` — see module docs.
pub fn cloudflare_streams(streams: Arc<dyn ProviderStreams>) -> Arc<dyn ProviderStreams> {
    Arc::new(CloudflareStreams { inner: streams })
}

struct CloudflareStreams {
    inner: Arc<dyn ProviderStreams>,
}

impl ProviderStreams for CloudflareStreams {
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        let model = resolve_cloudflare_model(model, options.as_ref().and_then(|o| o.env.as_ref()));
        self.inner.stream(&model, context, options)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> Result<AssistantMessageEventStream, String> {
        let model =
            resolve_cloudflare_model(model, options.as_ref().and_then(|o| o.stream.env.as_ref()));
        self.inner.stream_simple(&model, context, options)
    }
}

#[cfg(test)]
mod tests {
    //! Port intent of `test/cloudflare-stream.test.ts` @ 2efa728: the wrapper
    //! materializes the model endpoint before dispatch (stream and
    //! stream_simple), and keeps placeholders the env cannot resolve.

    use super::*;
    use crate::types::{InputModality, ModelCost};

    fn placeholder_model() -> Model {
        Model {
            id: "model".to_owned(),
            name: "model".to_owned(),
            api: crate::types::ApiKind::from(crate::types::ApiKind::OPENAI_COMPLETIONS),
            provider: "cloudflare-ai-gateway".to_owned(),
            base_url: "https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/openai".to_owned(),
            reasoning: false,
            thinking_level_map: None,
            input: vec![InputModality::Text],
            input_limits: None,
            cost: ModelCost::default(),
            prompt_cache: None,
            context_window: 1000,
            max_tokens: 100,
            headers: None,
            compat: None,
            sampling_params: None,
                    sampling_params_by_thinking_level: None,
}
    }

    #[test]
    fn materializes_the_model_endpoint_before_dispatch() {
        let model = placeholder_model();
        let env = ProviderEnv::from([
            (CLOUDFLARE_ACCOUNT_ID.to_owned(), "account".to_owned()),
            (CLOUDFLARE_GATEWAY_ID.to_owned(), "gateway".to_owned()),
        ]);
        let resolved = resolve_cloudflare_model(&model, Some(&env));
        assert_eq!(
            resolved.base_url,
            "https://gateway.ai.cloudflare.com/v1/account/gateway/openai"
        );
    }

    #[test]
    fn keeps_placeholders_when_the_env_does_not_resolve_them() {
        let model = placeholder_model();
        assert_eq!(
            resolve_cloudflare_model(&model, None).base_url,
            model.base_url
        );
        // Gateway-only env: the account placeholder stays verbatim.
        let env = ProviderEnv::from([(CLOUDFLARE_GATEWAY_ID.to_owned(), "gateway".to_owned())]);
        assert_eq!(
            resolve_cloudflare_model(&model, Some(&env)).base_url,
            "https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/gateway/openai"
        );
    }

    #[test]
    fn no_change_returns_the_model_unmodified() {
        let mut model = placeholder_model();
        model.base_url = "https://example.com/v1".to_owned();
        let env = ProviderEnv::from([(CLOUDFLARE_ACCOUNT_ID.to_owned(), "account".to_owned())]);
        assert_eq!(resolve_cloudflare_model(&model, Some(&env)), model);
    }
}
