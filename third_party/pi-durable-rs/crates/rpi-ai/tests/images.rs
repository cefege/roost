//! Contract tests for the image-generation subsystem, porting the intent of
//! `packages/ai/test/openrouter-images.test.ts` and the collection/catalog
//! halves of `packages/ai/test/images-models.test.ts` @ pi 0.82.1 (2efa728):
//! drive `generateImages` over a scripted loopback HTTP server and assert
//! both sides of the contract — the request shape (method / path / headers /
//! body JSON: non-streaming, `modalities`) and the returned `AssistantImages`
//! (text + `data:`-URL images, usage, **never rejects** — errors surface as
//! normal results with `stopReason: "error"`/`"aborted"`). No real network.
//!
//! Test names mirror the upstream vitest cases (snake_case). The upstream
//! `images.test.ts` E2E cases are live-API tests (skipped without
//! `OPENROUTER_API_KEY` upstream); their assertions live here as
//! stop-reason/timestamp/output checks against the loopback server.

use std::sync::Arc;

use rpi_ai::auth::AuthContext;
use rpi_ai::images::{builtin_images_models, generate_images, get_image_model};
use rpi_ai::types::{
    AssistantImages, ImagesApiKind, ImagesContext, ImagesInputContent, ImagesModel, ImagesOptions,
    ImagesOutputContent, ImagesOutputModality, ImagesStopReason, InputModality, ModelCost,
    ModelCostRates, ProviderResponse, TextContent,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Scripted HTTP server (same approach as contract_pi_messages.rs)
// ---------------------------------------------------------------------------

/// One captured request: request line, headers (lowercased names) and body.
#[derive(Debug)]
struct CapturedRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl CapturedRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn body_json(&self) -> Value {
        serde_json::from_str(&self.body).expect("request body is JSON")
    }
}

/// One scripted response: status, extra headers, body.
struct ScriptResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl ScriptResponse {
    fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: serde_json::to_string(&body).expect("json"),
        }
    }

    fn raw(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }
}

