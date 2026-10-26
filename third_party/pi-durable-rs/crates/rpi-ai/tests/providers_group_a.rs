//! T13 W4 phase 2, group A: provider-factory tests for `openai`,
//! `openai-codex`, `azure-openai-responses`, `anthropic`, `google`,
//! `google-vertex`, `amazon-bedrock`, `mistral`.
//!
//! Test intents ported from `packages/ai/test/providers.test.ts` @ pi 0.82.1
//! (2efa728) where they cover these providers ("resolves Anthropic bearer
//! auth from env with auth token precedence", "preserves Anthropic OAuth
//! token precedence over the API key", "runs provider-owned Bedrock bearer
//! token and AWS profile login flows", "reports bedrock as configured from
//! ambient AWS credentials without an api key", "runs provider-owned Vertex
//! API key and ADC login flows", "resolves vertex via ADC file plus project
//! and location", plus the per-factory config/catalog assertions). Per the
//! group charter every factory gets: id/name/base-url, auth shape, non-empty
//! catalog models; factory-specific auth logic gets dedicated tests.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rpi_ai::auth::{
    AuthContext, AuthEvent, AuthInfoLink, AuthInteraction, AuthPrompt, BoxFutureSend, ModelsError,
};
use rpi_ai::generated::get_builtin_model;
use rpi_ai::models::{CreateModelsOptions, Models};
use rpi_ai::providers::amazon_bedrock::amazon_bedrock_provider;
use rpi_ai::providers::anthropic::anthropic_provider;
use rpi_ai::providers::azure::azure_provider;
use rpi_ai::providers::google::google_provider;
use rpi_ai::providers::google_vertex::google_vertex_provider;
use rpi_ai::providers::mistral::mistral_provider;
use rpi_ai::providers::openai::openai_provider;
use rpi_ai::providers::openai_codex::openai_codex_provider;

/// `fakeAuthContext` from the upstream test file, with ADC-file support.
struct FakeAuthContext {
    env: HashMap<String, String>,
    files: Vec<String>,
}

impl FakeAuthContext {
    fn new(env: &[(&str, &str)]) -> Self {
        Self {
            env: env
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            files: Vec::new(),
        }
    }

    fn with_files(env: &[(&str, &str)], files: &[&str]) -> Self {
        Self {
            files: files.iter().map(|file| (*file).to_owned()).collect(),
            ..Self::new(env)
        }
    }
}

#[async_trait::async_trait]
impl AuthContext for FakeAuthContext {
    async fn env(&self, name: &str) -> Option<String> {
        self.env.get(name).cloned()
    }

    async fn file_exists(&self, path: &str) -> bool {
        self.files.iter().any(|file| file == path)
    }
}

/// Upstream `{ prompt: async () => answers.shift()!, notify: (e) => events.push(e) }`.
struct RecordingInteraction {
    answers: Mutex<Vec<String>>,
    prompts: Mutex<Vec<AuthPrompt>>,
    events: Mutex<Vec<AuthEvent>>,
}

impl RecordingInteraction {
    fn new(answers: &[&str]) -> Self {
        Self {
            answers: Mutex::new(answers.iter().map(|answer| (*answer).to_owned()).collect()),
            prompts: Mutex::new(Vec::new()),
            events: Mutex::new(Vec::new()),
        }
    }

    fn events(&self) -> Vec<AuthEvent> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl AuthInteraction for RecordingInteraction {
    fn prompt<'a>(&'a self, prompt: AuthPrompt) -> BoxFutureSend<'a, Result<String, ModelsError>> {
        Box::pin(async move {
            self.prompts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(prompt);
            let answer = self
                .answers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(0);
            Ok(answer)
        })
    }

    fn notify(&self, event: AuthEvent) {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event);
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
/// matching provider/api on every model.
fn assert_factory_basics(
    provider: &Arc<dyn rpi_ai::models::Provider>,
    id: &str,
    name: &str,
    base_url: Option<&str>,
    api: &str,
) {
    assert_eq!(provider.id(), id);
    assert_eq!(provider.name(), name);
    assert_eq!(provider.base_url(), base_url);
    let models = provider.get_models();
    assert!(!models.is_empty(), "{id}: catalog models expected");
    for model in &models {
        assert_eq!(model.provider, id);
        assert_eq!(
            model.api,
            api.into(),
            "{id}: unexpected api for {}",
            model.id
        );
    }
}

