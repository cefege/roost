//! Live smoke tests against real provider endpoints (T03 self-check list;
//! T13 W7 adds mistral / google-generative-ai / azure-openai-responses /
//! bedrock).
//!
//! Gated (coding-standards §12.6): every test returns immediately unless
//! `RPI_LIVE_TEST=1` is set **and** the provider's API key env var is present.
//! Model ids can be overridden via `RPI_LIVE_<PROVIDER>_MODEL`; base URLs via
//! `RPI_LIVE_<PROVIDER>_BASE_URL`. openai-codex (OAuth sign-in state) and
//! pi-messages (internal endpoint) intentionally have no live target.

use futures::StreamExt;
use rpi_ai::api::anthropic_messages::AnthropicMessages;
use rpi_ai::api::azure_openai_responses::AzureOpenAiResponses;
use rpi_ai::api::bedrock_converse_stream::BedrockConverseStream;
use rpi_ai::api::google_generative_ai::GoogleGenerativeAi;
use rpi_ai::api::mistral_conversations::MistralConversations;
use rpi_ai::api::openai_completions::OpenAiCompletions;
use rpi_ai::api::openai_responses::OpenAiResponses;
use rpi_ai::models::ProviderStreams;
use rpi_ai::types::{Context, DoneReason, Model, StreamEvent, StreamOptions};
use serde_json::json;

struct LiveTarget {
    key_env: &'static str,
    model_env: &'static str,
    base_url_env: &'static str,
    default_model: &'static str,
    default_base_url: &'static str,
    api: &'static str,
    provider: &'static str,
}

const ANTHROPIC: LiveTarget = LiveTarget {
    key_env: "ANTHROPIC_API_KEY",
    model_env: "RPI_LIVE_ANTHROPIC_MODEL",
    base_url_env: "RPI_LIVE_ANTHROPIC_BASE_URL",
    default_model: "claude-haiku-4-5",
    default_base_url: "https://api.anthropic.com",
    api: "anthropic-messages",
    provider: "anthropic",
};

const OPENAI_COMPLETIONS: LiveTarget = LiveTarget {
    key_env: "OPENAI_API_KEY",
    model_env: "RPI_LIVE_OPENAI_MODEL",
    base_url_env: "RPI_LIVE_OPENAI_BASE_URL",
    default_model: "gpt-4o-mini",
    default_base_url: "https://api.openai.com",
    api: "openai-completions",
    provider: "openai",
};

const OPENAI_RESPONSES: LiveTarget = LiveTarget {
    key_env: "OPENAI_API_KEY",
    model_env: "RPI_LIVE_OPENAI_RESPONSES_MODEL",
    base_url_env: "RPI_LIVE_OPENAI_RESPONSES_BASE_URL",
    default_model: "gpt-4o-mini",
    default_base_url: "https://api.openai.com",
    api: "openai-responses",
    provider: "openai",
};

const MISTRAL: LiveTarget = LiveTarget {
    key_env: "MISTRAL_API_KEY",
    model_env: "RPI_LIVE_MISTRAL_MODEL",
    base_url_env: "RPI_LIVE_MISTRAL_BASE_URL",
    default_model: "mistral-small-latest",
    default_base_url: "https://api.mistral.ai",
    api: "mistral-conversations",
    provider: "mistral",
};

const GOOGLE_GENERATIVE_AI: LiveTarget = LiveTarget {
    key_env: "GEMINI_API_KEY",
    model_env: "RPI_LIVE_GOOGLE_MODEL",
    base_url_env: "RPI_LIVE_GOOGLE_BASE_URL",
    default_model: "gemini-2.5-flash",
    default_base_url: "https://generativelanguage.googleapis.com",
    api: "google-generative-ai",
    provider: "google",
};

