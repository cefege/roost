//! Execution-face tests for the unified classifier / image-generation
//! runtimes (V16-07 FR-F/FR-G): System One transports over a scripted
//! loopback HTTP server, auth resolution, never-reject semantics, and the
//! type guards. No real network.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rpi_ai::api::cloudflare_workers_ai_system_one::cloudflare_workers_ai_system_one_api;
use rpi_ai::api::typesafe_system_one::typesafe_system_one_api;
use rpi_ai::auth::ModelsError;
use rpi_ai::auth::types::{ApiKeyAuth, AuthContext, AuthResult, ModelAuth, ProviderAuth};
use rpi_ai::models::{CreateProviderOptions, Models, ProviderApi, create_provider};
use rpi_ai::types::{
    AnyModel, ApiKind, AssistantImages, ClassifierAnswer, ClassifierBoolCriteria,
    ClassifierBoolQuestion, ClassifierChoiceQuestion, ClassifierContext, ClassifierModel,
    ClassifierOptions, ClassifierQuestion, ClassifierResult, ClassifierScoreQuestion,
    ClassifierStopReason, ImageModel, ImagesContext, ImagesInputContent, InputModality, ModelCost,
    ModelCostRates, ProviderImageGenerator, TextContent, assert_classifier_model,
    assert_image_model,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

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

struct StaticKeyAuth;

#[async_trait::async_trait]
impl ApiKeyAuth for StaticKeyAuth {
    fn name(&self) -> &str {
        "Test API key"
    }

    async fn resolve(
        &self,
        _ctx: &dyn AuthContext,
        _credential: Option<&rpi_ai::auth::ApiKeyCredential>,
    ) -> Result<Option<AuthResult>, ModelsError> {
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: Some("sk-test".to_owned()),
                headers: None,
                base_url: None,
            },
            env: None,
            source: Some("TEST_API_KEY".to_owned()),
        }))
    }
}

