//! Port of `packages/ai/src/api/openrouter-images.ts` @ pi 0.82.1 (2efa728).
//!
//! OpenRouter image generation over the chat-completions endpoint
//! (`POST {baseUrl}/chat/completions`, **non-streaming**, `modalities:
//! ["image"] | ["image", "text"]`). Images come back as `data:` URLs in the
//! message's `images` array; the text part is the message content.
//!
//! Contract: **never rejects** — every failure (missing api key, transport
//! error, HTTP error, malformed response) is returned as an `AssistantImages`
//! with `stopReason: "error"` (or `"aborted"` when the signal fired) and a
//! formatted `errorMessage`. `openrouter-images.lazy.ts` is a code-splitting
//! shim; Rust links statically, so [`OpenRouterImages`] is always available
//! and no lazy-load error path exists (see D-036).
//!
//! Intentional differences (deviation D-036):
//! - reqwest direct instead of the `openai` SDK: no SDK user-agent /
//!   stainless telemetry headers, no SDK-level retries (the SDK's
//!   `maxRetries: 0` is already the default here; the shared
//!   `retry_provider_request` helper drives retries), timeout via a reqwest
//!   client timeout, cancellation via `CancellationToken` raced against the
//!   request send.
//! - Error text follows the crate's openai-completions convention (D-005):
//!   `"Request failed with status {status}: {body}"` composed via
//!   [`NormalizedProviderError`], instead of the SDK's parsed
//!   `error.message`.
//! - The `modalities`/`messages` request body is built as wire JSON
//!   (`serde_json::Value`), so `on_payload` sees exactly the wire shape.
//! - Response parsing is tolerant exactly where upstream reads fields
//!   unguarded (`id`/`usage`/`choices` optional; non-string `content` is
//!   skipped like upstream's `typeof` check); a wholly malformed body fails
//!   with the `serde_json` error text.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::images::images_models::{ProviderImages, now_ms};
use crate::types::{
    AssistantImages, ImageContent, ImageModel, ImagesContext, ImagesModel, ImagesOptions,
    ImagesOutputContent, ImagesOutputModality, ImagesStopReason, ProviderHeaders,
    ProviderImageGenerator, TextContent, Usage, UsageCost,
};
use crate::utils::custom_fetch::send_provider_request;
use crate::utils::error_body::{NormalizedProviderError, format_provider_error};
use crate::utils::headers::{headers_to_record, merge_headers, provider_headers_to_header_map};
use crate::utils::provider_retry::{
    ProviderErrorInfo, ProviderRetryOptions, retry_provider_request,
};
use crate::utils::sanitize_unicode::sanitize_surrogates;

/// The openrouter-images api implementation (unit struct; stateless).
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenRouterImages;

impl ProviderImages for OpenRouterImages {
    fn generate_images(
        &self,
        model: &ImagesModel,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
    ) -> Pin<Box<dyn Future<Output = AssistantImages> + Send + 'static>> {
        let model = model.clone();
        let context = context.clone();
        let options = options.cloned();
        Box::pin(async move { generate_images(&model, &context, options.as_ref()).await })
    }
}

/// `openrouterImagesApi()` (`api/openrouter-images.lazy.ts`): the `ProviderImages`
/// value handed to `createImagesProvider`. The lazy `import()` resolves to
/// the statically-linked adapter.
pub fn openrouter_images_api() -> Arc<dyn ProviderImages> {
    Arc::new(OpenRouterImages)
}

/// The schema-v6 unified runtime adapter (V16-07 FR-G): maps the unified
/// `ImageModel` onto the legacy subsystem model, then reuses the same wire
/// implementation. Auth/baseUrl/headers are applied by `Models::generate_images`
/// before the call.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenRouterImagesV6;

impl ProviderImageGenerator for OpenRouterImagesV6 {
    fn generate_images(
        &self,
        model: &ImageModel,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
    ) -> Pin<Box<dyn Future<Output = AssistantImages> + Send + 'static>> {
        let model = images_model_from_v6(model);
        let context = context.clone();
        let options = options.cloned();
        Box::pin(async move { generate_images(&model, &context, options.as_ref()).await })
    }
}

/// `openrouterImagesV6Api()` — the unified-runtime image implementation.
pub fn openrouter_images_v6_api() -> Arc<dyn ProviderImageGenerator> {
    Arc::new(OpenRouterImagesV6)
}