/// Meta Model API (`META_API_KEY`; the OAuth-minted key works the same
/// way — upstream's `stream.test.ts`/`tokens.test.ts`/
/// `cross-provider-handoff.test.ts` Meta additions are all gated on this
/// env, V15-15 port note in the task file §7).
const META: LiveTarget = LiveTarget {
    key_env: "META_API_KEY",
    model_env: "RPI_LIVE_META_MODEL",
    base_url_env: "RPI_LIVE_META_BASE_URL",
    default_model: "muse-spark-1.3",
    default_base_url: "https://api.meta.ai/v1",
    api: "openai-responses",
    provider: "meta",
};

/// Azure gate: `AZURE_OPENAI_API_KEY` plus `AZURE_OPENAI_RESOURCE_NAME` (the
/// base URL is derived from the resource name unless overridden).
fn gate_azure() -> Option<(Model, StreamOptions)> {
    if std::env::var("RPI_LIVE_TEST").ok().as_deref() != Some("1") {
        return None;
    }
    let api_key = std::env::var("AZURE_OPENAI_API_KEY").ok()?;
    let resource = std::env::var("AZURE_OPENAI_RESOURCE_NAME").unwrap_or_else(|_| String::new());
    let base_url = std::env::var("RPI_LIVE_AZURE_BASE_URL")
        .ok()
        .unwrap_or_else(|| {
            if resource.is_empty() {
                return String::new();
            }
            format!("https://{resource}.openai.azure.com/openai/v1")
        });
    if base_url.is_empty() {
        return None;
    }
    let model_id =
        std::env::var("RPI_LIVE_AZURE_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_owned());
    Some(model_and_options(
        &model_id, &base_url, "azure", "azure", api_key,
    ))
}

/// Bedrock gate: AWS ambient credentials (`AWS_ACCESS_KEY_ID` +
/// `AWS_SECRET_ACCESS_KEY`); the endpoint region defaults to `AWS_REGION`
/// then `us-east-1` and can be overridden via `RPI_LIVE_BEDROCK_BASE_URL`.
fn gate_bedrock() -> Option<(Model, StreamOptions)> {
    if std::env::var("RPI_LIVE_TEST").ok().as_deref() != Some("1") {
        return None;
    }
    std::env::var("AWS_ACCESS_KEY_ID").ok()?;
    std::env::var("AWS_SECRET_ACCESS_KEY").ok()?;
    let base_url = std::env::var("RPI_LIVE_BEDROCK_BASE_URL")
        .ok()
        .unwrap_or_else(|| {
            let region = std::env::var("AWS_REGION")
                .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
                .unwrap_or_else(|_| "us-east-1".to_owned());
            format!("https://bedrock-runtime.{region}.amazonaws.com")
        });
    let model_id = std::env::var("RPI_LIVE_BEDROCK_MODEL")
        .unwrap_or_else(|_| "anthropic.claude-3-5-haiku-20241022-v1:0".to_owned());
    Some(model_and_options(
        &model_id,
        &base_url,
        "bedrock-converse-stream",
        "amazon-bedrock",
        String::new(),
    ))
}

fn model_and_options(
    model_id: &str,
    base_url: &str,
    api: &str,
    provider: &str,
    api_key: String,
) -> (Model, StreamOptions) {
    let model: Model = serde_json::from_value(json!({
        "id": model_id, "name": model_id, "api": api, "provider": provider,
        "baseUrl": base_url, "reasoning": false, "input": ["text"],
        "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0},
        "contextWindow": 128000, "maxTokens": 64
    }))
    .expect("model");
    let options = StreamOptions {
        max_tokens: Some(64),
        request: rpi_ai::ProviderRequestOptions {
            api_key: if api_key.is_empty() {
                None
            } else {
                Some(api_key)
            },
            ..Default::default()
        },
        ..StreamOptions::default()
    };
    (model, options)
}

/// Returns `Some((model, options))` when the gate is open, `None` to skip.
fn gate(target: &LiveTarget) -> Option<(Model, StreamOptions)> {
    if std::env::var("RPI_LIVE_TEST").ok().as_deref() != Some("1") {
        return None;
    }
    let api_key = std::env::var(target.key_env).ok()?;
    let model_id =
        std::env::var(target.model_env).unwrap_or_else(|_| target.default_model.to_owned());
    let base_url =
        std::env::var(target.base_url_env).unwrap_or_else(|_| target.default_base_url.to_owned());
    Some(model_and_options(
        &model_id,
        &base_url,
        target.api,
        target.provider,
        api_key,
    ))
}

fn context() -> rpi_ai::types::TranscriptContext {
    rpi_ai::utils::transcript::normalize_context(&Context {
        system_prompt: None,
        messages: vec![serde_json::from_value(
            json!({"role": "user", "content": "Reply with the single word: ok", "timestamp": 0}),
        )
        .expect("user")],
        tools: None,
    })
}

async fn smoke(events: Vec<StreamEvent>, label: &str) {
    let Some(StreamEvent::Done { reason, message }) = events.last() else {
        panic!("{label}: expected a terminal done event, got {events:?}");
    };
    assert_eq!(
        *reason,
        DoneReason::Stop,
        "{label}: {:?}",
        message.error_message
    );
    assert!(
        !message.content.is_empty(),
        "{label}: expected non-empty assistant content"
    );
    assert!(message.usage.output > 0, "{label}: expected output usage");
}

#[tokio::test]
async fn test_live_anthropic_messages() {
    let Some((model, options)) = gate(&ANTHROPIC) else {
        return;
    };
    let events = AnthropicMessages
        .stream(&model, &context(), Some(options))
        .collect()
        .await;
    smoke(events, "anthropic-messages").await;
}

#[tokio::test]
async fn test_live_openai_completions() {
    let Some((model, options)) = gate(&OPENAI_COMPLETIONS) else {
        return;
    };
    let events = OpenAiCompletions
        .stream(&model, &context(), Some(options))
        .collect()
        .await;
    smoke(events, "openai-completions").await;
}

#[tokio::test]
async fn test_live_openai_responses() {
    let Some((model, options)) = gate(&OPENAI_RESPONSES) else {
        return;
    };
    let events = OpenAiResponses
        .stream(&model, &context(), Some(options))
        .collect()
        .await;
    smoke(events, "openai-responses").await;
}

#[tokio::test]
async fn test_live_mistral_conversations() {
    let Some((model, options)) = gate(&MISTRAL) else {
        return;
    };
    let events = MistralConversations
        .stream(&model, &context(), Some(options))
        .collect()
        .await;
    smoke(events, "mistral-conversations").await;
}

#[tokio::test]
async fn test_live_google_generative_ai() {
    let Some((model, options)) = gate(&GOOGLE_GENERATIVE_AI) else {
        return;
    };
    let events = GoogleGenerativeAi
        .stream(&model, &context(), Some(options))
        .collect()
        .await;
    smoke(events, "google-generative-ai").await;
}

#[tokio::test]
async fn test_live_meta_openai_responses() {
    let Some((model, options)) = gate(&META) else {
        return;
    };
    let events = OpenAiResponses
        .stream(&model, &context(), Some(options))
        .collect()
        .await;
    smoke(events, "meta").await;
}

#[tokio::test]
async fn test_live_azure_openai_responses() {
    let Some((model, options)) = gate_azure() else {
        return;
    };
    let events = AzureOpenAiResponses
        .stream(&model, &context(), Some(options))
        .collect()
        .await;
    smoke(events, "azure").await;
}

#[tokio::test]
async fn test_live_bedrock_converse_stream() {
    let Some((model, options)) = gate_bedrock() else {
        return;
    };
    let events = BedrockConverseStream
        .stream(&model, &context(), Some(options))
        .collect()
        .await;
    smoke(events, "bedrock-converse-stream").await;
}
