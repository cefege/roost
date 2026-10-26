//! Execution-face tests for the OpenAI Decisions classifier (V17-04 FR-G,
//! `ce8972a0e`): question mapping, answers by name, image input, usage
//! pricing tiers, refusal handling, and the non-retried gateway 504. Over a
//! scripted loopback HTTP server; no real network.

use std::sync::Arc;

use rpi_ai::api::openai_decisions::{OPENAI_DECISIONS_API, openai_decisions_api};
use rpi_ai::auth::types::{Credential, OAuthCredential, ProviderAuth};
use rpi_ai::models::{CreateProviderOptions, create_provider};
use rpi_ai::types::{
    AnyModel, ApiKind, ClassifierAnswer, ClassifierBoolCriteria, ClassifierBoolQuestion,
    ClassifierChoiceQuestion, ClassifierContext, ClassifierModel, ClassifierOptions,
    ClassifierQuestion, ClassifierScoreQuestion, ClassifierStopReason, ImageContent, InputModality,
    ModelCost, ModelCostRates, ModelCostTier,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

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
}

#[derive(Clone)]
struct ScriptResponse {
    status: u16,
    body: String,
}

impl ScriptResponse {
    fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            body: serde_json::to_string(&body).expect("json"),
        }
    }
}

/// Serves `script.len()` requests; returns the base URL and the capture
/// receiver.
async fn serve(script: Vec<ScriptResponse>) -> (String, mpsc::Receiver<CapturedRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel(script.len().max(1));
    tokio::spawn(async move {
        for response_script in script {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let request = read_request(&mut socket).await;
            tx.send(request).await.expect("send captured request");
            let response = format!(
                "HTTP/1.1 {} Status\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                response_script.status,
                response_script.body.len(),
                response_script.body
            );
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

fn decisions_model(base_url: &str) -> ClassifierModel {
    ClassifierModel {
        model_type: rpi_ai::types::ModelType::Classifier,
        id: "gpt-6-luna".to_owned(),
        name: "GPT-6 Luna".to_owned(),
        api: ApiKind::from(OPENAI_DECISIONS_API),
        provider: "openai".to_owned(),
        base_url: base_url.to_owned(),
        input: vec![InputModality::Text, InputModality::Image],
        input_limits: None,
        cost: ModelCost {
            rates: ModelCostRates {
                input: 0.1,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            tiers: Some(vec![ModelCostTier {
                input_tokens_above: 272_000,
                rates: ModelCostRates {
                    input: 0.2,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                },
            }]),
        },
        context_window: 922_000,
        headers: None,
    }
}

fn classifier_context() -> ClassifierContext {
    let mut questions = std::collections::BTreeMap::new();
    let mut choice_criteria = std::collections::BTreeMap::new();
    choice_criteria.insert("success".to_owned(), "Successful".to_owned());
    choice_criteria.insert("failure".to_owned(), String::new());
    questions.insert(
        "category".to_owned(),
        ClassifierQuestion::Choice(ClassifierChoiceQuestion {
            instructions: "Classify the message".to_owned(),
            criteria: choice_criteria,
        }),
    );
    questions.insert(
        "satisfaction".to_owned(),
        ClassifierQuestion::Score(ClassifierScoreQuestion {
            instructions: "Score satisfaction".to_owned(),
            criteria: vec!["low".to_owned(), "neutral".to_owned(), "high".to_owned()],
        }),
    );
    questions.insert(
        "approved".to_owned(),
        ClassifierQuestion::Bool(ClassifierBoolQuestion {
            instructions: "Does the user approve?".to_owned(),
            criteria: ClassifierBoolCriteria {
                yes: "Approval".to_owned(),
                no: "No approval".to_owned(),
            },
        }),
    );
    ClassifierContext {
        state: json!({ "text": "The deployment succeeded, thank you." })
            .as_object()
            .cloned()
            .expect("state object"),
        images: None,
        questions,
    }
}

/// Response shape from the API reference and live `gpt-6-luna` requests.
fn wire_answers() -> Value {
    json!([
        {
            "type": "choice",
            "name": "category",
            "choice": "success",
            "probabilities": [
                { "value": "success", "probability": 0.9 },
                { "value": "failure", "probability": 0.1 }
            ],
            "confidence": 0.8
        },
        {
            "type": "score",
            "name": "satisfaction",
            "score": 1.8,
            "probabilities": [
                { "value": 0, "label": "low", "probability": 0.05 },
                { "value": 1, "label": "neutral", "probability": 0.1 },
                { "value": 2, "label": "high", "probability": 0.85 }
            ],
            "confidence": 0.7
        },
        { "type": "predicate", "name": "approved", "probability": 0.95 }
    ])
}

fn wire_usage(input_tokens: u64) -> Value {
    json!({
        "input_tokens": input_tokens,
        "input_tokens_details": { "cached_tokens": 0, "cache_write_tokens": 0 },
        "output_tokens": 0,
        "output_tokens_details": { "reasoning_tokens": 0 },
        "total_tokens": input_tokens,
    })
}

fn image(data: &str, mime: &str) -> ImageContent {
    ImageContent {
        data: data.to_owned(),
        mime_type: mime.to_owned(),
    }
}

#[tokio::test]
async fn maps_questions_to_decisions_types_and_answers_back_by_name() {
    let (base_url, mut requests) = serve(vec![ScriptResponse::json(
        200,
        json!({
            "model": "gpt-6-luna",
            // Answers out of question order: they are matched by name.
            "answers": wire_answers().as_array().map(|entries| entries.iter().rev().cloned().collect::<Vec<_>>()).unwrap_or_default(),
            "usage": wire_usage(164),
        }),
    )])
    .await;
    let model = decisions_model(&base_url);
    let options = ClassifierOptions {
        api_key: Some("secret".to_owned()),
        ..Default::default()
    };

    let result = openai_decisions_api()
        .classify(&model, &classifier_context(), Some(&options))
        .await;
    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);

    let request = requests.recv().await.expect("captured");
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/decisions");
    assert_eq!(request.header("authorization"), Some("Bearer secret"));
    let body: Value = serde_json::from_str(&request.body).expect("request json");
    assert_eq!(
        body,
        json!({
            "model": "gpt-6-luna",
            "input": "{\"text\":\"The deployment succeeded, thank you.\"}",
            // BTreeMap order (sorted question ids); answers match by name.
            "questions": [
                {
                    "type": "predicate",
                    "name": "approved",
                    "instructions": "Does the user approve?\n\nTrue means: Approval\nFalse means: No approval"
                },
                {
                    "type": "choice",
                    "name": "category",
                    "instructions": "Classify the message",
                    // Empty descriptions are omitted.
                    "choices": [
                        { "value": "failure" },
                        { "value": "success", "description": "Successful" }
                    ]
                },
                {
                    "type": "score",
                    "name": "satisfaction",
                    "instructions": "Score satisfaction",
                    "levels": [{ "label": "low" }, { "label": "neutral" }, { "label": "high" }]
                }
            ]
        })
    );

    assert_eq!(
        result.answers,
        std::collections::BTreeMap::from([
            (
                "approved".to_owned(),
                ClassifierAnswer::Bool(rpi_ai::types::ClassifierBoolAnswer { probability: 0.95 })
            ),
            (
                "category".to_owned(),
                ClassifierAnswer::Choice(rpi_ai::types::ClassifierChoiceAnswer {
                    choice: "success".to_owned(),
                    probabilities: std::collections::BTreeMap::from([
                        ("failure".to_owned(), 0.1),
                        ("success".to_owned(), 0.9),
                    ]),
                    confidence: 0.8,
                })
            ),
            (
                "satisfaction".to_owned(),
                ClassifierAnswer::Score(rpi_ai::types::ClassifierScoreAnswer {
                    score: 1.8,
                    confidence: 0.7,
                })
            ),
        ])
    );
    let usage = result.usage.expect("usage");
    assert_eq!(usage.input, 164);
    assert_eq!(usage.output, 0);
    assert_eq!(usage.total_tokens, 164);
    assert!(
        (usage.cost.total - 0.0000164).abs() < 1e-12,
        "{:?}",
        usage.cost
    );
}

#[tokio::test]
async fn prices_long_context_requests_at_the_long_context_input_rate() {
    let (base_url, mut requests) = serve(vec![ScriptResponse::json(
        200,
        json!({ "answers": wire_answers(), "usage": wire_usage(300_000) }),
    )])
    .await;
    let model = decisions_model(&base_url);
    let options = ClassifierOptions {
        api_key: Some("secret".to_owned()),
        ..Default::default()
    };

    let result = openai_decisions_api()
        .classify(&model, &classifier_context(), Some(&options))
        .await;
    let _ = requests.recv().await;
    let usage = result.usage.expect("usage");
    // 300k > 272k → 0.2 per million.
    assert!((usage.cost.total - 0.06).abs() < 1e-12, "{:?}", usage.cost);
}

#[tokio::test]
async fn sends_images_after_the_state_in_one_user_message() {
    let (base_url, mut requests) = serve(vec![ScriptResponse::json(
        200,
        json!({ "answers": wire_answers() }),
    )])
    .await;
    let model = decisions_model(&base_url);
    let mut context = classifier_context();
    context.images = Some(vec![
        image("aW1hZ2U=", "image/png"),
        image("aW1hZ2Uy", "image/jpeg"),
    ]);
    let options = ClassifierOptions {
        api_key: Some("secret".to_owned()),
        ..Default::default()
    };

    openai_decisions_api()
        .classify(&model, &context, Some(&options))
        .await;
    let request = requests.recv().await.expect("captured");
    let body: Value = serde_json::from_str(&request.body).expect("request json");
    assert_eq!(
        body["input"],
        json!([{
            "role": "user",
            "content": [
                { "type": "input_text", "text": "{\"text\":\"The deployment succeeded, thank you.\"}" },
                { "type": "input_image", "image_url": "data:image/png;base64,aW1hZ2U=" },
                { "type": "input_image", "image_url": "data:image/jpeg;base64,aW1hZ2Uy" }
            ]
        }])
    );
}

#[tokio::test]
async fn rejects_more_images_than_the_endpoint_accepts() {
    let model = decisions_model("http://127.0.0.1:1");
    let mut context = classifier_context();
    context.images = Some(
        (0..129)
            .map(|i| image(&format!("aW1hZ2U={i}"), "image/png"))
            .collect(),
    );
    let options = ClassifierOptions {
        api_key: Some("secret".to_owned()),
        ..Default::default()
    };

    let result = openai_decisions_api()
        .classify(&model, &context, Some(&options))
        .await;
    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("at most 128 images, got 129")),
        "{:?}",
        result.error_message
    );
}

#[tokio::test]
async fn refusal_fails_the_result() {
    let (base_url, mut requests) = serve(vec![ScriptResponse::json(
        200,
        json!({
            "answers": [
                { "type": "refusal", "name": "category" },
                { "type": "score", "name": "satisfaction", "score": 1.0, "confidence": 0.5 },
                { "type": "predicate", "name": "approved", "probability": 0.5 }
            ]
        }),
    )])
    .await;
    let model = decisions_model(&base_url);
    let options = ClassifierOptions {
        api_key: Some("secret".to_owned()),
        ..Default::default()
    };

    let result = openai_decisions_api()
        .classify(&model, &classifier_context(), Some(&options))
        .await;
    let _ = requests.recv().await;
    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("refused to answer category")),
        "{:?}",
        result.error_message
    );
}