fn classifier_model(base_url: &str, api: &str) -> ClassifierModel {
    ClassifierModel {
        model_type: rpi_ai::types::ModelType::Classifier,
        id: "jev-latest".to_owned(),
        name: "Jev".to_owned(),
        api: ApiKind::from(api),
        provider: "typesafe".to_owned(),
        base_url: base_url.to_owned(),
        input: vec![InputModality::Text],
        input_limits: None,
        cost: ModelCost {
            rates: ModelCostRates {
                input: 3.0,
                output: 15.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            tiers: None,
        },
        context_window: 64_000,
        headers: None,
    }
}

fn classifier_context() -> ClassifierContext {
    let mut questions = std::collections::BTreeMap::new();
    questions.insert(
        "q1".to_owned(),
        ClassifierQuestion::Choice(ClassifierChoiceQuestion {
            instructions: "Pick".to_owned(),
            criteria: [
                ("a".to_owned(), "first".to_owned()),
                ("b".to_owned(), "second".to_owned()),
            ]
            .into_iter()
            .collect(),
        }),
    );
    questions.insert(
        "q2".to_owned(),
        ClassifierQuestion::Score(ClassifierScoreQuestion {
            instructions: "Score".to_owned(),
            criteria: vec!["low".to_owned(), "high".to_owned()],
        }),
    );
    questions.insert(
        "q3".to_owned(),
        ClassifierQuestion::Bool(ClassifierBoolQuestion {
            instructions: "Yes or no".to_owned(),
            criteria: ClassifierBoolCriteria {
                yes: "yes".to_owned(),
                no: "no".to_owned(),
            },
        }),
    );
    ClassifierContext {
        state: json!({ "files": 2 })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        images: None,
        questions,
    }
}

fn models_with_classifier(base_url: &str) -> Models {
    let models = Models::new(None);
    let provider = create_provider(CreateProviderOptions {
        id: "typesafe".to_owned(),
        name: Some("TypeSafe".to_owned()),
        auth: ProviderAuth {
            api_key: Some(Arc::new(StaticKeyAuth)),
            oauth: None,
        },
        models: Vec::new(),
        all_models: Vec::new(),
        classifiers: std::collections::HashMap::from([(
            "typesafe-system-one".to_owned(),
            typesafe_system_one_api(),
        )]),
        images: std::collections::HashMap::new(),
        base_url: Some(base_url.to_owned()),
        headers: None,
        api: ProviderApi::Single(Arc::new(rpi_ai::api::openai_completions::OpenAiCompletions)),
        fetch_models: None,
        filter_models_fn: None,
        filter_all_models_fn: None,
    });
    models.set_provider(provider);
    models
}

#[tokio::test]
async fn classify_maps_bool_to_noul_prices_usage_and_applies_auth() {
    let (base_url, mut requests) = serve(vec![ScriptResponse::json(
        200,
        json!({
            "answers": {
                "q1": { "type": "choice", "choice": "a", "probabilities": { "a": 0.7, "b": 0.3 }, "confidence": 0.7 },
                "q2": { "type": "score", "score": 2, "confidence": 0.5 },
                "q3": { "type": "noul", "noul": 0.9 }
            },
            "usage": { "input_tokens": 100, "output_tokens": 20 }
        }),
    )])
    .await;
    let models = models_with_classifier(&base_url);
    let mut options = ClassifierOptions {
        headers: Some(
            [("x-extra".to_owned(), Some("1".to_owned()))]
                .into_iter()
                .collect(),
        ),
        temperature: Some(0.5),
        ..Default::default()
    };
    options.api_key = None;
    let result: ClassifierResult = models
        .classify(
            &classifier_model(&base_url, "typesafe-system-one"),
            &classifier_context(),
            Some(&options),
        )
        .await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(result.model, "jev-latest");
    match result.answers.get("q1") {
        Some(ClassifierAnswer::Choice(answer)) => {
            assert_eq!(answer.choice, "a");
            assert_eq!(answer.confidence, 0.7);
        }
        other => panic!("q1: {other:?}"),
    }
    match result.answers.get("q3") {
        Some(ClassifierAnswer::Bool(answer)) => assert_eq!(answer.probability, 0.9),
        other => panic!("q3: {other:?}"),
    }
    let usage = result.usage.expect("usage");
    assert_eq!(usage.input, 100);
    assert_eq!(usage.output, 20);
    assert!((usage.cost.total - 0.0006).abs() < 1e-9, "{:?}", usage.cost);

    let request = requests.recv().await.expect("request");
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/systemone");
    assert_eq!(request.header("authorization"), Some("Bearer sk-test"));
    assert_eq!(request.header("x-extra"), Some("1"));
    let body: Value = serde_json::from_str(&request.body).expect("json");
    assert_eq!(body["model"], json!("jev-latest"));
    assert_eq!(body["state"], json!({ "files": 2 }));
    assert_eq!(body["questions"]["q1"]["type"], json!("choice"));
    // Public `bool` questions cross the wire as `noul`.
    assert_eq!(body["questions"]["q3"]["type"], json!("noul"));
    assert_eq!(body["questions"]["q3"]["criteria"]["true"], json!("yes"));
    // `temperature` is accepted but the shared System One transport does not
    // put it on the wire (system-one-shared.ts only documents the knob).
    assert!(body.get("temperature").is_none(), "{body}");
}

#[tokio::test]
async fn classify_retries_twice_by_default_and_returns_an_error_result() {
    let (base_url, mut requests) = serve(vec![
        ScriptResponse::json(500, json!({ "error": "boom" })),
        ScriptResponse::json(500, json!({ "error": "boom" })),
        ScriptResponse::json(500, json!({ "error": "boom" })),
    ])
    .await;
    let models = models_with_classifier(&base_url);
    let result = models
        .classify(
            &classifier_model(&base_url, "typesafe-system-one"),
            &classifier_context(),
            None,
        )
        .await;
    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    let message = result.error_message.expect("error message");
    assert!(message.contains("System One API error (500)"), "{message}");
    // `maxRetries ?? 2` → three attempts.
    for _ in 0..3 {
        requests.recv().await.expect("request");
    }
}

#[tokio::test]
async fn classify_without_a_key_or_provider_is_an_error_result() {
    let (base_url, _requests) = serve(vec![]).await;
    let models = Models::new(None);
    let result = models
        .classify(
            &classifier_model(&base_url, "typesafe-system-one"),
            &classifier_context(),
            None,
        )
        .await;
    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("Unknown provider: typesafe")
    );

    // Provider present but no resolved credential: "Provider is not
    // configured" (never rejects).
    let models = Models::new(None);
    let provider = create_provider(CreateProviderOptions {
        id: "typesafe".to_owned(),
        auth: ProviderAuth {
            api_key: None,
            oauth: None,
        },
        classifiers: std::collections::HashMap::from([(
            "typesafe-system-one".to_owned(),
            typesafe_system_one_api(),
        )]),
        models: Vec::new(),
        all_models: Vec::new(),
        images: std::collections::HashMap::new(),
        base_url: None,
        headers: None,
        name: None,
        api: ProviderApi::Single(Arc::new(rpi_ai::api::openai_completions::OpenAiCompletions)),
        fetch_models: None,
        filter_models_fn: None,
        filter_all_models_fn: None,
    });
    models.set_provider(provider);
    let result = models
        .classify(
            &classifier_model(&base_url, "typesafe-system-one"),
            &classifier_context(),
            None,
        )
        .await;
    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("Provider is not configured"),
        "{result:?}"
    );

    // A provider without a classifier implementation reports that.
    let models = Models::new(None);
    let provider = create_provider(CreateProviderOptions {
        id: "typesafe".to_owned(),
        auth: ProviderAuth {
            api_key: Some(Arc::new(StaticKeyAuth)),
            oauth: None,
        },
        classifiers: std::collections::HashMap::new(),
        models: Vec::new(),
        all_models: Vec::new(),
        images: std::collections::HashMap::new(),
        base_url: None,
        headers: None,
        name: None,
        api: ProviderApi::Single(Arc::new(rpi_ai::api::openai_completions::OpenAiCompletions)),
        fetch_models: None,
        filter_models_fn: None,
        filter_all_models_fn: None,
    });
    models.set_provider(provider);
    let result = models
        .classify(
            &classifier_model(&base_url, "typesafe-system-one"),
            &classifier_context(),
            None,
        )
        .await;
    assert!(
        result
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("has no classifier implementation"),
        "{result:?}"
    );
}