// ---------------------------------------------------------------------------
// openai
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_openai_factory_config_and_auth() {
    let provider = openai_provider();
    assert_factory_basics(
        &provider,
        "openai",
        "OpenAI",
        Some("https://api.openai.com/v1"),
        "openai-responses",
    );
    assert!(provider.headers().is_none());

    let auth = provider.auth();
    // `02eed88fd`: the OpenAI provider gains the ChatGPT subscription OAuth
    // flow alongside the API key (two parallel channels).
    let oauth = auth.oauth.as_ref().expect("oauth auth");
    assert_eq!(oauth.name(), "OpenAI (ChatGPT subscription)");
    assert!(oauth.is_subscription());
    let api_key = auth.api_key.as_ref().expect("api key auth");
    assert_eq!(api_key.name(), "OpenAI API key");

    let ctx = FakeAuthContext::new(&[("OPENAI_API_KEY", "sk-openai")]);
    let result = api_key
        .resolve(&ctx, None)
        .await
        .expect("resolve")
        .expect("configured");
    assert_eq!(result.auth.api_key.as_deref(), Some("sk-openai"));
    assert_eq!(result.source.as_deref(), Some("OPENAI_API_KEY"));

    let ctx = FakeAuthContext::new(&[]);
    assert!(
        api_key
            .resolve(&ctx, None)
            .await
            .expect("resolve")
            .is_none()
    );
}

/// Upstream: "stores native constrained-sampling capabilities in model
/// metadata" (openai/anthropic half).
#[test]
fn test_openai_catalog_constrained_sampling_metadata() {
    let gpt4o = get_builtin_model("openai", "gpt-4o").expect("gpt-4o");
    let compat = gpt4o.compat.as_ref().expect("gpt-4o compat");
    assert_eq!(compat.supports_strict_mode, Some(true));
    assert_eq!(compat.supports_open_ai_grammar_tools, None);

    let gpt54 = get_builtin_model("openai", "gpt-5.4").expect("gpt-5.4");
    let compat = gpt54.compat.as_ref().expect("gpt-5.4 compat");
    assert_eq!(compat.supports_strict_mode, Some(true));
    assert_eq!(compat.supports_open_ai_grammar_tools, Some(true));

    let haiku = get_builtin_model("anthropic", "claude-haiku-4-5").expect("claude-haiku-4-5");
    assert_eq!(haiku.api, "anthropic-messages".into());
    assert_eq!(
        haiku
            .compat
            .as_ref()
            .and_then(|compat| compat.supports_strict_tools),
        Some(true)
    );
}

// ---------------------------------------------------------------------------
// openai-codex
// ---------------------------------------------------------------------------

#[test]
fn test_openai_codex_factory_config() {
    let provider = openai_codex_provider();
    assert_factory_basics(
        &provider,
        "openai-codex",
        "OpenAI Codex",
        Some("https://chatgpt.com/backend-api"),
        "openai-codex-responses",
    );
    // Deviation D-030 closed in W5: upstream auth is OAuth-only (no api-key
    // channel); the real flow is wired into the `oauth` slot.
    assert!(provider.auth().api_key.is_none());
    let oauth = provider.auth().oauth.as_ref().expect("oauth");
    assert_eq!(oauth.name(), "OpenAI (ChatGPT Plus/Pro)");
}

// ---------------------------------------------------------------------------
// azure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_azure_factory_config_and_auth() {
    let provider = azure_provider();
    assert_eq!(provider.id(), "azure");
    assert_eq!(provider.name(), "Azure");
    assert_eq!(provider.base_url(), None);
    let models = provider.get_models();
    assert!(!models.is_empty(), "azure: catalog models expected");
    for model in &models {
        assert_eq!(model.provider, "azure");
        assert!(
            matches!(
                model.api.as_str(),
                "azure-openai-responses" | "openai-completions"
            ),
            "azure: unexpected api for {}: {}",
            model.id,
            model.api
        );
    }
    // Foundry Chat Completions entry rides the shared openai-completions
    // transport; the Responses line keeps the unchanged api id (#9714).
    let foundry = models
        .iter()
        .find(|model| model.id == "deepseek-v4-pro")
        .expect("foundry deepseek-v4-pro");
    assert_eq!(foundry.api.as_str(), "openai-completions");
    let responses = models
        .iter()
        .find(|model| model.id == "gpt-5.4")
        .expect("gpt-5.4");
    assert_eq!(responses.api.as_str(), "azure-openai-responses");

    let auth = provider.auth();
    assert!(auth.oauth.is_none());
    let api_key = auth.api_key.as_ref().expect("api key auth");
    assert_eq!(api_key.name(), "Azure OpenAI API key");

    let ctx = FakeAuthContext::new(&[("AZURE_OPENAI_API_KEY", "azure-key")]);
    let result = api_key
        .resolve(&ctx, None)
        .await
        .expect("resolve")
        .expect("configured");
    assert_eq!(result.auth.api_key.as_deref(), Some("azure-key"));
    assert_eq!(result.source.as_deref(), Some("AZURE_OPENAI_API_KEY"));
}