/// Maps a schema-v6 `ImageModel` onto the legacy subsystem's `ImagesModel`
/// (the wire implementation only reads these fields).
fn images_model_from_v6(model: &ImageModel) -> ImagesModel {
    ImagesModel {
        id: model.id.clone(),
        name: model.name.clone(),
        api: model.api.as_str().into(),
        provider: model.provider.clone(),
        base_url: model.base_url.clone(),
        input: model.input.clone(),
        output: model
            .output
            .iter()
            .map(|modality| match modality {
                crate::types::InputModality::Text => ImagesOutputModality::Text,
                crate::types::InputModality::Image => ImagesOutputModality::Image,
            })
            .collect(),
        cost: model.cost.clone(),
        headers: model.headers.clone(),
    }
}

/// `generateImages` (openrouter-images): never rejects.
pub async fn generate_images(
    model: &ImagesModel,
    context: &ImagesContext,
    options: Option<&ImagesOptions>,
) -> AssistantImages {
    let mut output = AssistantImages {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        output: Vec::new(),
        response_id: None,
        usage: None,
        stop_reason: ImagesStopReason::Stop,
        error_message: None,
        timestamp: now_ms(),
    };
    if let Err(message) = generate_images_inner(model, context, options, &mut output).await {
        let aborted = options
            .and_then(|options| options.signal.as_ref())
            .is_some_and(|signal| signal.is_cancelled());
        output.stop_reason = if aborted {
            ImagesStopReason::Aborted
        } else {
            ImagesStopReason::Error
        };
        output.error_message = Some(message);
    }
    output
}

async fn generate_images_inner(
    model: &ImagesModel,
    context: &ImagesContext,
    options: Option<&ImagesOptions>,
    output: &mut AssistantImages,
) -> Result<(), String> {
    let api_key = options.and_then(|options| options.api_key.clone());
    let Some(api_key) = api_key else {
        return Err(format!("No API key for provider: {}", model.provider));
    };

    let mut params = build_params(model, context);
    if let Some(on_payload) = options.and_then(|options| options.on_payload.as_ref())
        && let Some(next_params) = on_payload(params.clone(), model).await
    {
        params = next_params;
    }

    let url = format!("{}/chat/completions", model.base_url.trim_end_matches('/'));
    let header_map = build_client_headers(model, &api_key, options)?;
    let mut client_builder = crate::api::http_client::adapter_client_builder(
        options.and_then(|options| options.env.as_ref()),
        &url,
    )?;
    if let Some(timeout_ms) = options.and_then(|options| options.timeout_ms) {
        client_builder = client_builder.timeout(std::time::Duration::from_millis(timeout_ms));
    }
    let client = client_builder.build().map_err(|error| error.to_string())?;

    let response = retry_provider_request(
        || {
            let request = client.post(&url).headers(header_map.clone()).json(&params);
            let signal = options.and_then(|options| options.signal.clone());
            let fetch = options.and_then(|options| options.fetch.clone());
            async move {
                // 027a58479 (R2.7.4): per-request custom fetch channel; `None`
                // keeps the reqwest default path unchanged. Image generation
                // is a non-streaming one-shot POST, so the total client
                // timeout set above stays the right semantics; no extra
                // headers budget is needed.
                let result =
                    send_provider_request(request, fetch.as_ref(), signal.as_ref(), None).await;
                match result {
                    Ok(response) => {
                        let status = response.status();
                        if status.is_success() {
                            Ok(response)
                        } else {
                            let status = status.as_u16();
                            let response_headers = headers_to_record(response.headers());
                            let body = response.text().await.unwrap_or_default();
                            let normalized = NormalizedProviderError::new(
                                Some(status),
                                Some(body),
                                format!("Request failed with status {status}"),
                            );
                            Err(ProviderErrorInfo {
                                status: Some(status),
                                headers: Some(response_headers),
                                message: format_provider_error(&normalized, None),
                            })
                        }
                    }
                    Err(error) => Err(error.into_provider_error_info()),
                }
            }
        },
        ProviderRetryOptions {
            max_retries: options.and_then(|options| options.max_retries),
            max_retry_delay_ms: options.and_then(|options| options.max_retry_delay_ms),
            ..Default::default()
        },
        options.and_then(|options| options.signal.as_ref()),
    )
    .await
    .map_err(|error| error.message())?;

    if let Some(on_response) = options.and_then(|options| options.on_response.as_ref()) {
        on_response(
            crate::types::ProviderResponse {
                status: response.status().as_u16(),
                headers: headers_to_record(response.headers()),
            },
            model,
        )
        .await;
    }

    // P2 (rc.12 review): race the body read against the abort signal —
    // upstream hands the signal to fetch, so cancellation interrupts the
    // in-flight body read; a bare `.text().await` would otherwise stall
    // until the optional total timeout (unbounded when unset).
    let body_fut = response.text();
    let body = match options.and_then(|options| options.signal.as_ref()) {
        Some(signal) => {
            tokio::select! {
                body = body_fut => body,
                () = signal.cancelled() => {
                    return Err("Request was aborted".to_owned());
                }
            }
        }
        None => body_fut.await,
    }
    .map_err(|error| crate::utils::transport_error::format_reqwest_error(&error))?;
    let parsed: OpenRouterImageGenerationResponse =
        serde_json::from_str(&body).map_err(|error| error.to_string())?;

    output.response_id = parsed.id;
    if let Some(raw_usage) = &parsed.usage {
        output.usage = Some(parse_usage(raw_usage, model));
    }

    if let Some(choice) = parsed.choices.first() {
        if let Some(RawMessageContent::Text(content)) = &choice.message.content
            && !content.is_empty()
        {
            output.output.push(ImagesOutputContent::Text(TextContent {
                text: content.clone(),
                text_signature: None,
            }));
        }
        for image in &choice.message.images {
            let image_url = match &image.image_url {
                Some(RawImageUrl::Text(url)) => Some(url.clone()),
                Some(RawImageUrl::Object { url }) => url.clone(),
                None => None,
            };
            let Some(image_url) = image_url else { continue };
            if !image_url.starts_with("data:") {
                continue;
            }
            let Some((mime_type, data)) = parse_data_url(&image_url) else {
                continue;
            };
            output
                .output
                .push(ImagesOutputContent::Image(ImageContent { mime_type, data }));
        }
    }

    Ok(())
}

