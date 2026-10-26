//! Golden tests for the per-request fetch injection channel (R2.7.4), porting
//! the intent of upstream `packages/ai/test/fetch-option.test.ts`
//! (`027a58479` @ 4181f66).
//!
//! Mapping notes:
//! - Upstream stubs `globalThis.fetch` as the "must not be called" fallback.
//!   rpi's default transport is reqwest, so the fallback assertion is
//!   structural: models point at an unroutable loopback address and the
//!   surfaced error message proves the custom fetch served the request (a
//!   reqwest attempt would fail with a connect error instead).
//! - "allows Google adapters to receive globalThis.fetch explicitly" has no
//!   direct analogue — rpi has no ambient global fetch. The default transport
//!   (`fetch: None`) is the `globalThis.fetch` case; it is asserted for
//!   Google in `google_default_transport_is_not_rejected` and covered by the
//!   existing Google contract tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::StreamExt;
use rpi_ai::types::{
    Context, FetchError, FetchFn, FetchRequest, FetchResponse, Model, SimpleStreamOptions,
    StreamEvent, StreamOptions, Transport,
};
use rpi_ai::utils::event_stream::AssistantMessageEventStream;
use serde_json::{Value, json};

/// Unroutable loopback address: any reqwest (default-transport) attempt fails
/// fast with a connect error, so reaching the canned body proves the custom
/// fetch channel was used.
const UNROUTABLE: &str = "http://127.0.0.1:1";

/// Upstream canned response: 401 with a JSON error body.
const CANNED_BODY: &[u8] = br#"{"error":{"message":"upstream rejected request"}}"#;

/// Call-counting probe, the `vi.fn` analogue.
#[derive(Clone, Default)]
struct FetchProbe {
    calls: Arc<AtomicU64>,
}

impl FetchProbe {
    fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}

/// `mockFetches().custom`: records the call and returns the canned 401.
fn canned_fetch(probe: &FetchProbe) -> FetchFn {
    let calls = probe.calls.clone();
    Arc::new(move |_request: FetchRequest| {
        calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(FetchResponse {
                status: 401,
                headers: vec![("content-type".to_owned(), "application/json".to_owned())],
                body: Box::pin(futures::stream::once(async move {
                    Ok::<Vec<u8>, FetchError>(CANNED_BODY.to_vec())
                })),
            })
        })
    })
}

fn model(api: &str, provider: &str, base_url: &str, extra: Value) -> Model {
    let mut value = json!({
        "id": "test-model", "name": "Test", "api": api, "provider": provider,
        "baseUrl": base_url, "reasoning": false, "input": ["text"],
        "cost": {"input": 1.0, "output": 2.0, "cacheRead": 0.5, "cacheWrite": 1.0},
        "contextWindow": 128000, "maxTokens": 8192
    });
    value
        .as_object_mut()
        .expect("object")
        .extend(extra.as_object().cloned().unwrap_or_default());
    serde_json::from_value(value).expect("model")
}

fn context() -> Context {
    Context {
        system_prompt: None,
        messages: vec![
            serde_json::from_value(json!({"role": "user", "content": "hello", "timestamp": 1}))
                .expect("user"),
        ],
        tools: None,
    }
}

fn simple_options(fetch: FetchFn, api_key: &str) -> SimpleStreamOptions {
    SimpleStreamOptions {
        stream: StreamOptions {
            request: rpi_ai::ProviderRequestOptions {
                api_key: Some(api_key.to_owned()),
                fetch: Some(fetch),
                max_retries: Some(0),
                ..Default::default()
            },
            ..StreamOptions::default()
        },
        reasoning: None,
        thinking_budgets: None,
        tool_choice: None,
    }
}

async fn collect(stream: AssistantMessageEventStream) -> Vec<StreamEvent> {
    tokio::time::timeout(Duration::from_secs(10), stream.collect())
        .await
        .expect("stream completes within 10s")
}

/// The terminal error message (`stream.result().errorMessage` upstream).
fn terminal_error_message(events: &[StreamEvent]) -> &str {
    for event in events.iter().rev() {
        match event {
            StreamEvent::Error { error, .. } => {
                return error.error_message.as_deref().expect("error message");
            }
            StreamEvent::Done { message, .. } => {
                return message.error_message.as_deref().expect("error message");
            }
            _ => {}
        }
    }
    panic!("expected a terminal event, got {} events", events.len());
}

// ---------------------------------------------------------------------------
// fetch stream option (fetch-option.test.ts)
// ---------------------------------------------------------------------------