#[tokio::test]
async fn gateway_504_is_not_retried_and_gets_the_specific_message() {
    // Four scripted 504s: with the opt-out only the first is consumed.
    let (base_url, mut requests) = serve(vec![
        ScriptResponse::json(
            504,
            json!({ "error": "gateway timeout" })
        );
        4
    ])
    .await;
    let model = decisions_model(&base_url);
    let options = ClassifierOptions {
        api_key: Some("secret".to_owned()),
        ..Default::default()
    };

    let start = std::time::Instant::now();
    let result = openai_decisions_api()
        .classify(&model, &classifier_context(), Some(&options))
        .await;
    let elapsed = start.elapsed();
    let request = requests.recv().await.expect("first request captured");
    assert_eq!(request.path, "/decisions");
    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("(504)") && message.contains("600K tokens")),
        "{:?}",
        result.error_message
    );
    // No retry backoff was spent and no second request arrived (the default
    // policy would back off for at least ~375ms).
    assert!(
        elapsed < std::time::Duration::from_millis(400),
        "{elapsed:?}"
    );
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    assert!(requests.try_recv().is_err(), "the 504 must not be retried");
}

#[tokio::test]
async fn missing_api_key_fails_without_a_request() {
    let model = decisions_model("http://127.0.0.1:1");
    let result = openai_decisions_api()
        .classify(&model, &classifier_context(), None)
        .await;
    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("No API key for provider: openai")),
        "{:?}",
        result.error_message
    );
}

