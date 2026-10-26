//! Port of `packages/ai/src/providers/azure.ts` @ pi 0.82.1 (2efa728; provider
//! rename + Foundry Chat Completions follow `a37306d43` / #9714 @ abe508e1b).
//!
//! No `baseUrl` upstream — the azure adapters derive it from the
//! `AZURE_OPENAI_RESOURCE_NAME`/base-URL env at request time. The provider
//! serves two APIs: `azure-openai-responses` (the Responses transport) and
//! `openai-completions` (Azure Foundry deployments; catalog entry
//! `azure/deepseek-v4-pro`). The `azure-openai-responses` **api id** and the
//! `AZURE_OPENAI_*` environment variables are unchanged by the provider
//! rename (the only breaking surface is the provider key `azure`).
//!
//! `azureStreams` (upstream): the Chat Completions leg resolves the Azure
//! endpoint onto the model before dispatch (the shared openai-completions
//! implementation reads it off the model) and swaps the deployment name into
//! the request body. Both happen inside the lazy stream, so an unconfigured
//! endpoint errors on the stream instead of out of `stream()`.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::api::azure_openai_responses::{
    AzureOpenAIResponsesOptions, AzureOpenAiResponses, resolve_azure_config,
    resolve_deployment_name,
};
use crate::api::lazy::lazy_stream;
use crate::api::openai_completions::OpenAiCompletions;
use crate::auth::{ModelsError, ModelsErrorCode, ProviderAuth, env_api_key_auth};
use crate::generated::get_builtin_models;
use crate::models::{
    CreateProviderOptions, Provider, ProviderApi, ProviderStreams, create_provider,
};
use crate::types::{
    ApiKind, Model, OnPayloadCallback, SimpleStreamOptions, StreamOptions, TranscriptContext,
};
use crate::utils::event_stream::AssistantMessageEventStream;

/// `azureProvider()`.
pub fn azure_provider() -> Arc<dyn Provider> {
    let api: HashMap<String, Arc<dyn ProviderStreams>> = HashMap::from([
        (
            ApiKind::AZURE_OPENAI_RESPONSES.to_owned(),
            Arc::new(AzureOpenAiResponses) as Arc<dyn ProviderStreams>,
        ),
        (
            ApiKind::OPENAI_COMPLETIONS.to_owned(),
            Arc::new(AzureCompletions) as Arc<dyn ProviderStreams>,
        ),
    ]);
    create_provider(CreateProviderOptions {
        id: "azure".to_owned(),
        name: Some("Azure".to_owned()),
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "Azure OpenAI API key",
                &["AZURE_OPENAI_API_KEY"],
            ))),
            oauth: None,
        },
        models: get_builtin_models("azure").to_vec(),
        api: ProviderApi::Map(api),
        ..Default::default()
    })
}

/// `withDeploymentName` (azure.ts): swap the deployment name into the request
/// body, then run the caller's own `onPayload` on the rewritten payload. A
/// callback returning `None` keeps the rewritten payload (`?? params`).
fn wrap_deployment_on_payload(
    existing: Option<OnPayloadCallback>,
    deployment_name: String,
) -> OnPayloadCallback {
    Arc::new(move |payload: Value, model: &Model| {
        let existing = existing.clone();
        let deployment_name = deployment_name.clone();
        let model = model.clone();
        Box::pin(async move {
            let mut params = payload;
            if let Value::Object(map) = &mut params {
                map.insert("model".to_owned(), json!(deployment_name));
            }
            match existing {
                Some(callback) => callback(params.clone(), &model).await.or(Some(params)),
                None => Some(params),
            }
        })
    })
}

/// `openai-completions` leg of the Azure provider (`azureStreams`).
///
/// Public like the other adapter structs so contract tests can drive it
/// through [`ProviderStreams`] directly.
#[derive(Debug, Clone, Copy, Default)]
pub struct AzureCompletions;

impl ProviderStreams for AzureCompletions {
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        dispatch(model, context.clone(), options.unwrap_or_default(), None)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> Result<AssistantMessageEventStream, String> {
        let simple = options.unwrap_or_default();
        Ok(dispatch(
            model,
            context.clone(),
            simple.stream.clone(),
            Some(simple),
        ))
    }
}

/// `lazyStream(model, () => streams.stream(...))` with the Azure model and
/// deployment resolution inside the setup future.
fn dispatch(
    model: &Model,
    context: TranscriptContext,
    mut options: StreamOptions,
    simple: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    let dispatch_model = model.clone();
    lazy_stream(model, async move {
        let azure_options = AzureOpenAIResponsesOptions {
            stream: options.clone(),
            ..AzureOpenAIResponsesOptions::default()
        };
        let config = resolve_azure_config(&dispatch_model, Some(&azure_options))
            .map_err(|message| ModelsError::new(ModelsErrorCode::Stream, message))?;
        let deployment_name = resolve_deployment_name(&dispatch_model, Some(&azure_options));
        let mut resolved = dispatch_model.clone();
        resolved.base_url = config.base_url;
        if deployment_name != dispatch_model.id {
            options.on_payload = Some(wrap_deployment_on_payload(
                options.on_payload.take(),
                deployment_name,
            ));
        }
        let inner = OpenAiCompletions;
        match simple {
            Some(mut simple) => {
                simple.stream = options;
                inner
                    .stream_simple(&resolved, &context, Some(simple))
                    .map_err(|message| ModelsError::new(ModelsErrorCode::Stream, message))
            }
            None => Ok(inner.stream(&resolved, &context, Some(options))),
        }
    })
}
