//! T13 W4 phase 2, group B: provider-factory tests for `github-copilot`,
//! `openrouter`, `vercel-ai-gateway`, `cloudflare-ai-gateway`,
//! `cloudflare-workers-ai`, `opencode`, `opencode-go`, `radius`.
//!
//! Test intents ported from `packages/ai/test/providers.test.ts` @ pi
//! 0.82.1+ (2efa728, updated through 19451accd) where they cover these
//! providers ("requires Cloudflare Workers AI account config and returns
//! scoped env", "requires Cloudflare AI Gateway account config and gateway
//! config and returns scoped env headers", "builtinModels registers every
//! builtin provider with models" — Radius ships the static public catalog
//! since 4d38031fb), plus `test/github-copilot-anthropic.test.ts` catalog
//! assertions (Copilot Claude thinking-level maps / static headers).
//! `test/cloudflare-stream.test.ts` is ported as unit tests in
//! `src/providers/cloudflare_stream.rs`. Per the group charter every factory
//! gets: id/name/base-url, auth shape, catalog model count; mixed-API
//! factories assert the dispatch-key set against the catalog.

use std::collections::HashMap;
use std::sync::Arc;

use rpi_ai::auth::{AuthContext, Credential, OAuthCredential};
use rpi_ai::generated::get_builtin_model;
use rpi_ai::models::{CreateModelsOptions, Models, Provider, get_supported_thinking_levels};
use rpi_ai::providers::cloudflare_ai_gateway::cloudflare_ai_gateway_provider;
use rpi_ai::providers::cloudflare_workers_ai::cloudflare_workers_ai_provider;
use rpi_ai::providers::github_copilot::github_copilot_provider;
use rpi_ai::providers::opencode::opencode_provider;
use rpi_ai::providers::opencode_go::opencode_go_provider;
use rpi_ai::providers::openrouter::openrouter_provider;
use rpi_ai::providers::radius::{RadiusProviderOptions, radius_provider, radius_provider_with};
use rpi_ai::providers::vercel_ai_gateway::vercel_ai_gateway_provider;
use rpi_ai::types::{ApiKind, ModelThinkingLevel, ProviderHeaders};

/// `fakeAuthContext` from the upstream test file.
struct FakeAuthContext {
    env: HashMap<String, String>,
}

impl FakeAuthContext {
    fn new(env: &[(&str, &str)]) -> Self {
        Self {
            env: env
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
        }
    }
}

#[async_trait::async_trait]
impl AuthContext for FakeAuthContext {
    async fn env(&self, name: &str) -> Option<String> {
        self.env.get(name).cloned()
    }

    async fn file_exists(&self, _path: &str) -> bool {
        false
    }
}

fn models_with_context(ctx: FakeAuthContext) -> Models {
    Models::new(Some(CreateModelsOptions {
        credentials: None,
        auth_context: Some(Arc::new(ctx)),
        models_store: None,
    }))
}

/// Shared per-factory assertions: identity, base url, non-empty catalog with
/// matching provider and an api inside the expected dispatch-key set.
fn assert_factory_basics(
    provider: &Arc<dyn Provider>,
    id: &str,
    name: &str,
    base_url: Option<&str>,
    apis: &[&str],
) {
    assert_eq!(provider.id(), id);
    assert_eq!(provider.name(), name);
    assert_eq!(provider.base_url(), base_url);
    let models = provider.get_models();
    assert!(!models.is_empty(), "{id}: catalog models expected");
    for model in &models {
        assert_eq!(model.provider, id);
        assert!(
            apis.contains(&model.api.as_str()),
            "{id}: {} has api {} outside the dispatch keys {apis:?}",
            model.id,
            model.api
        );
    }
}

// ---------------------------------------------------------------------------
// github-copilot
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_github_copilot_factory_config_and_auth() {
    let provider = github_copilot_provider();
    assert_factory_basics(
        &provider,
        "github-copilot",
        "GitHub Copilot",
        Some("https://api.individual.githubcopilot.com"),
        &[
            ApiKind::ANTHROPIC_MESSAGES,
            ApiKind::OPENAI_COMPLETIONS,
            ApiKind::OPENAI_RESPONSES,
        ],
    );

    let auth = provider.auth();
    let api_key = auth.api_key.as_ref().expect("api key auth");
    assert_eq!(api_key.name(), "GitHub Copilot token");
    // OAuth surface present (W5 wires the device-code flow); upstream name.
    let oauth = auth.oauth.as_ref().expect("oauth placeholder");
    assert_eq!(oauth.name(), "GitHub Copilot");

    let ctx = FakeAuthContext::new(&[("COPILOT_GITHUB_TOKEN", "copilot-token")]);
    let result = api_key
        .resolve(&ctx, None)
        .await
        .expect("resolve")
        .expect("configured");
    assert_eq!(result.auth.api_key.as_deref(), Some("copilot-token"));
    assert_eq!(result.source.as_deref(), Some("COPILOT_GITHUB_TOKEN"));

    let ctx = FakeAuthContext::new(&[]);
    assert!(
        api_key
            .resolve(&ctx, None)
            .await
            .expect("resolve")
            .is_none()
    );
}

