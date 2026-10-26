//! Model catalog and paid model calls exposed to Durable code-mode scripts.
//!
//! Catalog reads expose only public model metadata; execution resolves models
//! from the host catalog so script-supplied credentials are never trusted.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::chord::context::Context;
use crate::chord::json::JsonObject;
use crate::harness::types::{
    ToolCaller, ToolExecute, ToolExecuteError, ToolExecutionApi, ToolExecutionResult,
    ToolRegistration, ToolReplay,
};
use rpi_ai::models::Models;
use rpi_ai::types::{
    AnyModel, ClassifierContext, ClassifierOptions, ImagesContext, ModelType, Tool, Usage,
};

pub(super) const CLASSIFY_TOOL: &str = "pi.codemode.models.classify";
pub(super) const GENERATE_IMAGES_TOOL: &str = "pi.codemode.models.generateImages";

pub(super) const DESCRIPTION: &str = "models.getModelsOfType(type, provider?) and models.getAvailableOfType(type, provider?) list model metadata without credentials; models.getModelOfType(type, provider, id) looks one up. models.classify({provider,id}, {state, questions, images?}) answers typed questions; models.generateImages({provider,id}, {input}) returns text/image blocks. Use only classifier or image models for paid calls. Results carry stopReason, errorMessage, and usage.";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelReference {
    provider: String,
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClassifyArguments {
    model: ModelReference,
    context: ClassifierContext,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImageArguments {
    model: ModelReference,
    context: ImagesContext,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LookupArguments {
    #[serde(rename = "type")]
    model_type: ModelType,
    provider: String,
    id: String,
}

pub(super) fn tools() -> Vec<ToolRegistration> {
    let parameters = serde_json::json!({
        "type": "object",
        "properties": {"model": {}, "context": {}},
        "required": ["model", "context"]
    });
    vec![
        registration(
            CLASSIFY_TOOL,
            "models.classify() of code mode scripts.",
            parameters.clone(),
            Arc::new(Classify),
        ),
        registration(
            GENERATE_IMAGES_TOOL,
            "models.generateImages() of code mode scripts.",
            parameters,
            Arc::new(GenerateImages),
        ),
    ]
}

fn registration(
    name: &str,
    description: &str,
    parameters: Value,
    execute: Arc<dyn ToolExecute>,
) -> ToolRegistration {
    ToolRegistration {
        tool: Tool {
            name: name.to_owned(),
            description: description.to_owned(),
            parameters,
            constrained_sampling: None,
        },
        replay: ToolReplay::Unsafe,
        execution_mode: None,
        prepare_arguments: None,
        deferred: false,
        output_limits: None,
        structured_output_schema: None,
        callers: Some(vec![ToolCaller::Tool]),
        execute,
    }
}

pub(super) async fn catalog_call(
    name: &str,
    args: &[Value],
    models: &Models,
    _context: &Context,
) -> Result<Option<Value>, String> {
    if !matches!(
        name,
        "models.getModelsOfType" | "models.getAvailableOfType" | "models.getModelOfType"
    ) {
        return Ok(None);
    }
    if name == "models.getModelOfType" {
        if args.len() != 3 {
            return Err("models.getModelOfType(type, provider, id) expects three arguments".into());
        }
        let lookup: LookupArguments =
            parse(&serde_json::json!({"type":args[0], "provider":args[1], "id":args[2]}))?;
        let found = models.get_model_of_type(lookup.model_type, &lookup.provider, &lookup.id);
        return found.map(public_model).transpose();
    }
    let model_type: ModelType = args
        .first()
        .cloned()
        .ok_or_else(|| format!("{name} expects a model type"))
        .and_then(|value| {
            serde_json::from_value(value)
                .map_err(|_| "type must be chat, image, or classifier".to_owned())
        })?;
    let provider = match args.get(1) {
        None | Some(Value::Null) => None,
        Some(Value::String(provider)) => Some(provider.as_str()),
        Some(_) => return Err("provider must be a string".into()),
    };
    if args.len() > 2 {
        return Err(format!("{name} accepts at most two arguments"));
    }
    let values = match name {
        "models.getModelsOfType" => models.get_models_of_type(model_type, provider),
        "models.getAvailableOfType" => models
            .get_available_of_type(model_type, provider)
            .await
            .map_err(|error| error.to_string())?,
        _ => return Ok(None),
    };
    values
        .into_iter()
        .map(public_model)
        .collect::<Result<Vec<_>, _>>()
        .map(|models| Some(Value::Array(models)))
}

fn public_model(model: AnyModel) -> Result<Value, String> {
    let mut value = serde_json::to_value(model).map_err(|error| error.to_string())?;
    if let Some(object) = value.as_object_mut() {
        object.remove("headers");
    }
    Ok(value)
}

fn parse<T: for<'de> Deserialize<'de>>(value: &Value) -> Result<T, String> {
    serde_json::from_value(value.clone()).map_err(|error| format!("Invalid model call: {error}"))
}

fn result(value: Value, is_error: bool, usage: Option<Usage>) -> ToolExecutionResult {
    ToolExecutionResult {
        structured_output: Some(value),
        is_error: Some(is_error),
        usage,
        ..ToolExecutionResult::default()
    }
}

fn abort_token(context: &Context) -> Option<CancellationToken> {
    context.abort_signal().map(|signal| signal.token())
}

struct Classify;
#[async_trait]
impl ToolExecute for Classify {
    async fn execute(
        &self,
        args: JsonObject,
        api: &dyn ToolExecutionApi,
        context: &Context,
    ) -> Result<ToolExecutionResult, ToolExecuteError> {
        execute_classify_args(args, api.models(), context).await
    }
}

async fn execute_classify_args(
    args: JsonObject,
    models: &Models,
    context: &Context,
) -> Result<ToolExecutionResult, ToolExecuteError> {
    let arguments: ClassifyArguments = parse(&Value::Object(args))?;
    execute_classify(arguments, models, context).await
}

async fn execute_classify(
    arguments: ClassifyArguments,
    models: &Models,
    context: &Context,
) -> Result<ToolExecutionResult, ToolExecuteError> {
    let model = models
        .get_model_of_type(
            ModelType::Classifier,
            &arguments.model.provider,
            &arguments.model.id,
        )
        .and_then(|model| match model {
            AnyModel::Classifier(model) => Some(model),
            _ => None,
        });
    let Some(model) = model else {
        return Err(tool_error(
            "Classifier model not found; use models.getAvailableOfType('classifier') to find configured models",
        ));
    };
    let options = ClassifierOptions {
        signal: abort_token(context),
        ..ClassifierOptions::default()
    };
    let response = models
        .classify(&model, &arguments.context, Some(&options))
        .await;
    let failed = response.stop_reason != rpi_ai::types::ClassifierStopReason::Stop;
    let usage = response.usage.clone();
    Ok(result(serde_json::to_value(response)?, failed, usage))
}

struct GenerateImages;
#[async_trait]
impl ToolExecute for GenerateImages {
    async fn execute(
        &self,
        args: JsonObject,
        api: &dyn ToolExecutionApi,
        context: &Context,
    ) -> Result<ToolExecutionResult, ToolExecuteError> {
        execute_images_args(args, api.models(), context).await
    }
}

async fn execute_images_args(
    args: JsonObject,
    models: &Models,
    context: &Context,
) -> Result<ToolExecutionResult, ToolExecuteError> {
    let arguments: ImageArguments = parse(&Value::Object(args))?;
    execute_images(arguments, models, context).await
}

async fn execute_images(
    arguments: ImageArguments,
    models: &Models,
    context: &Context,
) -> Result<ToolExecutionResult, ToolExecuteError> {
    let model = models
        .get_model_of_type(
            ModelType::Image,
            &arguments.model.provider,
            &arguments.model.id,
        )
        .and_then(|model| match model {
            AnyModel::Image(model) => Some(model),
            _ => None,
        });
    let Some(model) = model else {
        return Err(tool_error(
            "Image model not found; use models.getAvailableOfType('image') to find configured models",
        ));
    };
    let options = rpi_ai::types::ImagesOptions {
        signal: abort_token(context),
        ..rpi_ai::types::ImagesOptions::default()
    };
    let response = models
        .generate_images(&model, &arguments.context, Some(&options))
        .await;
    let failed = response.stop_reason != rpi_ai::types::ImagesStopReason::Stop;
    let usage = response.usage.clone();
    Ok(result(serde_json::to_value(response)?, failed, usage))
}

fn tool_error(message: impl Into<String>) -> ToolExecuteError {
    Box::new(std::io::Error::other(message.into()))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, HashMap};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use rpi_ai::auth::types::{ApiKeyAuth, AuthContext, AuthResult, ModelAuth, ProviderAuth};
    use rpi_ai::models::{CreateProviderOptions, ProviderApi, create_provider};
    use rpi_ai::types::{
        ApiKind, AssistantImages, ClassifierAnswer, ClassifierBoolAnswer, ClassifierResult,
        ClassifierStopReason, ImageContent, ImageModel, ImagesApiKind, ImagesInputContent,
        ImagesOutputContent, ImagesStopReason, InputModality, ModelCost, ProviderClassifier,
        ProviderImageGenerator, TextContent, UsageCost,
    };

    fn usage() -> Usage {
        Usage {
            input: 4,
            output: 1,
            cache_read: 0,
            cache_write: 0,
            cache_write1h: None,
            reasoning: None,
            total_tokens: 5,
            cost: UsageCost {
                input: 0.1,
                output: 0.2,
                cache_read: 0.0,
                cache_write: 0.0,
                total: 0.3,
            },
        }
    }

    struct FixtureAuth;

    #[async_trait::async_trait]
    impl ApiKeyAuth for FixtureAuth {
        fn name(&self) -> &str {
            "Code-mode test credential"
        }

        async fn resolve(
            &self,
            _context: &dyn AuthContext,
            _credential: Option<&rpi_ai::auth::ApiKeyCredential>,
        ) -> Result<Option<AuthResult>, rpi_ai::auth::ModelsError> {
            Ok(Some(AuthResult {
                auth: ModelAuth {
                    api_key: Some("fixture-key".into()),
                    headers: None,
                    base_url: None,
                },
                env: None,
                source: Some("test fixture".into()),
            }))
        }
    }

    fn classifier() -> rpi_ai::types::ClassifierModel {
        rpi_ai::types::ClassifierModel {
            model_type: ModelType::Classifier,
            id: "classifier-id".into(),
            name: "Classifier".into(),
            api: ApiKind::from("classify"),
            provider: "fixture".into(),
            base_url: "https://fixture.invalid".into(),
            input: vec![InputModality::Text],
            input_limits: None,
            cost: ModelCost::default(),
            context_window: 4096,
            headers: Some(BTreeMap::from([("Authorization".into(), "secret".into())])),
        }
    }

    fn image_model() -> ImageModel {
        ImageModel {
            model_type: ModelType::Image,
            id: "image-id".into(),
            name: "Image generator".into(),
            api: ApiKind::from("images"),
            provider: "fixture".into(),
            base_url: "https://fixture.invalid".into(),
            input: vec![InputModality::Text],
            input_limits: None,
            cost: ModelCost::default(),
            output: vec![InputModality::Image],
            headers: None,
        }
    }

    struct ClassifierFixture {
        calls: Arc<AtomicUsize>,
        stop: ClassifierStopReason,
    }

    impl ProviderClassifier for ClassifierFixture {
        fn classify(
            &self,
            model: &rpi_ai::types::ClassifierModel,
            _context: &ClassifierContext,
            _options: Option<&ClassifierOptions>,
        ) -> std::pin::Pin<Box<dyn Future<Output = ClassifierResult> + Send + 'static>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let model = model.clone();
            let stop = self.stop;
            Box::pin(async move {
                ClassifierResult {
                    api: model.api,
                    provider: model.provider,
                    model: model.id,
                    answers: BTreeMap::from([(
                        "approved".into(),
                        ClassifierAnswer::Bool(ClassifierBoolAnswer { probability: 0.9 }),
                    )]),
                    usage: Some(usage()),
                    stop_reason: stop,
                    error_message: (stop != ClassifierStopReason::Stop)
                        .then(|| "classifier failure".into()),
                    timestamp: 1,
                }
            })
        }
    }

    struct ImageFixture {
        calls: Arc<AtomicUsize>,
        stop: ImagesStopReason,
    }

    impl ProviderImageGenerator for ImageFixture {
        fn generate_images(
            &self,
            model: &ImageModel,
            _context: &ImagesContext,
            _options: Option<&rpi_ai::types::ImagesOptions>,
        ) -> std::pin::Pin<Box<dyn Future<Output = AssistantImages> + Send + 'static>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let model = model.clone();
            let stop = self.stop;
            Box::pin(async move {
                AssistantImages {
                    api: ImagesApiKind::from(model.api.as_str()),
                    provider: model.provider,
                    model: model.id,
                    output: vec![ImagesOutputContent::Text(TextContent {
                        text: "generated image".into(),
                        text_signature: None,
                    })],
                    response_id: None,
                    usage: Some(usage()),
                    stop_reason: stop,
                    error_message: (stop != ImagesStopReason::Stop).then(|| "image failure".into()),
                    timestamp: 1,
                }
            })
        }
    }

    fn models(
        classifier_calls: Arc<AtomicUsize>,
        classifier_stop: ClassifierStopReason,
        image_calls: Arc<AtomicUsize>,
        image_stop: ImagesStopReason,
    ) -> Models {
        let models = Models::default();
        models.set_provider(create_provider(CreateProviderOptions {
            id: "fixture".into(),
            auth: ProviderAuth {
                api_key: Some(Arc::new(FixtureAuth)),
                oauth: None,
            },
            all_models: vec![
                AnyModel::Classifier(classifier()),
                AnyModel::Image(image_model()),
            ],
            classifiers: HashMap::from([(
                "classify".into(),
                Arc::new(ClassifierFixture {
                    calls: classifier_calls,
                    stop: classifier_stop,
                }) as Arc<dyn ProviderClassifier>,
            )]),
            images: HashMap::from([(
                "images".into(),
                Arc::new(ImageFixture {
                    calls: image_calls,
                    stop: image_stop,
                }) as Arc<dyn ProviderImageGenerator>,
            )]),
            api: ProviderApi::default(),
            ..CreateProviderOptions::default()
        }));
        models
    }

    fn classify_args(images: Option<Vec<ImageContent>>) -> ClassifyArguments {
        ClassifyArguments {
            model: ModelReference { provider: "fixture".into(), id: "classifier-id".into() },
            context: ClassifierContext {
                state: serde_json::from_value(serde_json::json!({"message":"review"})).expect("state"),
                images,
                questions: BTreeMap::from([(
                    "approved".into(),
                    serde_json::from_value(serde_json::json!({
                        "type":"bool","instructions":"Approve?","criteria":{"true":"yes","false":"no"}
                    })).expect("question"),
                )]),
            },
        }
    }

    fn images_args() -> ImageArguments {
        ImageArguments {
            model: ModelReference {
                provider: "fixture".into(),
                id: "image-id".into(),
            },
            context: ImagesContext {
                input: vec![ImagesInputContent::Text(TextContent {
                    text: "draw a simple shape".into(),
                    text_signature: None,
                })],
            },
        }
    }

    #[tokio::test]
    async fn catalog_call_strips_headers_from_actual_host_catalog() {
        let models = models(
            Arc::new(AtomicUsize::new(0)),
            ClassifierStopReason::Stop,
            Arc::new(AtomicUsize::new(0)),
            ImagesStopReason::Stop,
        );
        let entries = catalog_call(
            "models.getModelsOfType",
            &[serde_json::json!("classifier")],
            &models,
            &crate::chord::context::BACKGROUND_CONTEXT,
        )
        .await
        .expect("catalog")
        .expect("known global");
        assert_eq!(entries[0]["id"], "classifier-id");
        assert!(entries[0].get("headers").is_none());
    }

    #[tokio::test]
    async fn paid_model_calls_return_typed_results_and_usage() {
        let classifier_calls = Arc::new(AtomicUsize::new(0));
        let image_calls = Arc::new(AtomicUsize::new(0));
        let models = models(
            classifier_calls.clone(),
            ClassifierStopReason::Stop,
            image_calls.clone(),
            ImagesStopReason::Stop,
        );
        let context = &crate::chord::context::BACKGROUND_CONTEXT;
        let classified = execute_classify(classify_args(None), &models, context)
            .await
            .expect("classify");
        let generated = execute_images(images_args(), &models, context)
            .await
            .expect("generate");
        assert_eq!(
            classified.structured_output.as_ref().unwrap()["answers"]["approved"]["probability"],
            0.9
        );
        assert_eq!(classified.usage.as_ref().unwrap().total_tokens, 5);
        assert_eq!(classified.is_error, Some(false));
        assert_eq!(
            generated.structured_output.as_ref().unwrap()["output"][0]["text"],
            "generated image"
        );
        assert_eq!(generated.usage.as_ref().unwrap().total_tokens, 5);
        assert_eq!(generated.is_error, Some(false));
        assert_eq!(classifier_calls.load(Ordering::SeqCst), 1);
        assert_eq!(image_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn non_stop_results_are_error_marked_and_retain_usage() {
        let models = models(
            Arc::new(AtomicUsize::new(0)),
            ClassifierStopReason::Error,
            Arc::new(AtomicUsize::new(0)),
            ImagesStopReason::Error,
        );
        let context = &crate::chord::context::BACKGROUND_CONTEXT;
        let classified = execute_classify(classify_args(None), &models, context)
            .await
            .expect("result");
        let generated = execute_images(images_args(), &models, context)
            .await
            .expect("result");
        assert_eq!(classified.is_error, Some(true));
        assert_eq!(
            classified.structured_output.as_ref().unwrap()["stopReason"],
            "error"
        );
        assert_eq!(classified.usage.as_ref().unwrap().total_tokens, 5);
        assert_eq!(generated.is_error, Some(true));
        assert_eq!(
            generated.structured_output.as_ref().unwrap()["stopReason"],
            "error"
        );
        assert_eq!(generated.usage.as_ref().unwrap().total_tokens, 5);
    }

    #[tokio::test]
    async fn unsupported_classifier_image_context_does_not_reach_provider() {
        let calls = Arc::new(AtomicUsize::new(0));
        let models = models(
            calls.clone(),
            ClassifierStopReason::Stop,
            Arc::new(AtomicUsize::new(0)),
            ImagesStopReason::Stop,
        );
        let output = execute_classify(
            classify_args(Some(vec![ImageContent {
                data: "aGVsbG8=".into(),
                mime_type: "image/png".into(),
            }])),
            &models,
            &crate::chord::context::BACKGROUND_CONTEXT,
        )
        .await
        .expect("typed error result");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(output.is_error, Some(true));
        assert_eq!(
            output.structured_output.as_ref().unwrap()["stopReason"],
            "error"
        );
    }

    #[tokio::test]
    async fn malformed_context_is_rejected_before_model_dispatch() {
        let calls = Arc::new(AtomicUsize::new(0));
        let _models = models(
            calls.clone(),
            ClassifierStopReason::Stop,
            Arc::new(AtomicUsize::new(0)),
            ImagesStopReason::Stop,
        );
        let args = serde_json::json!({
            "model":{"provider":"fixture","id":"classifier-id"},
            "context":{"state":{"message":"x"},"questions":{"q":{"type":"bool","instructions":"?","criteria":{"true":"yes"}}}}
        }).as_object().expect("object arguments").clone();
        let error =
            execute_classify_args(args, &_models, &crate::chord::context::BACKGROUND_CONTEXT)
                .await
                .expect_err("incomplete question context rejected");
        assert!(error.to_string().contains("Invalid model call"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
