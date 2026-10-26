//! Port of `packages/ai/src/providers/cloudflare-ai-gateway.ts` @ pi 0.82.1
//! (2efa728) — Cloudflare AI Gateway: mixed 3-API dispatch on `model.api`,
//! every adapter wrapped so `{CLOUDFLARE_ACCOUNT_ID}` /
//! `{CLOUDFLARE_GATEWAY_ID}` base-URL placeholders materialize from the
//! resolved provider env before dispatch (see `cloudflare_stream`).
//!
//! Auth (`cloudflare-auth.ts`) lands in `crate::auth::cloudflare_auth`:
//! `CLOUDFLARE_API_KEY` + `CLOUDFLARE_ACCOUNT_ID` + `CLOUDFLARE_GATEWAY_ID`,
//! request auth via the `cf-aig-authorization` header.

use std::collections::HashMap;
use std::sync::Arc;

use crate::api::anthropic_messages::AnthropicMessages;
use crate::api::openai_completions::OpenAiCompletions;
use crate::api::openai_responses::OpenAiResponses;
use crate::auth::ProviderAuth;
use crate::auth::cloudflare_auth::cloudflare_ai_gateway_auth;
use crate::generated::get_builtin_models;
use crate::models::{
    CreateProviderOptions, Provider, ProviderApi, ProviderStreams, create_provider,
};
use crate::types::ApiKind;

use super::cloudflare_stream::cloudflare_streams;

/// `cloudflareAIGatewayProvider()`.
pub fn cloudflare_ai_gateway_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "cloudflare-ai-gateway".to_owned(),
        name: Some("Cloudflare AI Gateway".to_owned()),
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(cloudflare_ai_gateway_auth()),
            oauth: None,
        },
        models: get_builtin_models("cloudflare-ai-gateway").to_vec(),
        api: ProviderApi::Map(api_map()),
        ..Default::default()
    })
}

/// Mixed-API dispatch table (cloudflare-ai-gateway.ts:17-21).
fn api_map() -> HashMap<String, Arc<dyn ProviderStreams>> {
    HashMap::from([
        (
            ApiKind::ANTHROPIC_MESSAGES.to_owned(),
            cloudflare_streams(Arc::new(AnthropicMessages)),
        ),
        (
            ApiKind::OPENAI_COMPLETIONS.to_owned(),
            cloudflare_streams(Arc::new(OpenAiCompletions)),
        ),
        (
            ApiKind::OPENAI_RESPONSES.to_owned(),
            cloudflare_streams(Arc::new(OpenAiResponses)),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_map_covers_the_three_catalog_apis() {
        let map = api_map();
        assert_eq!(map.len(), 3);
        for model in get_builtin_models("cloudflare-ai-gateway") {
            assert!(
                map.contains_key(model.api.as_str()),
                "catalog model {} has undispatched api {}",
                model.id,
                model.api
            );
        }
    }

    #[test]
    fn catalog_base_urls_carry_the_gateway_placeholders() {
        for model in get_builtin_models("cloudflare-ai-gateway") {
            assert!(
                model.base_url.contains("{CLOUDFLARE_ACCOUNT_ID}"),
                "{}: {}",
                model.id,
                model.base_url
            );
            assert!(
                model.base_url.contains("{CLOUDFLARE_GATEWAY_ID}"),
                "{}: {}",
                model.id,
                model.base_url
            );
        }
    }
}