#[tokio::test]
async fn wrong_api_fails_fast() {
    let mut model = decisions_model("http://127.0.0.1:1");
    model.api = ApiKind::from("typesafe-system-one");
    let options = ClassifierOptions {
        api_key: Some("secret".to_owned()),
        ..Default::default()
    };
    let result = openai_decisions_api()
        .classify(&model, &classifier_context(), Some(&options))
        .await;
    assert!(
        result
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("Unsupported classifier API")),
        "{:?}",
        result.error_message
    );
}

/// The OpenAI provider exposes the Decisions classifier and hides it from
/// availability while the stored credential is an OAuth (ChatGPT) one.
#[test]
fn provider_hides_classifier_models_under_oauth() {
    let provider = rpi_ai::providers::openai::openai_provider();
    let all = provider.get_all_models();
    assert!(
        all.iter()
            .any(|model| matches!(model, AnyModel::Classifier(_))),
        "classifier catalog entries registered"
    );
    let with_key = provider.filter_all_models(all.clone(), None);
    assert!(
        with_key
            .iter()
            .any(|model| matches!(model, AnyModel::Classifier(_))),
        "api-key auth keeps the classifier"
    );
    let oauth = Credential::OAuth(OAuthCredential {
        refresh: "r".to_owned(),
        access: "a".to_owned(),
        expires: i64::MAX,
        extra: serde_json::Map::new(),
    });
    let hidden = provider.filter_all_models(all, Some(&oauth));
    assert!(
        hidden
            .iter()
            .all(|model| !matches!(model, AnyModel::Classifier(_))),
        "Sign in with ChatGPT hides the Decisions classifier"
    );
}

