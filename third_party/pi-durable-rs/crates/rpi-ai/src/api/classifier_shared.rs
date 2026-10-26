//! Shared classifier HTTP plumbing (port of
//! `packages/ai/src/api/classifier-shared.ts` @ `abe508e1b`, `ce8972a0e`).
//!
//! Owns the request shape, headers, per-attempt timeout, retry/cancel
//! semantics, usage parsing, and response parsing helpers shared by the
//! System One transports and the OpenAI Decisions classifier.

use serde_json::Value;
use url::Url;

use crate::api::http_client::adapter_client_builder;
use crate::types::{ClassifierModel, ClassifierOptions, ProviderHeaders, ProviderResponse, Usage};
use crate::utils::cost::calculate_cost_for;
use crate::utils::custom_fetch::send_provider_request;
use crate::utils::error_body::{NormalizedProviderError, format_provider_error};
use crate::utils::headers::{headers_to_record, merge_headers, provider_headers_to_header_map};
use crate::utils::provider_retry::{
    ProviderErrorInfo, ProviderRetryOptions, RetryError, retry_provider_request,
};

pub fn is_record(value: &Value) -> bool {
    value.is_object()
}

pub fn required_number(label: &str, value: &Value, field: &str) -> Result<f64, String> {
    value
        .as_f64()
        .filter(|number| number.is_finite())
        .ok_or_else(|| format!("{label} returned an invalid {field}"))
}

/// Builds the bearer + JSON headers, model headers first, then option
/// headers (`None` deletes).
pub fn classifier_request_headers(
    model: &ClassifierModel,
    api_key: &str,
    options_headers: Option<&ProviderHeaders>,
) -> Result<reqwest::header::HeaderMap, String> {
    let model_headers: Option<ProviderHeaders> = model.headers.as_ref().map(|headers| {
        headers
            .iter()
            .map(|(key, value)| (key.clone(), Some(value.clone())))
            .collect()
    });
    let merged = merge_headers(model_headers.as_ref(), options_headers);
    let mut map = reqwest::header::HeaderMap::new();
    map.insert(
        reqwest::header::AUTHORIZATION,
        reqwest::header::HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|error| format!("Invalid api key header: {error}"))?,
    );
    map.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    let empty = ProviderHeaders::new();
    let request_headers = provider_headers_to_header_map(merged.as_ref().unwrap_or(&empty))?;
    for (name, value) in &request_headers {
        map.insert(name, value.clone());
    }
    Ok(map)
}

/// A failed classifier request: `message` is the display text, `status` the
/// HTTP status when the failure came from a response (needed for the
/// Decisions gateway-504 wording).
#[derive(Debug, Clone)]
pub struct ClassifierPostError {
    pub status: Option<u16>,
    pub message: String,
    /// The caller's signal fired before the request completed.
    pub aborted: bool,
}