// ---------------------------------------------------------------------------
// anthropic
// ---------------------------------------------------------------------------

#[test]
fn test_anthropic_factory_config() {
    let provider = anthropic_provider();
    assert_factory_basics(
        &provider,
        "anthropic",
        "Anthropic",
        Some("https://api.anthropic.com"),
        "anthropic-messages",
    );

    let auth = provider.auth();
    assert_eq!(
        auth.api_key.as_ref().expect("api key auth").name(),
        "Anthropic API key"
    );
    // Upstream wires `lazyOAuth(loadAnthropicOAuth)`; rpi has no bundle
    // splitting, so the OAuth half is registered directly.
    assert_eq!(
        auth.oauth.as_ref().expect("oauth auth").name(),
        "Anthropic (Claude Pro/Max)"
    );
}

/// Upstream: "resolves Anthropic bearer auth from env with auth token
/// precedence".
#[tokio::test]
async fn test_anthropic_bearer_auth_token_precedence() {
    let models = models_with_context(FakeAuthContext::new(&[
        ("ANTHROPIC_AUTH_TOKEN", "auth-token"),
        ("ANTHROPIC_OAUTH_TOKEN", "oauth-token"),
        ("ANTHROPIC_API_KEY", "api-key"),
    ]));
    models.set_provider(anthropic_provider());
    let model = models
        .get_model("anthropic", "claude-haiku-4-5")
        .expect("model");

    let result = models
        .get_auth(&model, None)
        .await
        .expect("get auth")
        .expect("configured");
    assert_eq!(result.auth.api_key, None);
    assert_eq!(
        result
            .auth
            .headers
            .as_ref()
            .and_then(|headers| headers.get("Authorization")),
        Some(&Some("Bearer auth-token".to_owned()))
    );
    assert_eq!(result.source.as_deref(), Some("ANTHROPIC_AUTH_TOKEN"));
    assert_eq!(result.env, None);
}

/// Upstream: "preserves Anthropic OAuth token precedence over the API key".
#[tokio::test]
async fn test_anthropic_oauth_token_beats_api_key_env() {
    let models = models_with_context(FakeAuthContext::new(&[
        ("ANTHROPIC_API_KEY", "key"),
        ("ANTHROPIC_OAUTH_TOKEN", "oauth-token"),
    ]));
    models.set_provider(anthropic_provider());
    let model = models
        .get_model("anthropic", "claude-haiku-4-5")
        .expect("model");

    let result = models
        .get_auth(&model, None)
        .await
        .expect("get auth")
        .expect("configured");
    assert_eq!(result.auth.api_key.as_deref(), Some("oauth-token"));
    assert_eq!(result.source.as_deref(), Some("ANTHROPIC_OAUTH_TOKEN"));
}

// ---------------------------------------------------------------------------
// google
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_google_factory_config_and_auth() {
    let provider = google_provider();
    assert_factory_basics(
        &provider,
        "google",
        "Google",
        Some("https://generativelanguage.googleapis.com/v1beta"),
        "google-generative-ai",
    );

    let auth = provider.auth();
    assert!(auth.oauth.is_none());
    let api_key = auth.api_key.as_ref().expect("api key auth");
    assert_eq!(api_key.name(), "Gemini API key");

    let ctx = FakeAuthContext::new(&[("GEMINI_API_KEY", "gemini-key")]);
    let result = api_key
        .resolve(&ctx, None)
        .await
        .expect("resolve")
        .expect("configured");
    assert_eq!(result.auth.api_key.as_deref(), Some("gemini-key"));
    assert_eq!(result.source.as_deref(), Some("GEMINI_API_KEY"));
}

// ---------------------------------------------------------------------------
// google-vertex
// ---------------------------------------------------------------------------