/// The `create_provider` hook path used by the provider above.
#[test]
fn create_provider_filter_all_models_hook_wins() {
    let provider = create_provider(CreateProviderOptions {
        id: "test".to_owned(),
        name: None,
        base_url: None,
        headers: None,
        auth: ProviderAuth::default(),
        models: Vec::new(),
        all_models: vec![AnyModel::Chat(
            serde_json::from_value(json!({
                "id": "chat", "name": "chat", "api": "openai-responses", "provider": "test",
                "baseUrl": "http://x", "reasoning": false, "input": ["text"],
                "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
                "contextWindow": 1000, "maxTokens": 100
            }))
            .expect("chat"),
        )],
        classifiers: std::collections::HashMap::from([(
            OPENAI_DECISIONS_API.to_owned(),
            openai_decisions_api(),
        )]),
        images: std::collections::HashMap::new(),
        api: rpi_ai::models::ProviderApi::Single(Arc::new(
            rpi_ai::api::openai_completions::OpenAiCompletions,
        )),
        fetch_models: None,
        filter_models_fn: None,
        filter_all_models_fn: Some(Arc::new(|_models, _credential| Vec::new())),
    });
    assert!(
        provider
            .filter_all_models(provider.get_all_models(), None)
            .is_empty()
    );
}