/// `createClient` default headers: the model's static headers overlaid by the
/// request headers (`{ ...model.headers, ...options.headers }`, `None`
/// suppresses), plus `Authorization`/`Content-Type` defaults the SDK would
/// send. Request headers win over the defaults (can override them).
fn build_client_headers(
    model: &ImagesModel,
    api_key: &str,
    options: Option<&ImagesOptions>,
) -> Result<reqwest::header::HeaderMap, String> {
    let model_headers: Option<ProviderHeaders> = model.headers.as_ref().map(|headers| {
        headers
            .iter()
            .map(|(key, value)| (key.clone(), Some(value.clone())))
            .collect()
    });
    let merged = merge_headers(
        model_headers.as_ref(),
        options.and_then(|o| o.headers.as_ref()),
    );
    let empty = ProviderHeaders::new();
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
    let request_headers = provider_headers_to_header_map(merged.as_ref().unwrap_or(&empty))?;
    for (name, value) in &request_headers {
        map.insert(name, value.clone());
    }
    Ok(map)
}

/// `buildParams`: `OpenRouterImagesCreateParams` as wire JSON — a single
/// `user` message whose content maps text → `{type:"text"}` and image →
/// `{type:"image_url", image_url:{url: "data:<mime>;base64,<data>"}}`,
/// `stream: false`, and `modalities: ["image"]` plus `"text"` when the model
/// supports text output.
fn build_params(model: &ImagesModel, context: &ImagesContext) -> Value {
    let content: Vec<Value> = context
        .input
        .iter()
        .map(|item| match item {
            crate::types::ImagesInputContent::Text(text) => {
                json!({ "type": "text", "text": sanitize_surrogates(&text.text) })
            }
            crate::types::ImagesInputContent::Image(image) => json!({
                "type": "image_url",
                "image_url": { "url": format!("data:{};base64,{}", image.mime_type, image.data) }
            }),
        })
        .collect();
    let modalities = if model.output.contains(&ImagesOutputModality::Text) {
        vec!["image", "text"]
    } else {
        vec!["image"]
    };
    json!({
        "model": model.id,
        "messages": [ { "role": "user", "content": content } ],
        "stream": false,
        "modalities": modalities,
    })
}