#[test]
fn test_google_vertex_factory_config() {
    let provider = google_vertex_provider();
    assert_factory_basics(
        &provider,
        "google-vertex",
        "Google Vertex AI",
        None,
        "google-vertex",
    );
    assert!(provider.auth().oauth.is_none());
    assert_eq!(
        provider
            .auth()
            .api_key
            .as_ref()
            .expect("api key auth")
            .name(),
        "Google Cloud credentials"
    );
}

/// Upstream: "runs provider-owned Vertex API key and ADC login flows".
#[tokio::test]
async fn test_google_vertex_login_flows() {
    let provider = google_vertex_provider();
    let auth = provider.auth().api_key.clone().expect("api key auth");

    let interaction = RecordingInteraction::new(&["api-key", "vertex-key"]);
    let credential = auth.login(&interaction).await.expect("login");
    assert_eq!(credential.key.as_deref(), Some("vertex-key"));
    assert_eq!(credential.env, None);

    let interaction = RecordingInteraction::new(&["adc", "project-id", "us-central1"]);
    let credential = auth.login(&interaction).await.expect("login");
    assert_eq!(credential.key, None);
    let env = credential.env.expect("env");
    assert_eq!(
        env.get("GOOGLE_CLOUD_PROJECT").map(String::as_str),
        Some("project-id")
    );
    assert_eq!(
        env.get("GOOGLE_CLOUD_LOCATION").map(String::as_str),
        Some("us-central1")
    );
    assert!(!env.contains_key("GOOGLE_APPLICATION_CREDENTIALS"));
    let events = interaction.events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        AuthEvent::Info { links, .. } => {
            let links = links.as_ref().expect("links");
            assert!(
                links.iter().any(|link| {
                    link.label.as_deref() == Some("Application Default Credentials")
                })
            );
        }
        other => panic!("expected info event, got {other:?}"),
    }

    // Resolve with the stored ADC credential plus the well-known ADC file.
    let ctx = FakeAuthContext::with_files(
        &[],
        &["~/.config/gcloud/application_default_credentials.json"],
    );
    let credential = rpi_ai::auth::ApiKeyCredential {
        key: None,
        env: Some(env),
    };
    let result = auth
        .resolve(&ctx, Some(&credential))
        .await
        .expect("resolve")
        .expect("configured");
    assert_eq!(result.auth.api_key, None);
    assert_eq!(result.auth.headers, None);
    assert_eq!(result.source.as_deref(), Some("stored credential"));
    let result_env = result.env.expect("env passthrough");
    assert_eq!(
        result_env.get("GOOGLE_CLOUD_PROJECT").map(String::as_str),
        Some("project-id")
    );
}

/// Upstream: "resolves vertex via ADC file plus project and location"
/// (including the partial-config and explicit-key-wins branches).
#[tokio::test]
async fn test_google_vertex_resolves_adc_with_project_and_location() {
    let adc = "~/.config/gcloud/application_default_credentials.json";

    let configured = models_with_context(FakeAuthContext::with_files(
        &[
            ("GOOGLE_CLOUD_PROJECT", "proj"),
            ("GOOGLE_CLOUD_LOCATION", "us-central1"),
        ],
        &[adc],
    ));
    configured.set_provider(google_vertex_provider());
    let model = configured.get_models(Some("google-vertex"))[0].clone();
    let result = configured
        .get_auth(&model, None)
        .await
        .expect("get auth")
        .expect("configured");
    assert_eq!(result.auth.api_key, None);
    assert_eq!(result.auth.headers, None);
    assert!(
        result
            .source
            .as_deref()
            .is_some_and(|source| source.contains("application default")),
        "unexpected source {:?}",
        result.source
    );

    // ADC without location is not configured.
    let partial = models_with_context(FakeAuthContext::with_files(
        &[("GOOGLE_CLOUD_PROJECT", "proj")],
        &[adc],
    ));
    partial.set_provider(google_vertex_provider());
    assert!(
        partial
            .get_auth(&model, None)
            .await
            .expect("get auth")
            .is_none()
    );

    // Explicit key wins over ADC.
    let keyed = models_with_context(FakeAuthContext::new(&[(
        "GOOGLE_CLOUD_API_KEY",
        "vertex-key",
    )]));
    keyed.set_provider(google_vertex_provider());
    let result = keyed
        .get_auth(&model, None)
        .await
        .expect("get auth")
        .expect("configured");
    assert_eq!(result.auth.api_key.as_deref(), Some("vertex-key"));
}

