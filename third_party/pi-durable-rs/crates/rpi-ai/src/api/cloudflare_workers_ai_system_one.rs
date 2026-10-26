//! Cloudflare Workers AI System One classification (port of
//! `packages/ai/src/api/cloudflare-workers-ai-system-one.ts` @ abe508e1b).
//!
//! System One models on the Workers AI REST endpoint: `POST
//! /accounts/{account}/ai/run` with `{ model, input }`. The REST API wraps
//! the model output in Cloudflare's API envelope and a run record:
//! `{ success, result: { state: "Completed", result: { answers, usage } } }`.

use std::sync::Arc;

use serde_json::{Value, json};
use url::Url;

use crate::api::classifier_shared::is_record;
use crate::api::system_one_shared::{
    SystemOneTransport, SystemOneWireRequest, classify_system_one,
};
use crate::types::{
    ClassifierContext, ClassifierModel, ClassifierOptions, ClassifierResult, ProviderClassifier,
};

pub const CLOUDFLARE_WORKERS_AI_SYSTEM_ONE_API: &str = "cloudflare-workers-ai-system-one";

const LABEL: &str = "Cloudflare Workers AI";

fn cloudflare_error_message(errors: Option<&Value>) -> String {
    if let Some(Value::Array(errors)) = errors {
        let messages: Vec<String> = errors
            .iter()
            .filter_map(|error| {
                if is_record(error) {
                    error
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                } else {
                    None
                }
            })
            .collect();
        if !messages.is_empty() {
            return format!("{LABEL} error: {}", messages.join("; "));
        }
    }
    format!("{LABEL} request failed")
}

fn url(model: &ClassifierModel) -> Result<Url, String> {
    Url::parse(&format!("{}/run", model.base_url.trim_end_matches('/')))
        .map_err(|error| error.to_string())
}

fn payload(model: &ClassifierModel, request: SystemOneWireRequest) -> Value {
    json!({
        "model": model.id,
        "input": { "state": request.state, "questions": request.questions },
    })
}

fn output(body: &Value) -> Result<Value, String> {
    if !is_record(body) {
        return Err(format!("{LABEL} returned an unexpected response"));
    }
    if body.get("success") == Some(&Value::Bool(false)) {
        return Err(cloudflare_error_message(body.get("errors")));
    }
    let Some(run) = body.get("result") else {
        return Err(format!("{LABEL} returned an unexpected response"));
    };
    if !is_record(run) {
        return Err(format!("{LABEL} returned an unexpected response"));
    }
    if run.get("state").and_then(Value::as_str) != Some("Completed") {
        return Err(format!(
            "{LABEL} run did not complete (state: {})",
            run.get("state")
                .map(Value::to_string)
                .unwrap_or_else(|| "undefined".to_owned())
        ));
    }
    let Some(result) = run.get("result") else {
        return Err(format!("{LABEL} returned an unexpected response"));
    };
    if !is_record(result) {
        return Err(format!("{LABEL} returned an unexpected response"));
    }
    Ok(result.clone())
}

static TRANSPORT: SystemOneTransport = SystemOneTransport {
    api: CLOUDFLARE_WORKERS_AI_SYSTEM_ONE_API,
    label: LABEL,
    url,
    payload,
    output,
};

/// The `cloudflare-workers-ai-system-one` api implementation.
#[derive(Debug, Clone, Copy, Default)]
pub struct CloudflareWorkersAiSystemOne;

impl ProviderClassifier for CloudflareWorkersAiSystemOne {
    fn classify(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
        options: Option<&ClassifierOptions>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ClassifierResult> + Send + 'static>>
    {
        let model = model.clone();
        let context = context.clone();
        let options = options.cloned();
        Box::pin(async move {
            classify_system_one(&TRANSPORT, &model, &context, options.as_ref()).await
        })
    }
}

/// `cloudflareWorkersAISystemOneApi()`.
pub fn cloudflare_workers_ai_system_one_api() -> Arc<dyn ProviderClassifier> {
    Arc::new(CloudflareWorkersAiSystemOne)
}