/// `filterModels` (github-copilot.ts:19-27) through the factory's provider.
#[test]
fn test_github_copilot_filter_models() {
    let provider = github_copilot_provider();
    let models = provider.get_models();

    // Non-OAuth credential: catalog untouched.
    let api_key = Credential::ApiKey(rpi_ai::auth::ApiKeyCredential {
        key: Some("k".to_owned()),
        env: None,
    });
    assert_eq!(
        provider.filter_models(models.clone(), Some(&api_key)),
        models
    );

    // OAuth credential with availableModelIds: intersect, keep catalog order.
    let kept = models[0].id.clone();
    let mut extra = serde_json::Map::new();
    extra.insert(
        "availableModelIds".to_owned(),
        serde_json::json!([kept, "ghost-model"]),
    );
    let oauth = Credential::OAuth(OAuthCredential {
        refresh: "r".to_owned(),
        access: "a".to_owned(),
        expires: i64::MAX,
        extra,
    });
    let filtered = provider.filter_models(models.clone(), Some(&oauth));
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].id, kept);
}

/// Catalog intent of `test/github-copilot-anthropic.test.ts`: Copilot Claude
/// models ride `anthropic-messages` with adaptive-thinking overrides and the
/// static Copilot headers.
#[test]
fn test_github_copilot_catalog_claude_models() {
    let sonnet =
        get_builtin_model("github-copilot", "claude-sonnet-4.6").expect("claude-sonnet-4.6");
    assert_eq!(sonnet.api, ApiKind::ANTHROPIC_MESSAGES.into());
    assert_eq!(sonnet.context_window, 1_000_000);
    let map = sonnet
        .thinking_level_map
        .as_ref()
        .expect("thinkingLevelMap");
    assert_eq!(
        map.get(&ModelThinkingLevel::Minimal),
        Some(&Some("low".to_owned()))
    );
    assert_eq!(
        map.get(&ModelThinkingLevel::Max),
        Some(&Some("max".to_owned()))
    );
    let levels = get_supported_thinking_levels(sonnet);
    assert!(levels.contains(&ModelThinkingLevel::Max));
    assert!(!levels.contains(&ModelThinkingLevel::Xhigh));
    let headers = sonnet.headers.as_ref().expect("copilot headers");
    assert!(
        headers
            .get("User-Agent")
            .is_some_and(|value| value.contains("GitHubCopilotChat")),
        "User-Agent: {headers:?}"
    );
    assert_eq!(
        headers.get("Copilot-Integration-Id").map(String::as_str),
        Some("vscode-chat")
    );

    let opus47 = get_builtin_model("github-copilot", "claude-opus-4.7").expect("claude-opus-4.7");
    let levels = get_supported_thinking_levels(opus47);
    assert!(levels.contains(&ModelThinkingLevel::Xhigh));
    assert!(levels.contains(&ModelThinkingLevel::Max));
}

// ---------------------------------------------------------------------------
// openrouter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_openrouter_factory_config_and_auth() {
    let provider = openrouter_provider();
    assert_factory_basics(
        &provider,
        "openrouter",
        "OpenRouter",
        Some("https://openrouter.ai/api/v1"),
        // `650e7a612` (#8454): dual-api dispatch — anthropic-messages entries
        // arrive with the V14-09 catalog regen; current catalog entries are
        // all openai-completions.
        &[ApiKind::ANTHROPIC_MESSAGES, ApiKind::OPENAI_COMPLETIONS],
    );

    let auth = provider.auth();
    let api_key = auth.api_key.as_ref().expect("api key auth");
    assert_eq!(api_key.name(), "OpenRouter API key");
    let oauth = auth.oauth.as_ref().expect("oauth");
    assert_eq!(oauth.name(), "OpenRouter OAuth");

    let ctx = FakeAuthContext::new(&[("OPENROUTER_API_KEY", "sk-or-key")]);
    let result = api_key
        .resolve(&ctx, None)
        .await
        .expect("resolve")
        .expect("configured");
    assert_eq!(result.auth.api_key.as_deref(), Some("sk-or-key"));
    assert_eq!(result.source.as_deref(), Some("OPENROUTER_API_KEY"));
}