/// Upstream: "passes fetch through streamSimple to the Anthropic SDK".
#[tokio::test]
async fn passes_fetch_through_stream_simple_to_the_anthropic_sdk() {
    let probe = FetchProbe::default();
    let m = model("anthropic-messages", "test-provider", UNROUTABLE, json!({}));
    let events = collect(
        rpi_ai::api::anthropic_messages::stream_simple(
            &m,
            &rpi_ai::utils::transcript::normalize_context(&context()),
            Some(simple_options(canned_fetch(&probe), "test-key")),
        )
        .expect("stream_simple"),
    )
    .await;

    assert_eq!(probe.calls(), 1, "custom fetch called exactly once");
    assert!(
        terminal_error_message(&events).contains("upstream rejected request"),
        "canned body surfaces: {}",
        terminal_error_message(&events)
    );
}

/// Upstream: "passes fetch through streamSimple to OpenAI SDK adapters".
#[tokio::test]
async fn passes_fetch_through_stream_simple_to_openai_sdk_adapters() {
    let probe = FetchProbe::default();

    let completions = model("openai-completions", "test-provider", UNROUTABLE, json!({}));
    let events = collect(
        rpi_ai::api::openai_completions::stream_simple(
            &completions,
            &rpi_ai::utils::transcript::normalize_context(&context()),
            Some(simple_options(canned_fetch(&probe), "test-key")),
        )
        .expect("stream_simple"),
    )
    .await;
    assert!(terminal_error_message(&events).contains("upstream rejected request"));

    let responses = model("openai-responses", "test-provider", UNROUTABLE, json!({}));
    let events = collect(
        rpi_ai::api::openai_responses::stream_simple(
            &responses,
            &rpi_ai::utils::transcript::normalize_context(&context()),
            Some(simple_options(canned_fetch(&probe), "test-key")),
        )
        .expect("stream_simple"),
    )
    .await;
    assert!(terminal_error_message(&events).contains("upstream rejected request"));

    let azure = model("azure", "azure", UNROUTABLE, json!({}));
    let events = collect(
        rpi_ai::api::azure_openai_responses::stream_simple(
            &azure,
            &rpi_ai::utils::transcript::normalize_context(&context()),
            Some(simple_options(canned_fetch(&probe), "test-key")),
        )
        .expect("stream_simple"),
    )
    .await;
    assert!(terminal_error_message(&events).contains("upstream rejected request"));

    assert_eq!(probe.calls(), 3, "one custom fetch call per adapter");
}

/// Upstream: "uses fetch for Mistral, Codex SSE, and pi-messages HTTP
/// requests".
#[tokio::test]
async fn uses_fetch_for_mistral_codex_sse_and_pi_messages() {
    let probe = FetchProbe::default();

    let mistral = model(
        "mistral-conversations",
        "test-provider",
        UNROUTABLE,
        json!({}),
    );
    let events = collect(
        rpi_ai::api::mistral_conversations::stream_simple(
            &mistral,
            &rpi_ai::utils::transcript::normalize_context(&context()),
            Some(simple_options(canned_fetch(&probe), "test-key")),
        )
        .expect("stream_simple"),
    )
    .await;
    assert!(terminal_error_message(&events).contains("upstream rejected request"));

    // Codex over the SSE transport with a JWT-shaped key (account id claim).
    let codex = model(
        "openai-codex-responses",
        "openai-codex",
        UNROUTABLE,
        json!({"reasoning": true}),
    );
    let mut options = simple_options(canned_fetch(&probe), &mock_token());
    options.stream.transport = Some(Transport::Sse);
    let events = collect(
        rpi_ai::api::openai_codex_responses::stream_simple(
            &codex,
            &rpi_ai::utils::transcript::normalize_context(&context()),
            Some(options),
        )
        .expect("stream_simple"),
    )
    .await;
    assert!(
        terminal_error_message(&events).contains("upstream rejected request"),
        "canned body surfaces: {}",
        terminal_error_message(&events)
    );

    let pi = model("pi-messages", "test-provider", UNROUTABLE, json!({}));
    let events = collect(
        rpi_ai::api::pi_messages::stream_simple(
            &pi,
            &rpi_ai::utils::transcript::normalize_context(&context()),
            Some(simple_options(canned_fetch(&probe), "test-key")),
        )
        .expect("stream_simple"),
    )
    .await;
    assert!(terminal_error_message(&events).contains("upstream rejected request"));

    assert_eq!(probe.calls(), 3, "one custom fetch call per adapter");
}