/// Posts one JSON classifier request with bearer auth, `onPayload`/
/// `onResponse` hooks, a fresh timeout per attempt, and provider retries.
/// Returns the parsed response body and its response metadata.
/// `no_retry_statuses` lists HTTP statuses that fail at once although they
/// are normally retried (the Decisions gateway 504).
pub async fn post_classifier_request(
    label: &str,
    url: Url,
    model: &ClassifierModel,
    body: Value,
    options: Option<&ClassifierOptions>,
    no_retry_statuses: &[u16],
) -> Result<(Value, ProviderResponse), ClassifierPostError> {
    let Some(api_key) = options.and_then(|options| options.api_key.clone()) else {
        return Err(ClassifierPostError {
            status: None,
            message: format!("No API key for provider: {}", model.provider),
            aborted: false,
        });
    };
    let mut payload = body;
    if let Some(on_payload) = options.and_then(|options| options.on_payload.as_ref())
        && let Some(next) = on_payload(payload.clone(), model).await
    {
        payload = next;
    }
    let headers = classifier_request_headers(
        model,
        &api_key,
        options.and_then(|options| options.headers.as_ref()),
    )
    .map_err(|message| ClassifierPostError {
        status: None,
        message,
        aborted: false,
    })?;
    let timeout_ms = options.and_then(|options| options.timeout_ms);
    let mut client_builder =
        adapter_client_builder(options.and_then(|o| o.env.as_ref()), url.as_str()).map_err(
            |message| ClassifierPostError {
                status: None,
                message,
                aborted: false,
            },
        )?;
    if let Some(timeout_ms) = timeout_ms {
        client_builder = client_builder.timeout(std::time::Duration::from_millis(timeout_ms));
    }
    let client = client_builder
        .build()
        .map_err(|error| ClassifierPostError {
            status: None,
            message: error.to_string(),
            aborted: false,
        })?;
    let error_context = format!("{label} error");

    let (body, response_meta) = retry_provider_request(
        || {
            let request = client
                .post(url.clone())
                .headers(headers.clone())
                .json(&payload);
            let signal = options.and_then(|options| options.signal.clone());
            let fetch = options.and_then(|options| options.fetch.clone());
            let error_context = error_context.clone();
            async move {
                let result =
                    send_provider_request(request, fetch.as_ref(), signal.as_ref(), None).await;
                match result {
                    Ok(response) => {
                        let status = response.status();
                        let response_meta = ProviderResponse {
                            status: status.as_u16(),
                            headers: headers_to_record(response.headers()),
                        };
                        if status.is_success() {
                            let body =
                                response.text().await.map_err(|error| ProviderErrorInfo {
                                    status: None,
                                    headers: None,
                                    message: crate::utils::transport_error::format_reqwest_error(
                                        &error,
                                    ),
                                })?;
                            let parsed = serde_json::from_str::<Value>(&body).map_err(|error| {
                                ProviderErrorInfo {
                                    status: None,
                                    headers: None,
                                    message: format!(
                                        "{label} returned an unexpected response: {error}"
                                    ),
                                }
                            })?;
                            Ok((parsed, response_meta))
                        } else {
                            let status = status.as_u16();
                            let response_headers = headers_to_record(response.headers());
                            let body = response.text().await.unwrap_or_default();
                            let normalized = NormalizedProviderError::new(
                                Some(status),
                                Some(body),
                                format!("{label} returned {status}"),
                            );
                            Err(ProviderErrorInfo {
                                status: Some(status),
                                headers: Some(response_headers),
                                message: format_provider_error(&normalized, Some(&error_context)),
                            })
                        }
                    }
                    Err(error) => Err(error.into_provider_error_info()),
                }
            }
        },
        ProviderRetryOptions {
            // `maxRetries ?? 2` (classifier-shared.ts:99).
            max_retries: options.and_then(|options| options.max_retries).or(Some(2)),
            max_retry_delay_ms: options.and_then(|options| options.max_retry_delay_ms),
            no_retry_statuses: if no_retry_statuses.is_empty() {
                None
            } else {
                Some(no_retry_statuses.to_vec())
            },
        },
        options.and_then(|options| options.signal.as_ref()),
    )
    .await
    .map_err(|error| match error {
        RetryError::Aborted => ClassifierPostError {
            status: None,
            message: "Request aborted".to_owned(),
            aborted: true,
        },
        RetryError::Provider(info) => ClassifierPostError {
            status: info.status,
            message: info.message,
            aborted: false,
        },
        RetryError::Message(message) => ClassifierPostError {
            status: None,
            message,
            aborted: false,
        },
    })?;

    if let Some(on_response) = options.and_then(|options| options.on_response.as_ref()) {
        on_response(response_meta.clone(), model).await;
    }
    Ok((body, response_meta))
}

fn token_count(value: Option<&Value>) -> u64 {
    value
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value > 0.0)
        .map(|value| value as u64)
        .unwrap_or(0)
}

/// Usage from `{ input_tokens, output_tokens }`, priced from the model
/// catalog like chat usage. A missing or malformed usage object leaves the
/// result without usage instead of failing it.
pub fn parse_classifier_usage(value: Option<&Value>, model: &ClassifierModel) -> Option<Usage> {
    let object = value?.as_object()?;
    if !object.contains_key("input_tokens") && !object.contains_key("output_tokens") {
        return None;
    }
    let input = token_count(object.get("input_tokens"));
    let output = token_count(object.get("output_tokens"));
    let mut usage = Usage {
        input,
        output,
        cache_read: 0,
        cache_write: 0,
        cache_write1h: None,
        reasoning: None,
        total_tokens: input + output,
        cost: Default::default(),
    };
    calculate_cost_for(&model.cost, &mut usage);
    Some(usage)
}