/// Serves the scripted responses, one per connection, on a loopback port.
/// Returns the base URL and a channel of captured requests.
async fn serve(script: Vec<ScriptResponse>) -> (String, mpsc::Receiver<CapturedRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel(script.len().max(1));
    tokio::spawn(async move {
        for response_script in script {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let request = read_request(&mut socket).await;
            tx.send(request).await.expect("send captured request");
            let reason = match response_script.status {
                200 => "OK",
                401 => "Unauthorized",
                500 => "Internal Server Error",
                _ => "Status",
            };
            let mut response = format!(
                "HTTP/1.1 {} {reason}\r\ncontent-type: application/json\r\n",
                response_script.status
            );
            for (name, value) in &response_script.headers {
                response.push_str(&format!("{name}: {value}\r\n"));
            }
            response.push_str(&format!(
                "content-length: {}\r\nconnection: close\r\n\r\n{}",
                response_script.body.len(),
                response_script.body
            ));
            socket
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        }
    });
    (format!("http://{addr}"), rx)
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> CapturedRequest {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = socket.read(&mut chunk).await.expect("read");
        assert!(n > 0, "connection closed while reading request");
        buffer.extend_from_slice(&chunk[..n]);
        if let Some(position) = find_subslice(&buffer, b"\r\n\r\n") {
            break position + 4;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next().expect("request line").to_owned();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().expect("method").to_owned();
    let path = parts.next().expect("path").to_owned();
    let headers = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_lowercase(), value.trim().to_owned()))
        })
        .collect();
    let body = String::from_utf8_lossy(&buffer[header_end..]).to_string();
    CapturedRequest {
        method,
        path,
        headers,
        body,
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

// ---------------------------------------------------------------------------
// Test fixtures
// ---------------------------------------------------------------------------

/// A chat-completions-style image response (the upstream test mock shape).
fn image_response_body() -> Value {
    json!({
        "id": "img-1",
        "usage": {
            "prompt_tokens": 12,
            "completion_tokens": 34,
            "prompt_tokens_details": { "cached_tokens": 0 },
        },
        "choices": [
            {
                "message": {
                    "content": "Here is your image.",
                    "images": [{ "image_url": "data:image/png;base64,ZmFrZS1wbmc=" }],
                }
            }
        ],
    })
}

fn test_model(base_url: &str, output: Vec<ImagesOutputModality>) -> ImagesModel {
    ImagesModel {
        id: "google/gemini-3.1-flash-image-preview".to_owned(),
        name: "Gemini 3.1 Flash Image Preview".to_owned(),
        api: ImagesApiKind(ImagesApiKind::OPENROUTER_IMAGES.to_owned()),
        provider: "openrouter".to_owned(),
        base_url: base_url.to_owned(),
        input: vec![InputModality::Text, InputModality::Image],
        output,
        cost: ModelCost {
            rates: ModelCostRates {
                input: 0.015,
                output: 0.03,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            tiers: None,
        },
        headers: None,
    }
}

fn text_context(text: &str) -> ImagesContext {
    ImagesContext {
        input: vec![ImagesInputContent::Text(TextContent {
            text: text.to_owned(),
            text_signature: None,
        })],
    }
}

fn assert_image_content(item: &ImagesOutputContent, mime_type: &str, data: &str) {
    match item {
        ImagesOutputContent::Image(image) => {
            assert_eq!(image.mime_type, mime_type);
            assert_eq!(image.data, data);
        }
        other => panic!("expected image content, got {other:?}"),
    }
}

/// `fakeAuthContext` (images-models.test.ts).
struct FakeAuthContext {
    env: std::collections::HashMap<String, String>,
}

#[async_trait::async_trait]
impl AuthContext for FakeAuthContext {
    async fn env(&self, name: &str) -> Option<String> {
        self.env.get(name).cloned()
    }

    async fn file_exists(&self, path: &str) -> bool {
        let _ = path;
        false
    }
}

// ---------------------------------------------------------------------------
// OpenRouter images contract (openrouter-images.test.ts intent)
// ---------------------------------------------------------------------------

/// "returns text plus images in final output" — request shape and result.
#[tokio::test]
async fn returns_text_plus_images_in_final_output() {
    let (base_url, mut requests) =
        serve(vec![ScriptResponse::json(200, image_response_body())]).await;
    let mut model = test_model(
        &base_url,
        vec![ImagesOutputModality::Image, ImagesOutputModality::Text],
    );
    model.headers = Some(
        [("HTTP-Referer".to_owned(), "https://example.com".to_owned())]
            .into_iter()
            .collect(),
    );

    let output = generate_images(
        &model,
        &text_context("Generate a dog"),
        Some(&ImagesOptions {
            api_key: Some("test".to_owned()),
            ..Default::default()
        }),
    )
    .await
    .expect("dispatch");

    assert_eq!(output.stop_reason, ImagesStopReason::Stop);
    assert!(output.error_message.is_none());
    assert_eq!(output.response_id.as_deref(), Some("img-1"));
    assert!(output.timestamp > 0);
    match &output.output[0] {
        ImagesOutputContent::Text(text) => assert_eq!(text.text, "Here is your image."),
        other => panic!("expected text content, got {other:?}"),
    }
    assert_image_content(&output.output[1], "image/png", "ZmFrZS1wbmc=");
    let usage = output.usage.as_ref().expect("usage");
    assert_eq!(usage.input, 12);
    assert_eq!(usage.output, 34);
    assert_eq!(usage.total_tokens, 46);
    // 0.015/1e6 * 12 + 0.03/1e6 * 34
    let expected = 0.015 / 1_000_000.0 * 12.0 + 0.03 / 1_000_000.0 * 34.0;
    assert!((usage.cost.total - expected).abs() < f64::EPSILON);

    let request = requests.recv().await.expect("request");
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/chat/completions");
    assert_eq!(request.header("authorization"), Some("Bearer test"));
    assert_eq!(request.header("content-type"), Some("application/json"));
    assert_eq!(request.header("http-referer"), Some("https://example.com"));
    let body = request.body_json();
    assert_eq!(body["stream"], json!(false));
    assert_eq!(body["modalities"], json!(["image", "text"]));
    assert_eq!(
        body["model"],
        json!("google/gemini-3.1-flash-image-preview")
    );
    assert_eq!(body["messages"][0]["role"], json!("user"));
    assert_eq!(
        body["messages"][0]["content"][0],
        json!({ "type": "text", "text": "Generate a dog" })
    );
}

/// Image-only models request `modalities: ["image"]`.
#[tokio::test]
async fn image_only_model_requests_image_modality() {
    let (base_url, mut requests) =
        serve(vec![ScriptResponse::json(200, image_response_body())]).await;
    let model = test_model(&base_url, vec![ImagesOutputModality::Image]);

    generate_images(
        &model,
        &text_context("Generate a dog"),
        Some(&ImagesOptions {
            api_key: Some("test".to_owned()),
            ..Default::default()
        }),
    )
    .await
    .expect("dispatch");

    let request = requests.recv().await.expect("request");
    assert_eq!(request.body_json()["modalities"], json!(["image"]));
}

/// Image inputs are sent as `data:` URLs.
#[tokio::test]
async fn image_input_is_sent_as_data_url() {
    let (base_url, mut requests) =
        serve(vec![ScriptResponse::json(200, image_response_body())]).await;
    let model = test_model(&base_url, vec![ImagesOutputModality::Image]);
    let context = ImagesContext {
        input: vec![
            ImagesInputContent::Text(TextContent {
                text: "Create a variation of this image with a blue background.".to_owned(),
                text_signature: None,
            }),
            ImagesInputContent::Image(rpi_ai::types::ImageContent {
                data: "aGVsbG8=".to_owned(),
                mime_type: "image/png".to_owned(),
            }),
        ],
    };

    generate_images(
        &model,
        &context,
        Some(&ImagesOptions {
            api_key: Some("test".to_owned()),
            ..Default::default()
        }),
    )
    .await
    .expect("dispatch");

    let request = requests.recv().await.expect("request");
    let content = &request.body_json()["messages"][0]["content"];
    assert_eq!(content[0]["type"], json!("text"));
    assert_eq!(
        content[1],
        json!({
            "type": "image_url",
            "image_url": { "url": "data:image/png;base64,aGVsbG8=" }
        })
    );
}

/// "passes through abort signal and returns aborted result".
#[tokio::test]
async fn aborted_signal_returns_aborted_result() {
    let (base_url, _requests) = serve(vec![ScriptResponse::json(200, image_response_body())]).await;
    let model = test_model(&base_url, vec![ImagesOutputModality::Image]);
    let signal = CancellationToken::new();
    signal.cancel();

    let output = generate_images(
        &model,
        &text_context("Generate a dog"),
        Some(&ImagesOptions {
            api_key: Some("test".to_owned()),
            signal: Some(signal),
            ..Default::default()
        }),
    )
    .await
    .expect("dispatch");

    assert_eq!(output.stop_reason, ImagesStopReason::Aborted);
    assert_eq!(output.error_message.as_deref(), Some("Request aborted"));
}

/// Missing api key is an error result, not a rejection.
#[tokio::test]
async fn missing_api_key_is_an_error_result() {
    let (base_url, _requests) = serve(vec![ScriptResponse::json(200, image_response_body())]).await;
    let model = test_model(&base_url, vec![ImagesOutputModality::Image]);

    let output = generate_images(&model, &text_context("Generate a dog"), None)
        .await
        .expect("dispatch");

    assert_eq!(output.stop_reason, ImagesStopReason::Error);
    assert_eq!(
        output.error_message.as_deref(),
        Some("No API key for provider: openrouter")
    );
    assert!(output.output.is_empty());
}

/// HTTP errors are error results, not rejections.
#[tokio::test]
async fn http_error_is_an_error_result() {
    let (base_url, _requests) = serve(vec![ScriptResponse::raw(500, "boom")]).await;
    let model = test_model(&base_url, vec![ImagesOutputModality::Image]);

    let output = generate_images(
        &model,
        &text_context("Generate a dog"),
        Some(&ImagesOptions {
            api_key: Some("test".to_owned()),
            ..Default::default()
        }),
    )
    .await
    .expect("dispatch");

    assert_eq!(output.stop_reason, ImagesStopReason::Error);
    let message = output.error_message.as_deref().expect("error message");
    assert!(message.contains("500"), "message: {message}");
    assert!(message.contains("boom"), "message: {message}");
}

/// Malformed response bodies are error results, not rejections.
#[tokio::test]
async fn malformed_response_is_an_error_result() {
    let (base_url, _requests) = serve(vec![ScriptResponse::raw(200, "not json at all")]).await;
    let model = test_model(&base_url, vec![ImagesOutputModality::Image]);

    let output = generate_images(
        &model,
        &text_context("Generate a dog"),
        Some(&ImagesOptions {
            api_key: Some("test".to_owned()),
            ..Default::default()
        }),
    )
    .await
    .expect("dispatch");

    assert_eq!(output.stop_reason, ImagesStopReason::Error);
    assert!(output.error_message.is_some());
}

/// Non-`data:` image urls and non-string content are skipped, `data:` urls
/// behind `{ url }` objects are parsed.
#[tokio::test]
async fn skips_non_data_images_and_parses_url_objects() {
    let response = json!({
        "id": "img-2",
        "choices": [
            {
                "message": {
                    "content": ["ignored array content"],
                    "images": [
                        { "image_url": "https://example.com/remote.png" },
                        { "image_url": { "url": "data:image/jpeg;base64,YWJj" } },
                        { "image_url": { "url": "data:image/png;base64," } },
                    ],
                }
            }
        ],
    });
    let (base_url, _requests) = serve(vec![ScriptResponse::json(200, response)]).await;
    let model = test_model(
        &base_url,
        vec![ImagesOutputModality::Image, ImagesOutputModality::Text],
    );

    let output = generate_images(
        &model,
        &text_context("Generate a dog"),
        Some(&ImagesOptions {
            api_key: Some("test".to_owned()),
            ..Default::default()
        }),
    )
    .await
    .expect("dispatch");

    assert_eq!(output.output.len(), 1, "output: {:?}", output.output);
    assert_image_content(&output.output[0], "image/jpeg", "YWJj");
}

/// `onPayload` can replace the wire payload; `onResponse` sees the status.
#[tokio::test]
async fn on_payload_replaces_payload_and_on_response_sees_status() {
    let (base_url, mut requests) =
        serve(vec![ScriptResponse::json(200, image_response_body())]).await;
    let model = test_model(&base_url, vec![ImagesOutputModality::Image]);
    let on_payload: rpi_ai::types::ImagesOnPayloadCallback = Arc::new(
        |payload: Value,
         _model: &ImagesModel|
         -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Option<Value>> + Send + 'static>,
        > {
            let mut payload = payload;
            payload["model"] = json!("replaced/overridden");
            Box::pin(async move { Some(payload) })
        },
    );
    let (status_tx, mut status_rx) = mpsc::channel(1);
    let on_response: rpi_ai::types::ImagesOnResponseCallback =
        Arc::new(move |response: ProviderResponse, _model: &ImagesModel| {
            let status_tx = status_tx.clone();
            Box::pin(async move {
                let _ = status_tx.send(response.status).await;
            })
        });

    generate_images(
        &model,
        &text_context("Generate a dog"),
        Some(&ImagesOptions {
            api_key: Some("test".to_owned()),
            on_payload: Some(on_payload),
            on_response: Some(on_response),
            ..Default::default()
        }),
    )
    .await
    .expect("dispatch");

    let request = requests.recv().await.expect("request");
    assert_eq!(request.body_json()["model"], json!("replaced/overridden"));
    assert_eq!(status_rx.recv().await, Some(200));
}

/// `generateImages` dispatch errors on an unregistered api (upstream throw).
#[tokio::test]
async fn dispatch_errors_for_unregistered_api() {
    let mut model = test_model("http://127.0.0.1:1/v1", vec![ImagesOutputModality::Image]);
    model.api = ImagesApiKind::from("no-such-api");

    let error = generate_images(&model, &text_context("Generate a dog"), None)
        .await
        .expect_err("registry miss");
    assert_eq!(error, "No API provider registered for api: no-such-api");
}

/// "generateImages resolves the final assistant images result" — image in
/// output for image-only models.
#[tokio::test]
async fn resolves_final_assistant_images_result() {
    let (base_url, _requests) = serve(vec![ScriptResponse::json(200, image_response_body())]).await;
    let model = test_model(&base_url, vec![ImagesOutputModality::Image]);

    let output = generate_images(
        &model,
        &text_context("Generate a dog"),
        Some(&ImagesOptions {
            api_key: Some("test".to_owned()),
            ..Default::default()
        }),
    )
    .await
    .expect("dispatch");

    assert!(
        output
            .output
            .iter()
            .any(|item| matches!(item, ImagesOutputContent::Image(_)))
    );
}

// ---------------------------------------------------------------------------
// Builtin collection + catalog (images-models.test.ts intent)
// ---------------------------------------------------------------------------

/// `builtinImagesModels` registers the openrouter provider with its catalog.
#[tokio::test]
async fn builtin_images_models_registers_openrouter_with_catalog() {
    let models = builtin_images_models(Some(rpi_ai::models::CreateModelsOptions {
        credentials: None,
        auth_context: Some(Arc::new(FakeAuthContext {
            env: [("OPENROUTER_API_KEY".to_owned(), "or-key".to_owned())].into(),
        })),
        models_store: None,
    }));
    let providers: Vec<String> = models
        .get_providers()
        .iter()
        .map(|p| p.id().to_owned())
        .collect();
    assert_eq!(providers, ["openrouter"]);

    let list = models.get_models(Some("openrouter"));
    assert!(!list.is_empty());
    assert!(list.iter().all(|m| m.api.as_str() == "openrouter-images"));

    let first = list.first().expect("first");
    let auth = models.get_auth(first, None).await.expect("auth");
    assert_eq!(
        auth.as_ref()
            .and_then(|a| a.auth.api_key.clone())
            .as_deref(),
        Some("or-key")
    );
}

/// Catalog lookups (`image-models.ts` intent): known provider/model pairs.
#[test]
fn catalog_lookup_returns_models() {
    let model = get_image_model("openrouter", "google/gemini-2.5-flash-image").expect("model");
    assert_eq!(model.name, "Google: Nano Banana (Gemini 2.5 Flash Image)");
    assert_eq!(model.base_url, "https://openrouter.ai/api/v1");
    assert_eq!(model.api.as_str(), "openrouter-images");
    assert!(model.input.contains(&InputModality::Image));
    assert!(model.output.contains(&ImagesOutputModality::Text));
    assert!(get_image_model("openrouter", "no/such-model").is_none());
    assert!(get_image_model("no-such-provider", "x").is_none());
}

/// End-to-end through `ImagesModels::generate_images` with auth resolution:
/// the resolved api key reaches the wire. The catalog model's base url is
/// pointed at the loopback server.
#[tokio::test]
async fn images_models_generate_images_applies_auth_and_dispatches() {
    let (base_url, mut requests) =
        serve(vec![ScriptResponse::json(200, image_response_body())]).await;
    let models = builtin_images_models(Some(rpi_ai::models::CreateModelsOptions {
        credentials: None,
        auth_context: Some(Arc::new(FakeAuthContext {
            env: [("OPENROUTER_API_KEY".to_owned(), "or-key".to_owned())].into(),
        })),
        models_store: None,
    }));
    let mut model = get_image_model("openrouter", "google/gemini-3.1-flash-image").expect("model");
    model.base_url = base_url;

    let output: AssistantImages = models
        .generate_images(&model, &text_context("Generate a dog"), None)
        .await;

    assert_eq!(output.stop_reason, ImagesStopReason::Stop);
    assert!(
        output
            .output
            .iter()
            .any(|item| matches!(item, ImagesOutputContent::Image(_)))
    );
    let request = requests.recv().await.expect("request");
    assert_eq!(request.header("authorization"), Some("Bearer or-key"));
}