#[tokio::test]
async fn classify_marks_aborted_when_the_signal_fires() {
    let (base_url, _requests) = serve(vec![]).await;
    let models = models_with_classifier(&base_url);
    let signal = CancellationToken::new();
    signal.cancel();
    let result = models
        .classify(
            &classifier_model(&base_url, "typesafe-system-one"),
            &classifier_context(),
            Some(&ClassifierOptions {
                signal: Some(signal),
                ..Default::default()
            }),
        )
        .await;
    assert_eq!(result.stop_reason, ClassifierStopReason::Aborted);
}

#[tokio::test]
async fn cloudflare_transport_unwraps_the_rest_envelope() {
    let (base_url, mut requests) = serve(vec![ScriptResponse::json(
        200,
        json!({
            "success": true,
            "result": {
                "state": "Completed",
                "result": {
                    "answers": { "q1": { "type": "choice", "choice": "b", "probabilities": {}, "confidence": 1 } },
                    "usage": { "input_tokens": 1, "output_tokens": 1 }
                }
            }
        }),
    )])
    .await;
    let models = Models::new(None);
    let provider = create_provider(CreateProviderOptions {
        id: "cloudflare-workers-ai".to_owned(),
        auth: ProviderAuth {
            api_key: Some(Arc::new(StaticKeyAuth)),
            oauth: None,
        },
        classifiers: std::collections::HashMap::from([(
            "cloudflare-workers-ai-system-one".to_owned(),
            cloudflare_workers_ai_system_one_api(),
        )]),
        models: Vec::new(),
        all_models: Vec::new(),
        images: std::collections::HashMap::new(),
        base_url: None,
        headers: None,
        name: None,
        api: ProviderApi::Single(Arc::new(rpi_ai::api::openai_completions::OpenAiCompletions)),
        fetch_models: None,
        filter_models_fn: None,
        filter_all_models_fn: None,
    });
    models.set_provider(provider);
    let mut model = classifier_model(&base_url, "cloudflare-workers-ai-system-one");
    model.provider = "cloudflare-workers-ai".to_owned();
    // One question: the Cloudflare envelope only carries that answer.
    let mut context = classifier_context();
    context.questions.retain(|id, _| id == "q1");
    let result = models.classify(&model, &context, None).await;
    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    assert!(matches!(
        result.answers.get("q1"),
        Some(ClassifierAnswer::Choice(_))
    ));
    let request = requests.recv().await.expect("request");
    assert_eq!(request.path, "/run");
    let body: Value = serde_json::from_str(&request.body).expect("json");
    assert_eq!(body["model"], json!("jev-latest"));
    assert!(body["input"]["questions"].is_object());
}

/// A fake image generator that records auth/options and returns a fixed
/// result; used to test the unified `Models::generate_images` surface
/// without HTTP.
struct RecordingGenerator {
    calls: Arc<AtomicUsize>,
    api_key: Arc<std::sync::Mutex<Option<String>>>,
    base_url: Arc<std::sync::Mutex<Option<String>>>,
}