// ---------------------------------------------------------------------------
// amazon-bedrock
// ---------------------------------------------------------------------------

#[test]
fn test_amazon_bedrock_factory_config() {
    let provider = amazon_bedrock_provider();
    assert_factory_basics(
        &provider,
        "amazon-bedrock",
        "Amazon Bedrock",
        None,
        "bedrock-converse-stream",
    );
    assert!(provider.auth().oauth.is_none());
    assert_eq!(
        provider
            .auth()
            .api_key
            .as_ref()
            .expect("api key auth")
            .name(),
        "AWS credentials or bearer token"
    );
}

/// Upstream: "runs provider-owned Bedrock bearer token and AWS profile login
/// flows".
#[tokio::test]
async fn test_amazon_bedrock_login_flows() {
    let provider = amazon_bedrock_provider();
    let auth = provider.auth().api_key.clone().expect("api key auth");

    let interaction = RecordingInteraction::new(&["bearer-token", "bedrock-token"]);
    let credential = auth.login(&interaction).await.expect("login");
    assert_eq!(credential.key.as_deref(), Some("bedrock-token"));
    assert_eq!(credential.env, None);

    let interaction = RecordingInteraction::new(&["aws-profile", "work"]);
    let credential = auth.login(&interaction).await.expect("login");
    assert_eq!(credential.key, None);
    let env = credential.env.expect("env");
    assert_eq!(env.get("AWS_PROFILE").map(String::as_str), Some("work"));
    let events = interaction.events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        AuthEvent::Info { links, .. } => {
            let links = links.as_ref().expect("links");
            assert!(links.iter().any(|link: &AuthInfoLink| link.label.as_deref()
                == Some("AWS credential provider chain")));
        }
        other => panic!("expected info event, got {other:?}"),
    }

    // Resolve with the stored profile credential.
    let ctx = FakeAuthContext::new(&[]);
    let credential = rpi_ai::auth::ApiKeyCredential {
        key: None,
        env: Some(env),
    };
    let result = auth
        .resolve(&ctx, Some(&credential))
        .await
        .expect("resolve")
        .expect("configured");
    assert_eq!(result.auth.api_key, None);
    assert_eq!(result.source.as_deref(), Some("stored credential"));
    assert_eq!(
        result
            .env
            .as_ref()
            .and_then(|env| env.get("AWS_PROFILE"))
            .map(String::as_str),
        Some("work")
    );
}

/// Upstream: "reports bedrock as configured from ambient AWS credentials
/// without an api key".
#[tokio::test]
async fn test_amazon_bedrock_ambient_credentials() {
    let configured = models_with_context(FakeAuthContext::new(&[("AWS_PROFILE", "dev")]));
    configured.set_provider(amazon_bedrock_provider());
    let model = configured.get_models(Some("amazon-bedrock"))[0].clone();
    let result = configured
        .get_auth(&model, None)
        .await
        .expect("get auth")
        .expect("configured");
    assert_eq!(result.auth.api_key, None);
    assert_eq!(result.auth.headers, None);
    assert_eq!(result.source.as_deref(), Some("AWS_PROFILE"));

    let unconfigured = models_with_context(FakeAuthContext::new(&[]));
    unconfigured.set_provider(amazon_bedrock_provider());
    assert!(
        unconfigured
            .get_auth(&model, None)
            .await
            .expect("get auth")
            .is_none()
    );
}

// ---------------------------------------------------------------------------
// mistral
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_mistral_factory_config_and_auth() {
    let provider = mistral_provider();
    assert_factory_basics(
        &provider,
        "mistral",
        "Mistral",
        Some("https://api.mistral.ai"),
        "mistral-conversations",
    );

    let auth = provider.auth();
    assert!(auth.oauth.is_none());
    let api_key = auth.api_key.as_ref().expect("api key auth");
    assert_eq!(api_key.name(), "Mistral API key");

    let ctx = FakeAuthContext::new(&[("MISTRAL_API_KEY", "mistral-key")]);
    let result = api_key
        .resolve(&ctx, None)
        .await
        .expect("resolve")
        .expect("configured");
    assert_eq!(result.auth.api_key.as_deref(), Some("mistral-key"));
    assert_eq!(result.source.as_deref(), Some("MISTRAL_API_KEY"));
}