// ---------------------------------------------------------------------------
// vercel-ai-gateway
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_vercel_ai_gateway_factory_config_and_auth() {
    let provider = vercel_ai_gateway_provider();
    assert_factory_basics(
        &provider,
        "vercel-ai-gateway",
        "Vercel AI Gateway",
        Some("https://ai-gateway.vercel.sh"),
        &[ApiKind::ANTHROPIC_MESSAGES],
    );

    let auth = provider.auth();
    assert!(auth.oauth.is_none());
    let api_key = auth.api_key.as_ref().expect("api key auth");
    assert_eq!(api_key.name(), "Vercel AI Gateway API key");

    let ctx = FakeAuthContext::new(&[("AI_GATEWAY_API_KEY", "vercel-key")]);
    let result = api_key
        .resolve(&ctx, None)
        .await
        .expect("resolve")
        .expect("configured");
    assert_eq!(result.auth.api_key.as_deref(), Some("vercel-key"));
    assert_eq!(result.source.as_deref(), Some("AI_GATEWAY_API_KEY"));
}

// ---------------------------------------------------------------------------
// cloudflare-workers-ai
// ---------------------------------------------------------------------------

/// Upstream: "requires Cloudflare Workers AI account config and returns
/// scoped env" (providers.test.ts:155-168).
#[tokio::test]
async fn test_cloudflare_workers_ai_requires_account_config() {
    let provider = cloudflare_workers_ai_provider();
    assert_eq!(provider.id(), "cloudflare-workers-ai");
    assert_eq!(provider.name(), "Cloudflare Workers AI");
    assert_eq!(provider.base_url(), None);
    let models = provider.get_models();
    assert!(!models.is_empty());
    let model = &models[0];

    let missing = models_with_context(FakeAuthContext::new(&[("CLOUDFLARE_API_KEY", "cf-key")]));
    missing.set_provider(cloudflare_workers_ai_provider());
    assert!(
        missing
            .get_auth(model, None)
            .await
            .expect("get_auth")
            .is_none()
    );

    let configured = models_with_context(FakeAuthContext::new(&[
        ("CLOUDFLARE_API_KEY", "cf-key"),
        ("CLOUDFLARE_ACCOUNT_ID", "account-id"),
    ]));
    configured.set_provider(cloudflare_workers_ai_provider());
    let result = configured
        .get_auth(model, None)
        .await
        .expect("get_auth")
        .expect("configured");
    assert_eq!(result.auth.api_key.as_deref(), Some("cf-key"));
    assert_eq!(
        result.env,
        Some(rpi_ai::types::ProviderEnv::from([(
            "CLOUDFLARE_ACCOUNT_ID".to_owned(),
            "account-id".to_owned()
        )]))
    );
}

// ---------------------------------------------------------------------------
// cloudflare-ai-gateway
// ---------------------------------------------------------------------------

/// Upstream: "requires Cloudflare AI Gateway account and gateway config and
/// returns scoped env headers" (providers.test.ts:170-198).
#[tokio::test]
async fn test_cloudflare_ai_gateway_requires_account_and_gateway_config() {
    let provider = cloudflare_ai_gateway_provider();
    assert_eq!(provider.id(), "cloudflare-ai-gateway");
    assert_eq!(provider.name(), "Cloudflare AI Gateway");
    assert_eq!(provider.base_url(), None);
    let models = provider.get_models();
    assert!(!models.is_empty());
    for model in &models {
        assert!(
            [
                ApiKind::ANTHROPIC_MESSAGES,
                ApiKind::OPENAI_COMPLETIONS,
                ApiKind::OPENAI_RESPONSES,
            ]
            .contains(&model.api.as_str()),
            "{}: api {}",
            model.id,
            model.api
        );
    }
    let model = &models[0];

    let missing_gateway = models_with_context(FakeAuthContext::new(&[
        ("CLOUDFLARE_API_KEY", "cf-key"),
        ("CLOUDFLARE_ACCOUNT_ID", "account-id"),
    ]));
    missing_gateway.set_provider(cloudflare_ai_gateway_provider());
    assert!(
        missing_gateway
            .get_auth(model, None)
            .await
            .expect("get_auth")
            .is_none()
    );

    let configured = models_with_context(FakeAuthContext::new(&[
        ("CLOUDFLARE_API_KEY", "cf-key"),
        ("CLOUDFLARE_ACCOUNT_ID", "account-id"),
        ("CLOUDFLARE_GATEWAY_ID", "gateway-id"),
    ]));
    configured.set_provider(cloudflare_ai_gateway_provider());
    let result = configured
        .get_auth(model, None)
        .await
        .expect("get_auth")
        .expect("configured");
    let headers = result.auth.headers.clone().unwrap_or_default();
    for (name, value) in ProviderHeaders::from([
        (
            "cf-aig-authorization".to_owned(),
            Some("Bearer cf-key".to_owned()),
        ),
        ("Authorization".to_owned(), None),
        ("x-api-key".to_owned(), None),
    ]) {
        assert_eq!(headers.get(&name), Some(&value), "header {name}");
    }
    assert_eq!(
        result.env,
        Some(rpi_ai::types::ProviderEnv::from([
            ("CLOUDFLARE_ACCOUNT_ID".to_owned(), "account-id".to_owned()),
            ("CLOUDFLARE_GATEWAY_ID".to_owned(), "gateway-id".to_owned()),
        ]))
    );
}