impl ProviderImageGenerator for RecordingGenerator {
    fn generate_images(
        &self,
        model: &ImageModel,
        _context: &ImagesContext,
        options: Option<&rpi_ai::types::ImagesOptions>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = AssistantImages> + Send + 'static>>
    {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.api_key.lock().unwrap() = options.and_then(|options| options.api_key.clone());
        *self.base_url.lock().unwrap() = Some(model.base_url.clone());
        let model = model.clone();
        Box::pin(
            async move { rpi_ai::types::image_error_result(&model, "generator failure", false) },
        )
    }
}

#[tokio::test]
async fn generate_images_resolves_auth_and_never_rejects() {
    let calls = Arc::new(AtomicUsize::new(0));
    let api_key = Arc::new(std::sync::Mutex::new(None));
    let base_url_seen = Arc::new(std::sync::Mutex::new(None));
    let models = Models::new(None);
    let provider = create_provider(CreateProviderOptions {
        id: "openrouter".to_owned(),
        auth: ProviderAuth {
            api_key: Some(Arc::new(StaticKeyAuth)),
            oauth: None,
        },
        images: std::collections::HashMap::from([(
            "openrouter-images".to_owned(),
            Arc::new(RecordingGenerator {
                calls: calls.clone(),
                api_key: api_key.clone(),
                base_url: base_url_seen.clone(),
            }) as Arc<dyn ProviderImageGenerator>,
        )]),
        models: Vec::new(),
        all_models: Vec::new(),
        classifiers: std::collections::HashMap::new(),
        base_url: None,
        headers: None,
        name: None,
        api: ProviderApi::Single(Arc::new(rpi_ai::api::openai_completions::OpenAiCompletions)),
        fetch_models: None,
        filter_models_fn: None,
        filter_all_models_fn: None,
    });
    models.set_provider(provider);
    let mut model = ImageModel {
        model_type: rpi_ai::types::ModelType::Image,
        id: "flux".to_owned(),
        name: "Flux".to_owned(),
        api: ApiKind::from("openrouter-images"),
        provider: "openrouter".to_owned(),
        base_url: "https://example.invalid/api".to_owned(),
        input: vec![InputModality::Text],
        input_limits: None,
        cost: ModelCost {
            rates: ModelCostRates {
                input: 1.0,
                output: 1.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            tiers: None,
        },
        output: vec![InputModality::Image],
        headers: None,
    };
    let context = ImagesContext {
        input: vec![ImagesInputContent::Text(TextContent {
            text: "draw".to_owned(),
            text_signature: None,
        })],
    };
    let result = models.generate_images(&model, &context, None).await;
    // Never rejects: the fake generator's error shape comes back.
    assert_eq!(result.stop_reason, rpi_ai::types::ImagesStopReason::Error);
    assert_eq!(result.error_message.as_deref(), Some("generator failure"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(api_key.lock().unwrap().as_deref(), Some("sk-test"));

    // A request-time base URL override (auth resolution) reaches the
    // implementation.
    model.base_url = "https://override.invalid".to_owned();
    let _ = models.generate_images(&model, &context, None).await;
    assert_eq!(
        base_url_seen.lock().unwrap().as_deref(),
        Some("https://override.invalid")
    );

    // Unknown provider / no implementation still returns a result.
    let unknown = models
        .generate_images(
            &ImageModel {
                provider: "nope".to_owned(),
                ..model.clone()
            },
            &context,
            None,
        )
        .await;
    assert_eq!(unknown.stop_reason, rpi_ai::types::ImagesStopReason::Error);
    assert!(
        unknown
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("Unknown provider"),
        "{unknown:?}"
    );
}

#[test]
fn type_guards_reject_mismatched_models() {
    let chat: AnyModel = serde_json::from_value(json!({
        "id": "m", "name": "m", "api": "openai-completions", "provider": "p",
        "baseUrl": "https://example.com", "reasoning": false, "input": ["text"],
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
        "contextWindow": 1000, "maxTokens": 100
    }))
    .expect("chat");
    let error = assert_image_model(&chat).expect_err("not an image model");
    assert!(
        error.message.contains("is not a image model"),
        "{}",
        error.message
    );
    let error = assert_classifier_model(&chat).expect_err("not a classifier");
    assert!(error.message.contains("is not a classifier model"));
}

/// `ClassifierContext.images` (ce8972a0e): a context with one image part.
fn classifier_context_with_image() -> ClassifierContext {
    let mut context = classifier_context();
    context.images = Some(vec![rpi_ai::types::ImageContent {
        data: "aGVsbG8=".to_owned(),
        mime_type: "image/png".to_owned(),
    }]);
    context
}

/// Records the context it was called with and returns a fixed result.
#[derive(Default)]
struct CapturingClassifier {
    contexts: std::sync::Mutex<Vec<ClassifierContext>>,
}

impl rpi_ai::types::ProviderClassifier for CapturingClassifier {
    fn classify(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
        _options: Option<&ClassifierOptions>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ClassifierResult> + Send + 'static>>
    {
        self.contexts
            .lock()
            .expect("contexts")
            .push(context.clone());
        let model = model.clone();
        Box::pin(async move {
            ClassifierResult {
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                answers: std::collections::BTreeMap::new(),
                usage: None,
                stop_reason: ClassifierStopReason::Stop,
                error_message: None,
                timestamp: 0,
            }
        })
    }
}

fn models_with_capture_classifier(
    classifier: Arc<dyn rpi_ai::types::ProviderClassifier>,
) -> Models {
    let models = Models::new(None);
    let provider = create_provider(CreateProviderOptions {
        id: "typesafe".to_owned(),
        name: Some("TypeSafe".to_owned()),
        auth: ProviderAuth {
            api_key: Some(Arc::new(StaticKeyAuth)),
            oauth: None,
        },
        models: Vec::new(),
        all_models: Vec::new(),
        classifiers: std::collections::HashMap::from([("capture-classify".to_owned(), classifier)]),
        images: std::collections::HashMap::new(),
        base_url: Some("http://127.0.0.1:1".to_owned()),
        headers: None,
        api: ProviderApi::Single(Arc::new(rpi_ai::api::openai_completions::OpenAiCompletions)),
        fetch_models: None,
        filter_models_fn: None,
        filter_all_models_fn: None,
    });
    models.set_provider(provider);
    models
}

/// Images reach an image-capable classifier unchanged (the codemode
/// `models.classify()` consumption face, R3.2.6).
#[tokio::test]
async fn classifier_images_reach_an_image_capable_provider() {
    let classifier = Arc::new(CapturingClassifier::default());
    let models = models_with_capture_classifier(classifier.clone());
    let mut model = classifier_model("http://127.0.0.1:1", "capture-classify");
    model.input = vec![InputModality::Text, InputModality::Image];

    let result = models
        .classify(&model, &classifier_context_with_image(), None)
        .await;
    assert_eq!(result.stop_reason, ClassifierStopReason::Stop, "{result:?}");
    let contexts = classifier.contexts.lock().expect("contexts");
    assert_eq!(contexts.len(), 1);
    let images = contexts[0].images.as_ref().expect("images");
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].mime_type, "image/png");
    assert_eq!(images[0].data, "aGVsbG8=");
}

/// A model whose catalog entry does not accept image input returns the
/// upstream error result before any request (`assertClassifierInputSupported`,
/// ce8972a0e).
#[tokio::test]
async fn classifier_images_require_an_image_capable_model() {
    let models = models_with_classifier("http://127.0.0.1:1");
    let result = models
        .classify(
            &classifier_model("http://127.0.0.1:1", "typesafe-system-one"),
            &classifier_context_with_image(),
            None,
        )
        .await;
    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    let message = result.error_message.expect("error message");
    assert!(
        message.contains("Model typesafe/jev-latest does not accept image input"),
        "{message}"
    );
}

/// APIs that cannot send images fail in the transport with their own
/// message once the model entry accepts image input (ce8972a0e).
#[tokio::test]
async fn system_one_transports_reject_images() {
    let models = models_with_classifier("http://127.0.0.1:1");
    let mut model = classifier_model("http://127.0.0.1:1", "typesafe-system-one");
    model.input = vec![InputModality::Text, InputModality::Image];
    let result = models
        .classify(&model, &classifier_context_with_image(), None)
        .await;
    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    let message = result.error_message.expect("error message");
    assert!(
        message.contains("System One API does not support image input"),
        "{message}"
    );
}

/// Without images the new field changes nothing (zero regression).
#[tokio::test]
async fn classifier_without_images_still_succeeds() {
    let classifier = Arc::new(CapturingClassifier::default());
    let models = models_with_capture_classifier(classifier.clone());
    let mut model = classifier_model("http://127.0.0.1:1", "capture-classify");
    model.input = vec![InputModality::Text];
    let result = models.classify(&model, &classifier_context(), None).await;
    assert_eq!(result.stop_reason, ClassifierStopReason::Stop, "{result:?}");
    let contexts = classifier.contexts.lock().expect("contexts");
    assert!(contexts[0].images.is_none());
}