/// `parseDataUrl` equivalent of the upstream regex
/// `^data:([^;]+);base64,(.+)$` (both groups non-empty).
fn parse_data_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let (mime_type, tail) = rest.split_once(';')?;
    let data = tail.strip_prefix("base64,")?;
    if mime_type.is_empty() || data.is_empty() {
        return None;
    }
    Some((mime_type.to_owned(), data.to_owned()))
}

/// `OpenRouterGeneratedImage` — `image_url?: string | { url?: string }`.
#[derive(Debug, Deserialize)]
struct OpenRouterGeneratedImage {
    #[serde(default)]
    image_url: Option<RawImageUrl>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawImageUrl {
    Text(String),
    Object {
        #[serde(default)]
        url: Option<String>,
    },
}

/// `OpenRouterImageGenerationMessage` — chat message plus the OpenRouter
/// `images` array.
#[derive(Debug, Default, Deserialize)]
struct OpenRouterImageGenerationMessage {
    #[serde(default)]
    content: Option<RawMessageContent>,
    #[serde(default)]
    images: Vec<OpenRouterGeneratedImage>,
}

/// `content` may be a string or (per the OpenAI schema) an array; upstream
/// only reads the string case (`typeof content === "string"`), so non-string
/// content is captured but skipped.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawMessageContent {
    Text(String),
    #[allow(dead_code)]
    // captured so the untagged parse succeeds; skipped like upstream's `typeof` check
    Other(Value),
}

#[derive(Debug, Deserialize)]
struct OpenRouterImageGenerationChoice {
    #[serde(default)]
    message: OpenRouterImageGenerationMessage,
}

#[derive(Debug, Deserialize)]
struct OpenRouterImageGenerationResponse {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    usage: Option<RawUsage>,
    #[serde(default)]
    choices: Vec<OpenRouterImageGenerationChoice>,
}

/// `ChatCompletionUsage` subset with the OpenRouter cache fields.
#[derive(Debug, Deserialize)]
struct RawUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    prompt_tokens_details: Option<RawPromptTokensDetails>,
}

#[derive(Debug, Deserialize)]
struct RawPromptTokensDetails {
    #[serde(default)]
    cached_tokens: u64,
    #[serde(default)]
    cache_write_tokens: u64,
}