/// Upstream: "rejects custom fetch for Google adapters instead of silently
/// bypassing it".
#[tokio::test]
async fn rejects_custom_fetch_for_google_adapters() {
    let probe = FetchProbe::default();

    let google = model(
        "google-generative-ai",
        "test-provider",
        UNROUTABLE,
        json!({}),
    );
    let events = collect(
        rpi_ai::api::google_generative_ai::stream_simple(
            &google,
            &rpi_ai::utils::transcript::normalize_context(&context()),
            Some(simple_options(canned_fetch(&probe), "test-key")),
        )
        .expect("stream_simple"),
    )
    .await;
    assert_eq!(
        terminal_error_message(&events),
        "Custom fetch is not supported by the Google Generative AI adapter"
    );

    let vertex = model("google-vertex", "test-provider", UNROUTABLE, json!({}));
    let events = collect(
        rpi_ai::api::google_vertex::stream_simple(
            &vertex,
            &rpi_ai::utils::transcript::normalize_context(&context()),
            Some(simple_options(canned_fetch(&probe), "test-key")),
        )
        .expect("stream_simple"),
    )
    .await;
    assert_eq!(
        terminal_error_message(&events),
        "Custom fetch is not supported by the Google Vertex adapter"
    );

    assert_eq!(probe.calls(), 0, "rejection happens before any request");
}

/// Upstream: "allows Google adapters to receive globalThis.fetch explicitly".
/// rpi analogue: the default transport (`fetch: None`) is the global-fetch
/// case and must not trip the rejection branch.
#[tokio::test]
async fn google_default_transport_is_not_rejected() {
    let google = model(
        "google-generative-ai",
        "test-provider",
        UNROUTABLE,
        json!({}),
    );
    let mut options = simple_options(canned_fetch(&FetchProbe::default()), "test-key");
    options.stream.request.fetch = None;
    let events = collect(
        rpi_ai::api::google_generative_ai::stream_simple(
            &google,
            &rpi_ai::utils::transcript::normalize_context(&context()),
            Some(options),
        )
        .expect("stream_simple"),
    )
    .await;
    assert!(
        !terminal_error_message(&events).contains("Custom fetch is not supported"),
        "default transport must not be rejected: {}",
        terminal_error_message(&events)
    );
}

/// Upstream: "uses fetch for image generation".
#[tokio::test]
async fn uses_fetch_for_image_generation() {
    use rpi_ai::types::{
        ImagesApiKind, ImagesContext, ImagesInputContent, ImagesModel, ImagesOptions,
        ImagesOutputModality, ImagesStopReason, InputModality, ModelCost, ModelCostRates,
        TextContent,
    };

    let probe = FetchProbe::default();
    let model = ImagesModel {
        id: "test-image-model".to_owned(),
        name: "Test".to_owned(),
        api: ImagesApiKind(ImagesApiKind::OPENROUTER_IMAGES.to_owned()),
        provider: "openrouter".to_owned(),
        base_url: UNROUTABLE.to_owned(),
        input: vec![InputModality::Text],
        output: vec![ImagesOutputModality::Image],
        cost: ModelCost {
            rates: ModelCostRates {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            tiers: None,
        },
        headers: None,
    };
    let context = ImagesContext {
        input: vec![ImagesInputContent::Text(TextContent {
            text: "draw".to_owned(),
            text_signature: None,
        })],
    };

    // `generateImages` never rejects; the error lands on the output.
    let output = rpi_ai::api::openrouter_images::generate_images(
        &model,
        &context,
        Some(&ImagesOptions {
            api_key: Some("test-key".to_owned()),
            fetch: Some(canned_fetch(&probe)),
            max_retries: Some(0),
            ..Default::default()
        }),
    )
    .await;

    assert_eq!(probe.calls(), 1, "custom fetch called exactly once");
    assert_eq!(output.stop_reason, ImagesStopReason::Error);
    let message = output.error_message.as_deref().expect("error message");
    assert!(
        message.contains("upstream rejected request"),
        "canned body surfaces: {message}"
    );
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn mock_token() -> String {
    use base64::Engine;
    let payload = base64::engine::general_purpose::STANDARD.encode(
        serde_json::to_string(&json!({
            "https://api.openai.com/auth": {"chatgpt_account_id": "account"}
        }))
        .expect("json"),
    );
    format!("aaa.{payload}.bbb")
}