// ---------------------------------------------------------------------------
// opencode / opencode-go
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_opencode_factory_config_and_auth() {
    let provider = opencode_provider();
    assert_factory_basics(
        &provider,
        "opencode",
        "OpenCode Zen",
        None,
        &[
            ApiKind::ANTHROPIC_MESSAGES,
            ApiKind::GOOGLE_GENERATIVE_AI,
            ApiKind::OPENAI_COMPLETIONS,
            ApiKind::OPENAI_RESPONSES,
        ],
    );

    let auth = provider.auth();
    assert!(auth.oauth.is_none());
    let api_key = auth.api_key.as_ref().expect("api key auth");
    assert_eq!(api_key.name(), "OpenCode API key");

    let ctx = FakeAuthContext::new(&[("OPENCODE_API_KEY", "oc-key")]);
    let result = api_key
        .resolve(&ctx, None)
        .await
        .expect("resolve")
        .expect("configured");
    assert_eq!(result.auth.api_key.as_deref(), Some("oc-key"));
    assert_eq!(result.source.as_deref(), Some("OPENCODE_API_KEY"));
}

#[tokio::test]
async fn test_opencode_go_factory_config_and_auth() {
    let provider = opencode_go_provider();
    assert_factory_basics(
        &provider,
        "opencode-go",
        "OpenCode Go",
        None,
        &[
            ApiKind::ANTHROPIC_MESSAGES,
            ApiKind::GOOGLE_GENERATIVE_AI,
            ApiKind::OPENAI_COMPLETIONS,
            ApiKind::OPENAI_RESPONSES,
        ],
    );

    let auth = provider.auth();
    assert!(auth.oauth.is_none());
    let api_key = auth.api_key.as_ref().expect("api key auth");
    assert_eq!(api_key.name(), "OpenCode API key");

    let ctx = FakeAuthContext::new(&[("OPENCODE_API_KEY", "oc-go-key")]);
    let result = api_key
        .resolve(&ctx, None)
        .await
        .expect("resolve")
        .expect("configured");
    assert_eq!(result.auth.api_key.as_deref(), Some("oc-go-key"));
}

// ---------------------------------------------------------------------------
// radius
// ---------------------------------------------------------------------------

/// Upstream (providers.test.ts:41 / radius-provider.test.ts
/// "ships a static public catalog for the default gateway"): the default
/// gateway provider carries the vendored public catalog from construction;
/// only the overlay is dynamic.
#[tokio::test]
async fn test_radius_factory_config_and_auth() {
    let provider = radius_provider();
    assert_eq!(provider.id(), "radius");
    assert_eq!(provider.name(), "Radius");
    assert_eq!(provider.base_url(), None);
    assert!(!provider.get_models().is_empty());
    assert!(
        provider
            .get_models()
            .iter()
            .any(|model| model.id == "balanced")
    );

    let auth = provider.auth();
    let api_key = auth.api_key.as_ref().expect("api key auth");
    assert_eq!(api_key.name(), "Radius API key");
    let oauth = auth.oauth.as_ref().expect("oauth placeholder");
    assert_eq!(oauth.name(), "Radius");

    let ctx = FakeAuthContext::new(&[("RADIUS_API_KEY", "radius-key")]);
    let result = api_key
        .resolve(&ctx, None)
        .await
        .expect("resolve")
        .expect("configured");
    assert_eq!(result.auth.api_key.as_deref(), Some("radius-key"));
    assert_eq!(result.source.as_deref(), Some("RADIUS_API_KEY"));
}

#[test]
fn test_radius_provider_options_normalize_gateway() {
    let provider = radius_provider_with(RadiusProviderOptions::default());
    assert_eq!(provider.gateway(), "https://radius.pi.dev");

    let provider = radius_provider_with(RadiusProviderOptions {
        id: Some("radius-eu".to_owned()),
        name: Some("Radius EU".to_owned()),
        gateway: Some("radius.eu.example.com/".to_owned()),
    });
    assert_eq!(provider.id(), "radius-eu");
    assert_eq!(provider.name(), "Radius EU");
    assert_eq!(provider.gateway(), "https://radius.eu.example.com");
}