/// `parseUsage`: cache-read/write split and cost computation, ported
/// line-for-line (the JS `|| 0` defaults become serde defaults; the
/// saturating subtractions become `i64` arithmetic since the token types are
/// unsigned).
fn parse_usage(raw: &RawUsage, model: &ImagesModel) -> Usage {
    let prompt_tokens = raw.prompt_tokens;
    let reported_cached_tokens = raw
        .prompt_tokens_details
        .as_ref()
        .map(|details| details.cached_tokens)
        .unwrap_or(0);
    let cache_write_tokens = raw
        .prompt_tokens_details
        .as_ref()
        .map(|details| details.cache_write_tokens)
        .unwrap_or(0);
    let cache_read_tokens = if cache_write_tokens > 0 {
        reported_cached_tokens.saturating_sub(cache_write_tokens)
    } else {
        reported_cached_tokens
    };
    let input =
        (prompt_tokens as i64 - (cache_read_tokens + cache_write_tokens) as i64).max(0) as u64;
    let output = raw.completion_tokens;
    let rates = &model.cost.rates;
    let cost = UsageCost {
        input: (rates.input / 1_000_000.0) * input as f64,
        output: (rates.output / 1_000_000.0) * output as f64,
        cache_read: (rates.cache_read / 1_000_000.0) * cache_read_tokens as f64,
        cache_write: (rates.cache_write / 1_000_000.0) * cache_write_tokens as f64,
        total: 0.0,
    };
    let total_tokens = input + output + cache_read_tokens + cache_write_tokens;
    Usage {
        input,
        output,
        cache_read: cache_read_tokens,
        cache_write: cache_write_tokens,
        cache_write1h: None,
        reasoning: None,
        total_tokens,
        cost: UsageCost {
            total: cost.input + cost.output + cost.cache_read + cost.cache_write,
            ..cost
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        ImagesApiKind, ImagesInputContent, InputModality, ModelCost, ModelCostRates,
    };

    fn model(output: Vec<ImagesOutputModality>) -> ImagesModel {
        ImagesModel {
            id: "google/gemini-3.1-flash-image-preview".to_owned(),
            name: "Gemini 3.1 Flash Image Preview".to_owned(),
            api: ImagesApiKind::from(ImagesApiKind::OPENROUTER_IMAGES),
            provider: "openrouter".to_owned(),
            base_url: "https://openrouter.ai/api/v1".to_owned(),
            input: vec![InputModality::Text, InputModality::Image],
            output,
            cost: ModelCost {
                rates: ModelCostRates {
                    input: 0.5,
                    output: 3.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                },
                tiers: None,
            },
            headers: None,
        }
    }

    #[test]
    fn build_params_text_only_input() {
        let model = model(vec![
            ImagesOutputModality::Image,
            ImagesOutputModality::Text,
        ]);
        let context = ImagesContext {
            input: vec![ImagesInputContent::Text(TextContent {
                text: "Generate a dog".to_owned(),
                text_signature: None,
            })],
        };
        let params = build_params(&model, &context);
        assert_eq!(params["stream"], json!(false));
        assert_eq!(
            params["model"],
            json!("google/gemini-3.1-flash-image-preview")
        );
        assert_eq!(params["modalities"], json!(["image", "text"]));
        assert_eq!(params["messages"][0]["role"], json!("user"));
        assert_eq!(
            params["messages"][0]["content"][0],
            json!({ "type": "text", "text": "Generate a dog" })
        );
    }

    #[test]
    fn build_params_image_input_and_image_only_output() {
        let model = model(vec![ImagesOutputModality::Image]);
        let context = ImagesContext {
            input: vec![ImagesInputContent::Image(ImageContent {
                data: "ZmFrZS1wbmc=".to_owned(),
                mime_type: "image/png".to_owned(),
            })],
        };
        let params = build_params(&model, &context);
        assert_eq!(params["modalities"], json!(["image"]));
        assert_eq!(
            params["messages"][0]["content"][0],
            json!({
                "type": "image_url",
                "image_url": { "url": "data:image/png;base64,ZmFrZS1wbmc=" }
            })
        );
    }

    #[test]
    fn parse_data_url_cases() {
        assert_eq!(
            parse_data_url("data:image/png;base64,ZmFrZS1wbmc="),
            Some(("image/png".to_owned(), "ZmFrZS1wbmc=".to_owned()))
        );
        // Non-data URLs and malformed data URLs are skipped (upstream regex).
        assert_eq!(parse_data_url("https://example.com/x.png"), None);
        assert_eq!(parse_data_url("data:;base64,abc"), None);
        assert_eq!(parse_data_url("data:image/png;base64,"), None);
        assert_eq!(parse_data_url("data:image/png;other,abc"), None);
    }

    fn raw_usage(prompt: u64, completion: u64, cached: u64, cache_write: u64) -> RawUsage {
        RawUsage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            prompt_tokens_details: Some(RawPromptTokensDetails {
                cached_tokens: cached,
                cache_write_tokens: cache_write,
            }),
        }
    }

    #[test]
    fn parse_usage_plain() {
        // Upstream test mock: prompt 12, completion 34, cached 0.
        let model = model(vec![ImagesOutputModality::Image]);
        let usage = parse_usage(&raw_usage(12, 34, 0, 0), &model);
        assert_eq!(usage.input, 12);
        assert_eq!(usage.output, 34);
        assert_eq!(usage.cache_read, 0);
        assert_eq!(usage.cache_write, 0);
        assert_eq!(usage.total_tokens, 46);
        // 0.5/1e6 * 12 + 3.0/1e6 * 34
        let expected = 0.5 / 1_000_000.0 * 12.0 + 3.0 / 1_000_000.0 * 34.0;
        assert!((usage.cost.total - expected).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_usage_cache_split() {
        let model = model(vec![ImagesOutputModality::Image]);
        // reported cached 1000, cache_write 300 -> cacheRead 700; prompt
        // tokens 1200 -> input 200.
        let usage = parse_usage(&raw_usage(1200, 50, 1000, 300), &model);
        assert_eq!(usage.input, 200);
        assert_eq!(usage.cache_read, 700);
        assert_eq!(usage.cache_write, 300);
        assert_eq!(usage.total_tokens, 1250);
        // cacheWrite > prompt leftovers saturate to 0 input.
        let usage = parse_usage(&raw_usage(100, 50, 400, 300), &model);
        assert_eq!(usage.input, 0);
        assert_eq!(usage.cache_read, 100);
        assert_eq!(usage.cache_write, 300);
    }
}
