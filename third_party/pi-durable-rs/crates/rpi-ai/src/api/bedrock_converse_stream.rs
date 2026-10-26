//! Port of `packages/ai/src/api/bedrock-converse-stream.ts` @ pi 0.82.1 (2efa728).
//! (`bedrock-converse-stream.lazy.ts` is the upstream dynamic-import wrapper;
//! rpi adapters are linked statically, so there is no lazy counterpart.)
//! Stream-termination semantics (rawStopReason, unknown-reason error text)
//! updated to 4181f66 (637737ca7, 5a2539a7b); failure diagnostics
//! (`bedrock_response_failure`) added at 4181f66 (70bbe47a9, #7286).
//!
//! Amazon Bedrock ConverseStream adapter: SigV4 vs bearer-token auth, region /
//! endpoint resolution, the reserved-header whitelist for caller headers,
//! cachePoint injection (1h TTL on long retention), adaptive vs budget-based
//! thinking for Claude model families, message/tool conversion with the
//! `<empty>` placeholder, and AWS event-stream decoding into [`StreamEvent`]s.
//!
//! Transport layer reversed from the pinned SDK sources (the TS adapter
//! delegates to `@aws-sdk/client-bedrock-runtime` + `@smithy`, so the wire
//! details are not observable in upstream TS; design §14 source-blank note):
//! - `POST {endpoint}/model/{extendedEncodeURIComponent(modelId)}/converse-stream`,
//!   `content-type: application/json`, rest-json1 body (camelCase, schema
//!   member order), from `@aws-sdk/client-bedrock-runtime` schemas_0.js +
//!   `@smithy/core` `HttpBindingProtocol` / `@aws-sdk/core` `AwsRestJsonProtocol`.
//! - SigV4 (`service = "bedrock"`) hand-written per `@smithy/signature-v4`
//!   (see `bedrock/sigv4.rs`); bearer token sends `Authorization: Bearer`
//!   with no SigV4 headers.
//! - Response `application/vnd.amazon.eventstream` frames decoded per
//!   `@smithy/core` eventstream-codec (see `bedrock/event_stream.rs`).
//!
//! Intentional differences (upstream deviations, D-026):
//! - HTTP is a direct reqwest call, not the SDK: `amz-sdk-invocation-id` /
//!   `amz-sdk-request` / SDK `user-agent` headers are not sent, and retries
//!   use the shared `retry_provider_request` helper driven by
//!   `StreamOptions::max_retries`, defaulting to 2 extra attempts when unset
//!   (the SDK standard-mode default; upstream ignores `maxRetries` for
//!   Bedrock — rpi honors an explicit value).
//! - The AWS credential chain is env-only: `AWS_ACCESS_KEY_ID` /
//!   `AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN` (plus `AWS_BEDROCK_SKIP_AUTH`
//!   dummy credentials and the bearer token). Profile files, SSO, IMDS and
//!   `~/.aws/config` are not consulted; `BedrockOptions::profile` is accepted
//!   for parity but inert. When no region resolves (the case upstream defers
//!   to the SDK profile chain) rpi falls back to `us-east-1`, matching
//!   upstream's own non-Node fallback branch. Likewise the SDK endpoint
//!   ruleset collapses to `bedrock-runtime.{region}.amazonaws.com`
//!   (`.amazonaws.com.cn` for `cn-*`); FIPS/dualstack variants are not
//!   derived.
//! - HTTP(S) proxy agent configuration and `AWS_BEDROCK_FORCE_HTTP1` are
//!   Node-specific SDK request-handler knobs and are not ported.
//! - `on_payload` sees the wire (camelCase rest-json1) command input
//!   including `modelId`; `modelId` is consumed as the path label and
//!   stripped from the body (the SDK would take it from the command input
//!   after `onPayload`, so replacing it via `on_payload` still works).
//! - HTTP error details come from the raw response: the exception name is
//!   read from `x-amzn-errortype` / body `code` / `__type`
//!   (`loadRestJsonErrorCode` order) and the message from the parsed body
//!   instead of an SDK exception object; `normalizeProviderError`'s
//!   `$metadata` probing has no reqwest analogue.
//! - Image blocks pass the base64 payload through (upstream `atob`s it and
//!   the SDK re-encodes — identity on the wire; invalid base64 now fails
//!   server-side instead of at `atob`).

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use regex::Regex;
use serde_json::{Map, Value, json};

use crate::api::anthropic_messages::resolve_cache_retention;
use crate::api::bedrock::event_stream::{EventStreamDecoder, EventStreamMessage};
use crate::api::bedrock::sigv4::{self, SigV4Credentials, extended_encode_uri_component};
use crate::api::constrained_sampling::{
    get_json_schema_tool_parameters, resolve_json_schema_strict_sampling,
};
use crate::api::simple_options::{
    adjust_max_tokens_for_thinking, build_base_options, clamp_max_tokens_to_context,
    clamp_reasoning,
};
use crate::api::stream_cancel::{StreamNext, next_chunk_or_cancelled};
use crate::models::ProviderStreams;
use crate::types::{
    AssistantContent, AssistantMessage, AssistantMessageDiagnostic, AssistantRole, CacheRetention,
    DoneReason, ErrorReason, Message, Model, ProviderEnv, ProviderHeaders, ProviderResponse,
    SimpleStreamOptions, StopReason, StreamEvent, StreamOptions, ThinkingBudgets, ThinkingLevel,
    Tool, ToolResultContent, TranscriptContext, Usage, UserContent, UserContentBlock,
};
use crate::utils::cost::calculate_cost;
use crate::utils::error_body::NormalizedProviderError;
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::headers::{headers_to_record, provider_headers_to_header_map};
use crate::utils::json_parse::parse_streaming_json;
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::provider_retry::{
    ProviderErrorInfo, ProviderRetryOptions, retry_provider_request,
};
use crate::utils::sanitize_unicode::sanitize_surrogates;
use crate::utils::transform_messages::transform_messages;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// `EMPTY_TEXT_PLACEHOLDER` (bedrock-converse-stream.ts:104).
pub const EMPTY_TEXT_PLACEHOLDER: &str = "<empty>";

/// `REDACTED_THINKING_PLACEHOLDER` — matches the placeholder the Anthropic
/// API path uses for redacted thinking (bedrock-converse-stream.ts:106,
/// `d57e531f5` / #8314).
const REDACTED_THINKING_PLACEHOLDER: &str = "[Reasoning redacted]";

/// `BEDROCK_DATA_RETENTION_DOCS_URL`.
const BEDROCK_DATA_RETENTION_DOCS_URL: &str =
    "https://docs.aws.amazon.com/bedrock/latest/userguide/data-retention.html";

/// `BEDROCK_ERROR_PREFIXES`: human-readable prefixes for SDK exception names.
/// Downstream retry/context-overflow logic string-matches these prefixes, so
/// they are preserved verbatim.
const BEDROCK_ERROR_PREFIXES: [(&str, &str); 5] = [
    ("InternalServerException", "Internal server error"),
    ("ModelStreamErrorException", "Model stream error"),
    ("ValidationException", "Validation error"),
    ("ThrottlingException", "Throttling error"),
    ("ServiceUnavailableException", "Service unavailable"),
];

/// SigV4 signing service (`signingName` in the bedrock-runtime runtime config).
const SIGNING_SERVICE: &str = "bedrock";

static DATA_RETENTION_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("(?i)data retention mode").expect("static data-retention pattern must compile")
});

static ARN_REGION_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("^arn:aws(?:-[a-z0-9-]+)?:bedrock:([a-z0-9-]+):")
        .expect("static ARN region pattern must compile")
});

static STANDARD_ENDPOINT_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("^bedrock-runtime(?:-fips)?\\.([a-z0-9-]+)\\.amazonaws\\.com(?:\\.cn)?$")
        .expect("static endpoint pattern must compile")
});

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// `BedrockThinkingDisplay = "summarized" | "omitted"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BedrockThinkingDisplay {
    Summarized,
    Omitted,
}

impl BedrockThinkingDisplay {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Summarized => "summarized",
            Self::Omitted => "omitted",
        }
    }
}

/// `BedrockOptions.toolChoice`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BedrockToolChoice {
    Auto,
    Any,
    None,
    /// `{ type: "tool", name }` — force a specific tool.
    Tool {
        name: String,
    },
}

/// `BedrockOptions` — `StreamOptions` plus Bedrock-specific extras.
///
/// `profile` is accepted for upstream parity but inert: rpi's credential
/// chain is env-only (see module header, D-026).
#[derive(Debug, Clone, Default)]
pub struct BedrockOptions {
    pub stream: StreamOptions,
    pub region: Option<String>,
    pub profile: Option<String>,
    pub tool_choice: Option<BedrockToolChoice>,
    pub reasoning: Option<ThinkingLevel>,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub interleaved_thinking: Option<bool>,
    pub thinking_display: Option<BedrockThinkingDisplay>,
    /// `requestMetadata`: key-value pairs attached for cost allocation
    /// tagging, passed through to the wire.
    pub request_metadata: Option<Map<String, Value>>,
    /// Bearer token for Bedrock API key authentication; bypasses SigV4.
    pub bearer_token: Option<String>,
}

// ---------------------------------------------------------------------------
// Client config resolution (the SDK `BedrockRuntimeClientConfig` analogue)
// ---------------------------------------------------------------------------

/// The resolved equivalent of the upstream `BedrockRuntimeClientConfig`:
/// which region to sign for, which endpoint to call, and which auth to use.
/// `region == None` mirrors upstream deferring to the SDK default chain
/// (ambient profile); rpi then falls back to `us-east-1`
/// ([`BedrockClientConfig::effective_region`]).
#[derive(Debug, Clone, Default)]
pub struct BedrockClientConfig {
    pub region: Option<String>,
    /// The base URL to call: `model.base_url` when the endpoint is pinned
    /// (`useExplicitEndpoint`), otherwise derived from the region.
    pub endpoint: Option<String>,
    pub profile: Option<String>,
    pub credentials: Option<SigV4Credentials>,
    pub bearer_token: Option<String>,
}

impl BedrockClientConfig {
    /// The region actually used for endpoint derivation and SigV4.
    pub fn effective_region(&self) -> String {
        self.region
            .clone()
            .unwrap_or_else(|| "us-east-1".to_owned())
    }
}

/// `getConfiguredBedrockRegion`: explicit option > `AWS_REGION` >
/// `AWS_DEFAULT_REGION`.
pub fn get_configured_bedrock_region(options: &BedrockOptions) -> Option<String> {
    options
        .region
        .clone()
        .filter(|region| !region.is_empty())
        .or_else(|| get_provider_env_value("AWS_REGION", options.stream.env.as_ref()))
        .or_else(|| get_provider_env_value("AWS_DEFAULT_REGION", options.stream.env.as_ref()))
}

/// `getConfiguredBedrockCredentials`: static env credentials.
fn get_configured_bedrock_credentials(env: Option<&ProviderEnv>) -> Option<SigV4Credentials> {
    let access_key_id = get_provider_env_value("AWS_ACCESS_KEY_ID", env)?;
    let secret_access_key = get_provider_env_value("AWS_SECRET_ACCESS_KEY", env)?;
    Some(SigV4Credentials {
        access_key_id,
        secret_access_key,
        session_token: get_provider_env_value("AWS_SESSION_TOKEN", env),
    })
}

/// `getStandardBedrockEndpointRegion`: extracts the region from a standard
/// `bedrock-runtime[.fips].{region}.amazonaws.com[.cn]` hostname; custom
/// (VPC/proxy) endpoints yield `None`.
pub fn get_standard_bedrock_endpoint_region(base_url: &str) -> Option<String> {
    let url = url::Url::parse(base_url).ok()?;
    let hostname = url.host_str()?.to_lowercase();
    STANDARD_ENDPOINT_PATTERN
        .captures(&hostname)
        .and_then(|captures| captures.get(1).map(|m| m.as_str().to_owned()))
}

/// `shouldUseExplicitBedrockEndpoint`: custom endpoints are always pinned;
/// standard AWS endpoints are pinned only when no region and no ambient
/// `AWS_PROFILE` is configured (preserving catalog defaults such as
/// `us-east-1` from overriding `AWS_REGION` / `AWS_PROFILE`, upstream #3402).
pub fn should_use_explicit_bedrock_endpoint(
    base_url: &str,
    configured_region: Option<&str>,
    has_ambient_configured_profile: bool,
) -> bool {
    match get_standard_bedrock_endpoint_region(base_url) {
        None => true,
        Some(_) => configured_region.is_none() && !has_ambient_configured_profile,
    }
}

/// The region-selection part of the upstream `stream` setup. Region
/// resolution order: ARN-embedded > explicit option / env vars > standard
/// endpoint (when pinned) > `us-east-1` when no ambient profile; `None`
/// defers to the ambient profile (SDK chain upstream, `us-east-1` in rpi).
pub fn resolve_bedrock_config(model: &Model, options: &BedrockOptions) -> BedrockClientConfig {
    let env = options.stream.env.as_ref();
    let profile = options
        .profile
        .clone()
        .or_else(|| get_provider_env_value("AWS_PROFILE", env));
    // Upstream checks the *process* env only for the ambient profile
    // (`getProviderEnvValue("AWS_PROFILE")` without the options env).
    let has_ambient_configured_profile = get_provider_env_value("AWS_PROFILE", None).is_some();
    let configured_region = get_configured_bedrock_region(options);
    let endpoint_region = get_standard_bedrock_endpoint_region(&model.base_url);
    let use_explicit_endpoint = should_use_explicit_bedrock_endpoint(
        &model.base_url,
        configured_region.as_deref(),
        has_ambient_configured_profile,
    );

    let arn_region = ARN_REGION_PATTERN
        .captures(&model.id)
        .and_then(|captures| captures.get(1).map(|m| m.as_str().to_owned()));
    let region = if let Some(arn_region) = arn_region {
        Some(arn_region)
    } else if configured_region.is_some() {
        configured_region
    } else if endpoint_region.is_some() && use_explicit_endpoint {
        endpoint_region
    } else if !has_ambient_configured_profile {
        Some("us-east-1".to_owned())
    } else {
        None
    };

    // `AWS_BEDROCK_SKIP_AUTH=1`: dummy credentials so unsigned proxies work.
    let skip_auth = get_provider_env_value("AWS_BEDROCK_SKIP_AUTH", env).as_deref() == Some("1");
    let bearer_token = options
        .bearer_token
        .clone()
        .or_else(|| options.stream.api_key.clone())
        .or_else(|| get_provider_env_value("AWS_BEARER_TOKEN_BEDROCK", env))
        .filter(|_| !skip_auth);

    let credentials = if bearer_token.is_some() {
        None
    } else if skip_auth {
        Some(SigV4Credentials {
            access_key_id: "dummy-access-key".to_owned(),
            secret_access_key: "dummy-secret-key".to_owned(),
            session_token: None,
        })
    } else {
        get_configured_bedrock_credentials(env)
    };

    BedrockClientConfig {
        region,
        endpoint: use_explicit_endpoint.then(|| model.base_url.clone()),
        profile,
        credentials,
        bearer_token,
    }
}

/// The endpoint used when the base URL is not pinned: the SDK endpoint
/// ruleset collapsed to the regional standard hostname (deviation, D-026).
fn derive_regional_endpoint(region: &str) -> String {
    let suffix = if region.starts_with("cn-") {
        "amazonaws.com.cn"
    } else {
        "amazonaws.com"
    };
    format!("https://bedrock-runtime.{region}.{suffix}")
}

// ---------------------------------------------------------------------------
// Reserved header whitelist
// ---------------------------------------------------------------------------

/// `isReservedHeader`: `x-amz-*`, `authorization` and `host` participate in
/// SigV4 / are owned by the auth path and are silently skipped for
/// caller-supplied headers. Compared case-insensitively.
pub fn is_reserved_header(key: &str) -> bool {
    let lower = key.to_lowercase();
    lower.starts_with("x-amz-") || lower == "authorization" || lower == "host"
}

/// The custom-headers middleware effect (`addCustomHeadersMiddleware`):
/// caller headers minus reserved ones, `None` suppression markers dropped
/// (`providerHeadersToRecord` drops undefined values).
pub fn filtered_custom_headers(headers: Option<&ProviderHeaders>) -> Vec<(String, String)> {
    headers
        .into_iter()
        .flatten()
        .filter(|(key, _)| !is_reserved_header(key))
        .filter_map(|(key, value)| value.clone().map(|value| (key.clone(), value)))
        .collect()
}

// ---------------------------------------------------------------------------
// Model family detection
// ---------------------------------------------------------------------------

/// `getModelMatchCandidates`: model id and name, each lowercased and with
/// `[\s_.:]+` runs collapsed to `-` (inference-profile ARNs may lack the
/// model name, so both fields are checked).
pub fn get_model_match_candidates(model_id: &str, model_name: &str) -> Vec<String> {
    static SEPARATOR_PATTERN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[\s_.:]+").expect("static separator pattern must compile"));
    [model_id, model_name]
        .into_iter()
        .flat_map(|value| {
            let lower = value.to_lowercase();
            let normalized = SEPARATOR_PATTERN.replace_all(&lower, "-").into_owned();
            [lower, normalized]
        })
        .collect()
}

/// `supportsAdaptiveThinking` (Opus 4.6+, Sonnet 4.6, Claude 5 families).
pub fn supports_adaptive_thinking(model_id: &str, model_name: &str) -> bool {
    get_model_match_candidates(model_id, model_name)
        .iter()
        .any(|s| {
            s.contains("opus-4-6")
                || s.contains("opus-4-7")
                || s.contains("opus-4-8")
                || s.contains("opus-5")
                || s.contains("sonnet-4-6")
                || s.contains("sonnet-5")
                || s.contains("haiku-5")
                || s.contains("fable-5")
        })
}

/// `supportsNativeXhighEffort`: the adaptive families minus Opus 4.6 and
/// Sonnet 4.6.
pub fn supports_native_xhigh_effort(model: &Model) -> bool {
    get_model_match_candidates(&model.id, &model.name)
        .iter()
        .any(|s| {
            s.contains("opus-4-7")
                || s.contains("opus-4-8")
                || s.contains("opus-5")
                || s.contains("sonnet-5")
                || s.contains("haiku-5")
                || s.contains("fable-5")
        })
}

/// `isAnthropicClaudeModel`: id/name contains `anthropic.claude` /
/// `anthropic/claude`, or the display name mentions `claude`.
pub fn is_anthropic_claude_model(model: &Model) -> bool {
    let id = model.id.to_lowercase();
    let name = model.name.to_lowercase();
    id.contains("anthropic.claude")
        || id.contains("anthropic/claude")
        || name.contains("anthropic.claude")
        || name.contains("anthropic/claude")
        || name.contains("claude")
}

/// `supportsPromptCaching`: Claude 3.5 Haiku, 3.7 Sonnet, 4.x and 5 models.
/// `AWS_BEDROCK_FORCE_CACHE=1` forces cache points for inference profiles
/// whose ARNs don't carry the model name.
pub fn supports_prompt_caching(model: &Model, env: Option<&ProviderEnv>) -> bool {
    let candidates = get_model_match_candidates(&model.id, &model.name);
    if !candidates.iter().any(|s| s.contains("claude")) {
        return get_provider_env_value("AWS_BEDROCK_FORCE_CACHE", env).as_deref() == Some("1");
    }
    // Claude 5 (fable-5, opus-5, sonnet-5, haiku-5).
    if candidates.iter().any(|s| {
        s.contains("fable-5")
            || s.contains("opus-5")
            || s.contains("sonnet-5")
            || s.contains("haiku-5")
    }) {
        return true;
    }
    // Claude 4.x (opus-4, sonnet-4, haiku-4).
    if candidates.iter().any(|s| s.contains("-4-")) {
        return true;
    }
    candidates
        .iter()
        .any(|s| s.contains("claude-3-7-sonnet") || s.contains("claude-3-5-haiku"))
}

/// `supportsThinkingSignature`: only Anthropic Claude models accept the
/// `reasoningContent.reasoningText.signature` field.
fn supports_thinking_signature(model: &Model) -> bool {
    is_anthropic_claude_model(model)
}

/// `isGovCloudBedrockTarget`: GovCloud rejects the `thinking.display` field.
pub fn is_gov_cloud_bedrock_target(model: &Model, options: &BedrockOptions) -> bool {
    if let Some(region) = get_configured_bedrock_region(options)
        && region.to_lowercase().starts_with("us-gov-")
    {
        return true;
    }
    let id = model.id.to_lowercase();
    id.starts_with("us-gov.") || id.starts_with("arn:aws-us-gov:")
}

/// `mapThinkingLevelToEffort`: native xhigh for supporting families, then the
/// model's `thinkingLevelMap` value verbatim, then the default ladder.
pub fn map_thinking_level_to_effort(model: &Model, level: Option<ThinkingLevel>) -> String {
    if level == Some(ThinkingLevel::Xhigh) && supports_native_xhigh_effort(model) {
        return "xhigh".to_owned();
    }
    if let Some(level) = level
        && let Some(Some(mapped)) = model
            .thinking_level_map
            .as_ref()
            .and_then(|map| map.get(&level.to_model_level()))
    {
        return mapped.clone();
    }
    match level {
        Some(ThinkingLevel::Minimal) | Some(ThinkingLevel::Low) => "low".to_owned(),
        Some(ThinkingLevel::Medium) => "medium".to_owned(),
        _ => "high".to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Payload construction
// ---------------------------------------------------------------------------

/// `normalizeToolCallId`: sanitize to `[a-zA-Z0-9_-]`, truncate at 64.
pub fn normalize_tool_call_id(id: &str) -> String {
    let sanitized: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    sanitized.chars().take(64).collect()
}

/// `createImageBlock` wire shape. The base64 payload passes through
/// (upstream decodes and the SDK re-encodes; see module header).
fn create_image_block(mime_type: &str, data: &str) -> Result<Value, String> {
    let format = match mime_type {
        "image/jpeg" | "image/jpg" => "jpeg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        other => return Err(format!("Unknown image type: {other}")),
    };
    Ok(json!({"format": format, "source": {"bytes": data}}))
}

/// `createNonBlankTextBlock`: blank (whitespace-only) text drops out.
fn create_non_blank_text_block(text: &str) -> Option<Value> {
    let sanitized = sanitize_surrogates(text);
    if sanitized.trim().is_empty() {
        None
    } else {
        Some(json!({"text": sanitized}))
    }
}

/// `sanitizeBedrockDocument` (`98145a6c0`, :900-908): recursively drop
/// empty-string keys — AWS rejects them — preserving arrays, nested values
/// and key order.
fn sanitize_bedrock_document(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(sanitize_bedrock_document).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(key, _)| !key.is_empty())
                .map(|(key, nested)| (key.clone(), sanitize_bedrock_document(nested)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// `decodeRedactedContent` (:1304-1311): the persisted signature carries the
/// encrypted reasoning as base64. A hand-edited or externally produced
/// session can hold a signature that is not base64; drop that block instead
/// of failing the whole request. Returns the canonical base64 re-encoding
/// (the wire shape for `redactedContent`).
fn decode_redacted_content(signature: Option<&str>) -> Option<String> {
    use base64::Engine as _;
    let signature = signature?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(signature)
        .ok()?;
    Some(base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// `createRequiredTextBlock`: blank text becomes the `<empty>` placeholder.
fn create_required_text_block(text: &str) -> Value {
    create_non_blank_text_block(text).unwrap_or_else(|| json!({"text": EMPTY_TEXT_PLACEHOLDER}))
}

/// `convertToolResultContent`: blank text drops out; an empty result list
/// yields the `<empty>` placeholder.
fn convert_tool_result_content(content: &[ToolResultContent]) -> Result<Vec<Value>, String> {
    let mut result = Vec::new();
    for block in content {
        match block {
            ToolResultContent::Image(image) => {
                result.push(json!({"image": create_image_block(&image.mime_type, &image.data)?}));
            }
            ToolResultContent::Text(text) => {
                if let Some(text_block) = create_non_blank_text_block(&text.text) {
                    result.push(text_block);
                }
            }
        }
    }
    if result.is_empty() {
        result.push(json!({"text": EMPTY_TEXT_PLACEHOLDER}));
    }
    Ok(result)
}

/// `cachePoint` block for the resolved retention (`ttl: "1h"` on long).
fn cache_point_block(cache_retention: CacheRetention) -> Value {
    let mut block = json!({"cachePoint": {"type": "default"}});
    if cache_retention == CacheRetention::Long {
        block["cachePoint"]["ttl"] = json!("1h");
    }
    block
}

/// `buildSystemPrompt`: `None`/empty system prompt yields no system blocks;
/// supported Claude models get a trailing cache point.
pub fn build_system_prompt(
    system_prompt: Option<&str>,
    model: &Model,
    cache_retention: CacheRetention,
    env: Option<&ProviderEnv>,
) -> Option<Vec<Value>> {
    let system_prompt = system_prompt.filter(|prompt| !prompt.is_empty())?;
    let mut blocks = vec![json!({"text": sanitize_surrogates(system_prompt)})];
    if cache_retention != CacheRetention::None && supports_prompt_caching(model, env) {
        blocks.push(cache_point_block(cache_retention));
    }
    Some(blocks)
}

/// `convertMessages`: user / assistant / toolResult conversion with tool
/// result grouping and the `<empty>` placeholder rules. The leading system
/// message is excluded — Bedrock has no mid-conversation system messages
/// and the prompt rides the top-level `system` field (#9548).
pub fn convert_messages(
    context: &TranscriptContext,
    model: &Model,
    cache_retention: CacheRetention,
    env: Option<&ProviderEnv>,
) -> Result<Vec<Value>, String> {
    let mut result: Vec<Value> = Vec::new();
    let mut normalize =
        |id: &str, _model: &Model, _msg: &AssistantMessage| normalize_tool_call_id(id);
    let conversation = crate::utils::transcript::without_initial_system_message(&context.messages);
    let transformed_messages = transform_messages(conversation, model, Some(&mut normalize));

    let mut i = 0;
    while i < transformed_messages.len() {
        match &transformed_messages[i] {
            // Unreachable: the leading system message was excluded and later
            // ones collapsed before conversion (#9548).
            Message::System(_) => {}
            Message::User(user) => {
                let mut content: Vec<Value> = Vec::new();
                match &user.content {
                    UserContent::Text(text) => content.push(create_required_text_block(text)),
                    UserContent::Blocks(blocks) => {
                        for block in blocks {
                            match block {
                                UserContentBlock::Text(text) => {
                                    if let Some(text_block) = create_non_blank_text_block(&text.text)
                                    {
                                        content.push(text_block);
                                    }
                                }
                                UserContentBlock::Image(image) => content.push(
                                    json!({"image": create_image_block(&image.mime_type, &image.data)?}),
                                ),
                            }
                        }
                        if content.is_empty() {
                            content.push(json!({"text": EMPTY_TEXT_PLACEHOLDER}));
                        }
                    }
                }
                result.push(json!({"role": "user", "content": content}));
            }
            Message::Assistant(assistant) => {
                // Bedrock rejects messages with empty content arrays (e.g.
                // from aborted requests): skip them.
                if assistant.content.is_empty() {
                    i += 1;
                    continue;
                }
                let mut content_blocks: Vec<Value> = Vec::new();
                for block in &assistant.content {
                    match block {
                        AssistantContent::Text(text) => {
                            if let Some(text_block) = create_non_blank_text_block(&text.text) {
                                content_blocks.push(text_block);
                            }
                        }
                        AssistantContent::ToolCall(call) => {
                            content_blocks.push(json!({
                                "toolUse": {
                                    "toolUseId": call.id,
                                    "name": call.name,
                                    // `sanitizeBedrockDocument` (`98145a6c0`): AWS
                                    // rejects document values with empty-string
                                    // keys; strip them recursively, preserving
                                    // every other nested value.
                                    "input": sanitize_bedrock_document(&Value::Object(
                                        call.arguments.clone(),
                                    )),
                                },
                            }));
                        }
                        AssistantContent::Thinking(thinking) => {
                            // Encrypted reasoning is opaque: replay the stored
                            // payload as the `redactedContent` member instead of
                            // lowering it to reasoning text (`d57e531f5` /
                            // #8314, bedrock-converse-stream.ts:988-996).
                            if thinking.redacted.unwrap_or(false) {
                                if let Some(redacted_content) =
                                    decode_redacted_content(thinking.thinking_signature.as_deref())
                                    && !redacted_content.is_empty()
                                {
                                    content_blocks.push(json!({
                                        "reasoningContent": {
                                            "redactedContent": redacted_content,
                                        },
                                    }));
                                }
                                continue;
                            }
                            let thinking_text = sanitize_surrogates(&thinking.thinking);
                            if thinking_text.trim().is_empty() {
                                continue;
                            }
                            if supports_thinking_signature(model) {
                                // Signatures arrive after thinking deltas; a
                                // partial or externally persisted message
                                // lacking one is rejected by Bedrock on
                                // replay. Fall back to plain text, matching
                                // Anthropic.
                                let signature = thinking
                                    .thinking_signature
                                    .as_deref()
                                    .unwrap_or("")
                                    .trim()
                                    .to_owned();
                                if signature.is_empty() {
                                    content_blocks.push(json!({"text": thinking_text}));
                                } else {
                                    content_blocks.push(json!({
                                        "reasoningContent": {
                                            "reasoningText": {
                                                "text": thinking_text,
                                                "signature": signature,
                                            },
                                        },
                                    }));
                                }
                            } else {
                                content_blocks.push(json!({
                                    "reasoningContent": {
                                        "reasoningText": { "text": thinking_text },
                                    },
                                }));
                            }
                        }
                    }
                }
                if content_blocks.is_empty() {
                    i += 1;
                    continue;
                }
                result.push(json!({"role": "assistant", "content": content_blocks}));
            }
            Message::ToolResult(_) => {
                // Collect all consecutive toolResult messages into a single
                // user message (Bedrock requires them grouped).
                let mut tool_results: Vec<Value> = Vec::new();
                let mut j = i;
                while j < transformed_messages.len() {
                    let Message::ToolResult(next) = &transformed_messages[j] else {
                        break;
                    };
                    tool_results.push(json!({
                        "toolResult": {
                            "toolUseId": next.tool_call_id,
                            "content": convert_tool_result_content(&next.content)?,
                            "status": if next.is_error { "error" } else { "success" },
                        },
                    }));
                    j += 1;
                }
                i = j - 1;
                result.push(json!({"role": "user", "content": tool_results}));
            }
        }
        i += 1;
    }

    // Cache point on the last user message for supported Claude models.
    if cache_retention != CacheRetention::None
        && supports_prompt_caching(model, env)
        && !result.is_empty()
        && let Some(last) = result.last_mut()
        && last.get("role").and_then(Value::as_str) == Some("user")
        && let Some(content) = last.get_mut("content").and_then(Value::as_array_mut)
    {
        content.push(cache_point_block(cache_retention));
    }

    Ok(result)
}

/// `convertToolConfig`: tool schemas with strict-mode gating; `None` tools or
/// `toolChoice: "none"` yield no tool config.
pub fn convert_tool_config(
    tools: Option<&[Tool]>,
    tool_choice: Option<&BedrockToolChoice>,
    supports_strict_mode: bool,
) -> Result<Option<Value>, String> {
    let Some(tools) = tools.filter(|tools| !tools.is_empty()) else {
        return Ok(None);
    };
    if tool_choice == Some(&BedrockToolChoice::None) {
        return Ok(None);
    }

    let mut bedrock_tools = Vec::new();
    for tool in tools {
        let strict = resolve_json_schema_strict_sampling(tool, supports_strict_mode)?;
        let parameters = get_json_schema_tool_parameters(tool, strict)?;
        // Wire member order follows the SDK schema (name, inputSchema,
        // description, strict), not the upstream object-literal order.
        let mut tool_spec = Map::new();
        tool_spec.insert("name".to_owned(), json!(tool.name));
        tool_spec.insert("inputSchema".to_owned(), json!({"json": parameters}));
        tool_spec.insert("description".to_owned(), json!(tool.description));
        if strict == Some(true) {
            tool_spec.insert("strict".to_owned(), json!(true));
        }
        let mut tool_entry = Map::new();
        tool_entry.insert("toolSpec".to_owned(), Value::Object(tool_spec));
        bedrock_tools.push(Value::Object(tool_entry));
    }

    let bedrock_tool_choice = match tool_choice {
        Some(BedrockToolChoice::Auto) => Some(json!({"auto": {}})),
        Some(BedrockToolChoice::Any) => Some(json!({"any": {}})),
        Some(BedrockToolChoice::Tool { name }) => Some(json!({"tool": {"name": name}})),
        _ => None,
    };

    let mut tool_config = Map::new();
    tool_config.insert("tools".to_owned(), Value::Array(bedrock_tools));
    if let Some(choice) = bedrock_tool_choice {
        tool_config.insert("toolChoice".to_owned(), choice);
    }
    Ok(Some(Value::Object(tool_config)))
}

/// `THINKING_BINDING_CONTROLS_BETA` (bedrock-converse-stream.ts:121
/// @ 69f0be6f0): dropped stale thinking blocks on replays.
const THINKING_BINDING_CONTROLS_BETA: &str = "thinking-binding-controls-2026-08-01";

/// `supportsThinkingBlockBinding` (#10324 `69f0be6f0`): the model accepts
/// `thinking.block_binding`. Opus 4.6 and Sonnet 4.6 reject it with
/// "thinking.adaptive.block_binding: Extra inputs are not permitted".
fn supports_thinking_block_binding(model: &Model) -> bool {
    get_model_match_candidates(&model.id, &model.name)
        .iter()
        .any(|s| {
            s.contains("opus-4-7")
                || s.contains("opus-4-8")
                || s.contains("opus-5")
                || s.contains("sonnet-5")
                || s.contains("fable-5")
        })
}

/// OpenAI GPT effort on Bedrock (#9331/#10142 `2989eb581`): GPT-5.x and
/// GPT-6 take a nested `reasoning.effort` and reject `minimal`.
const OPENAI_GPT_EFFORT: [(ThinkingLevel, &str); 6] = [
    (ThinkingLevel::Minimal, "low"),
    (ThinkingLevel::Low, "low"),
    (ThinkingLevel::Medium, "medium"),
    (ThinkingLevel::High, "high"),
    (ThinkingLevel::Xhigh, "xhigh"),
    (ThinkingLevel::Max, "max"),
];

/// gpt-oss takes a flat `reasoning_effort` and only accepts low, medium and
/// high.
const OPENAI_GPT_OSS_EFFORT: [(ThinkingLevel, &str); 6] = [
    (ThinkingLevel::Minimal, "low"),
    (ThinkingLevel::Low, "low"),
    (ThinkingLevel::Medium, "medium"),
    (ThinkingLevel::High, "high"),
    (ThinkingLevel::Xhigh, "high"),
    (ThinkingLevel::Max, "high"),
];

fn table_effort(table: &[(ThinkingLevel, &'static str)], level: ThinkingLevel) -> &'static str {
    table
        .iter()
        .find(|(candidate, _)| *candidate == level)
        .map(|(_, effort)| *effort)
        .unwrap_or("medium")
}

/// `buildAdditionalModelRequestFields`: adaptive thinking (effort) for the
/// newer Claude families, budget-based thinking with the interleaved-thinking
/// beta flag otherwise; `display` and `block_binding` are omitted on GovCloud.
/// Non-Claude reasoning models on Converse get their OpenAI reasoning effort
/// form (#9331/#10142).
pub fn build_additional_model_request_fields(
    model: &Model,
    options: &BedrockOptions,
) -> Option<Value> {
    let reasoning = options.reasoning?;
    if !model.reasoning {
        return None;
    }

    if !is_anthropic_claude_model(model) {
        let candidates = get_model_match_candidates(&model.id, &model.name);
        if candidates.iter().any(|s| s.contains("gpt-oss")) {
            return Some(json!({
                "reasoning_effort": table_effort(&OPENAI_GPT_OSS_EFFORT, reasoning),
            }));
        }
        if candidates.iter().any(|s| s.contains("gpt-")) {
            let mapped = model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.get(&reasoning.to_model_level()))
                .and_then(|value| value.clone());
            let effort =
                mapped.unwrap_or_else(|| table_effort(&OPENAI_GPT_EFFORT, reasoning).to_owned());
            return Some(json!({"reasoning": {"effort": effort}}));
        }
        return None;
    }

    let is_gov_cloud = is_gov_cloud_bedrock_target(model, options);
    let display = if is_gov_cloud {
        None
    } else {
        Some(
            options
                .thinking_display
                .unwrap_or(BedrockThinkingDisplay::Summarized)
                .as_str(),
        )
    };
    // Replayed signed thinking blocks are bound to the system prompt and
    // tools they were created with. Bedrock 400s on replay after either
    // changes unless stale blocks are dropped, matching the Anthropic
    // provider. Skipped on GovCloud like display (#10324).
    let use_block_binding = !is_gov_cloud && supports_thinking_block_binding(model);

    let adaptive = supports_adaptive_thinking(&model.id, &model.name);
    let mut fields = Map::new();
    if adaptive {
        let mut thinking = Map::new();
        thinking.insert("type".to_owned(), json!("adaptive"));
        if let Some(display) = display {
            thinking.insert("display".to_owned(), json!(display));
        }
        if use_block_binding {
            thinking.insert(
                "block_binding".to_owned(),
                json!({"prefix_mismatch_behavior": "drop_block"}),
            );
        }
        fields.insert("thinking".to_owned(), Value::Object(thinking));
        fields.insert(
            "output_config".to_owned(),
            json!({"effort": map_thinking_level_to_effort(model, options.reasoning)}),
        );
        if use_block_binding {
            fields.insert(
                "anthropic_beta".to_owned(),
                json!([THINKING_BINDING_CONTROLS_BETA]),
            );
        }
    } else {
        let default_budget = match reasoning {
            ThinkingLevel::Minimal => 1024,
            ThinkingLevel::Low => 2048,
            ThinkingLevel::Medium => 8192,
            // Budget-based Claude clamps extended levels to high.
            ThinkingLevel::High | ThinkingLevel::Xhigh | ThinkingLevel::Max => 16384,
        };
        // Custom budgets only cover token-based levels through high.
        let custom_level = clamp_reasoning(Some(reasoning)).unwrap_or(reasoning);
        let custom_budget =
            options
                .thinking_budgets
                .as_ref()
                .and_then(|budgets| match custom_level {
                    ThinkingLevel::Minimal => budgets.minimal,
                    ThinkingLevel::Low => budgets.low,
                    ThinkingLevel::Medium => budgets.medium,
                    _ => budgets.high,
                });
        let budget = custom_budget.unwrap_or(default_budget);

        let mut thinking = Map::new();
        thinking.insert("type".to_owned(), json!("enabled"));
        thinking.insert("budget_tokens".to_owned(), json!(budget));
        if let Some(display) = display {
            thinking.insert("display".to_owned(), json!(display));
        }
        fields.insert("thinking".to_owned(), Value::Object(thinking));
    }

    if !adaptive && options.interleaved_thinking.unwrap_or(true) {
        fields.insert(
            "anthropic_beta".to_owned(),
            json!(["interleaved-thinking-2025-05-14"]),
        );
    }

    Some(Value::Object(fields))
}

/// The `ConverseStreamCommand` input (camelCase, including the `modelId`
/// path label) as handed to `on_payload`.
fn build_command_input(
    model: &Model,
    context: &TranscriptContext,
    options: &BedrockOptions,
    cache_retention: CacheRetention,
) -> Result<Value, String> {
    let env = options.stream.env.as_ref();
    // Bedrock has no mid-conversation system messages; fold them into the
    // leading prompt (#9548, bedrock-converse-stream.ts:132).
    let context = &crate::utils::transcript::collapse_system_messages(context);
    let initial_system_prompt =
        crate::utils::transcript::get_initial_system_message(&context.messages)
            .map(crate::utils::text::get_system_message_text);
    let inference_max_tokens = options
        .stream
        .max_tokens
        .or(if is_anthropic_claude_model(model) {
            Some(model.max_tokens)
        } else {
            None
        });

    let mut input = Map::new();
    input.insert("modelId".to_owned(), json!(model.id));
    input.insert(
        "messages".to_owned(),
        Value::Array(convert_messages(context, model, cache_retention, env)?),
    );
    if let Some(system) = build_system_prompt(
        initial_system_prompt.as_deref(),
        model,
        cache_retention,
        env,
    ) {
        input.insert("system".to_owned(), Value::Array(system));
    }
    let mut inference_config = Map::new();
    if let Some(max_tokens) = inference_max_tokens {
        inference_config.insert("maxTokens".to_owned(), json!(max_tokens));
    }
    if let Some(temperature) = options.stream.temperature {
        inference_config.insert("temperature".to_owned(), json!(temperature));
    }
    if !inference_config.is_empty() {
        input.insert(
            "inferenceConfig".to_owned(),
            Value::Object(inference_config),
        );
    }
    let supports_strict_mode = model
        .compat
        .as_ref()
        .and_then(|compat| compat.supports_strict_mode)
        .unwrap_or(false);
    let current_tools = crate::utils::transcript::get_current_tools(&context.messages);
    if let Some(tool_config) = convert_tool_config(
        Some(current_tools.as_slice()),
        options.tool_choice.as_ref(),
        supports_strict_mode,
    )? {
        input.insert("toolConfig".to_owned(), tool_config);
    }
    if let Some(fields) = build_additional_model_request_fields(model, options) {
        input.insert("additionalModelRequestFields".to_owned(), fields);
    }
    if let Some(request_metadata) = &options.request_metadata {
        input.insert(
            "requestMetadata".to_owned(),
            Value::Object(request_metadata.clone()),
        );
    }
    Ok(Value::Object(input))
}

// ---------------------------------------------------------------------------
// Error formatting
// ---------------------------------------------------------------------------

/// `formatBedrockError`: human-readable exception-name prefix plus the
/// normalized status/body composition, and the data-retention hint.
///
/// `message` plays the role of the SDK exception's message; `status`/`body`
/// come from the HTTP response (or stream exception payload) directly.
pub fn format_bedrock_error(
    exception_name: Option<&str>,
    status: Option<u16>,
    body: Option<&str>,
    message: &str,
) -> String {
    let norm = NormalizedProviderError::new(status, body.map(str::to_owned), message.to_owned());
    let core = match (&norm.status, &norm.body) {
        (Some(status), Some(body)) if !norm.message_carries_body => {
            format!("{status}: {body}")
        }
        _ => norm.message.clone(),
    };
    let hint = if DATA_RETENTION_PATTERN.is_match(&core) {
        format!(" See {BEDROCK_DATA_RETENTION_DOCS_URL} for supported data retention modes.")
    } else {
        String::new()
    };
    match exception_name {
        Some(name) => {
            let prefix = BEDROCK_ERROR_PREFIXES
                .iter()
                .find_map(|(key, prefix)| (*key == name).then_some(*prefix))
                .unwrap_or(name);
            format!("{prefix}: {core}{hint}")
        }
        None => format!("{core}{hint}"),
    }
}

/// `sanitizeErrorCode` (parseJsonBody.ts): first entry before `,`, before
/// `:`; namespace (`#`) stripped.
fn sanitize_error_code(raw: &str) -> String {
    let first = raw.split(',').next().unwrap_or(raw);
    let first = first.split(':').next().unwrap_or(first);
    match first.split_once('#') {
        Some((_, name)) => name.to_owned(),
        None => first.to_owned(),
    }
}

/// `loadRestJsonErrorCode` order: `x-amzn-errortype` header > body `code` >
/// body `__type`.
fn resolve_error_code(
    headers: Option<&HashMap<String, String>>,
    body: Option<&str>,
) -> Option<String> {
    if let Some(header) = headers
        .and_then(|headers| headers.get("x-amzn-errortype"))
        .filter(|value| !value.is_empty())
    {
        return Some(sanitize_error_code(header));
    }
    let parsed: Option<Value> = body.and_then(|body| serde_json::from_str(body).ok());
    for key in ["code", "__type"] {
        if let Some(value) = parsed
            .as_ref()
            .and_then(|parsed| parsed.get(key))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            return Some(sanitize_error_code(value));
        }
    }
    None
}

/// Error-body `message` / `Message` extraction (`parseJsonErrorBody`).
fn extract_error_message(body: Option<&str>) -> Option<String> {
    let parsed: Value = serde_json::from_str(body?).ok()?;
    parsed
        .get("message")
        .or_else(|| parsed.get("Message"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

// ---------------------------------------------------------------------------
// Failure diagnostics (70bbe47a9, #7286)
// ---------------------------------------------------------------------------

/// The SDK error metadata (`$metadata`) rpi analogue: HTTP status, modeled
/// error code, and request id captured alongside a failed turn for the
/// `bedrock_response_failure` diagnostic. Written by [`run`] (initial-request
/// failures and unmodeled mid-stream error frames), read by [`stream`] when
/// the turn ends in `stopReason: "error"`.
#[derive(Debug, Default)]
struct BedrockFailureMetadata {
    /// HTTP status of the failed initial response (`$metadata.httpStatusCode`).
    status: Option<u16>,
    /// Modeled AWS error code — the SDK puts it on `error.name` for service
    /// exceptions (via `x-amzn-errortype` / body `__type`) and for unmodeled
    /// mid-stream errors (via the frame's `:error-code`).
    error_code: Option<String>,
    /// Request id of the failed initial response (`$metadata.requestId`).
    request_id: Option<String>,
    /// Request id of the initial (successful) response, kept outside the
    /// failure arms so a mid-stream failure stays correlatable: exceptions
    /// delivered as stream events carry no HTTP metadata of their own.
    response_request_id: Option<String>,
}

/// Over-long header-derived values are dropped rather than truncated: a
/// truncated request id is not a request id (70bbe47a9).
const MAX_BEDROCK_DIAGNOSTIC_VALUE_CHARS: usize = 200;

/// `normalizeDiagnosticValue` (70bbe47a9). Counts Unicode scalars; JS counts
/// UTF-16 units — request ids and AWS error codes are ASCII in practice.
fn normalize_diagnostic_value(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_BEDROCK_DIAGNOSTIC_VALUE_CHARS {
        return None;
    }
    Some(trimmed.to_owned())
}

/// `extractBedrockErrorCode` (70bbe47a9): all 13 modeled Bedrock errors end
/// in `Exception`; that suffix is the gate — it excludes transport failures
/// such as `TimeoutError` and the SDK's `Unknown` placeholder without
/// enumerating them.
fn extract_bedrock_error_code(name: Option<&str>) -> Option<String> {
    let name = name?;
    if !name.ends_with("Exception") {
        return None;
    }
    normalize_diagnostic_value(name)
}

/// `appendBedrockFailureDiagnostic` (70bbe47a9): structured metadata
/// alongside `errorMessage`, which stays byte-identical because
/// `isRetryableAssistantError` classifies retries by string-matching it.
/// Unknown fields are omitted, never guessed: a modeled mid-stream exception
/// reaches the handler as a bare object literal upstream, leaving only the
/// fallback request id. `details` only, as the throw is not always an
/// `Error`.
fn append_bedrock_failure_diagnostic(
    output: &mut AssistantMessage,
    metadata: &BedrockFailureMetadata,
) {
    let mut details = Map::new();
    if let Some(status) = metadata.status {
        details.insert("status".to_owned(), json!(status));
    }
    if let Some(error_code) = extract_bedrock_error_code(metadata.error_code.as_deref()) {
        details.insert("errorCode".to_owned(), json!(error_code));
    }
    let request_id = metadata
        .request_id
        .as_deref()
        .and_then(normalize_diagnostic_value)
        .or_else(|| metadata.response_request_id.clone());
    if let Some(request_id) = request_id {
        details.insert("requestId".to_owned(), json!(request_id));
    }
    if details.is_empty() {
        return;
    }
    output
        .diagnostics
        .get_or_insert_with(Vec::new)
        .push(AssistantMessageDiagnostic {
            kind: "bedrock_response_failure".to_owned(),
            timestamp: now_ms(),
            error: None,
            details: Some(details),
        });
}

// ---------------------------------------------------------------------------
// Stop-reason mapping
// ---------------------------------------------------------------------------

/// `mapStopReason` — empty/unknown reasons map to `Error`; the empty string
/// carries no message (JS `reason ? … : …` falsy edge). Unknown non-empty
/// reasons carry `Provider stopped with: <reason>` (637737ca7; text unified
/// by 5a2539a7b).
fn map_stop_reason(reason: Option<&str>) -> (StopReason, Option<String>) {
    match reason {
        Some("end_turn") | Some("stop_sequence") => (StopReason::Stop, None),
        Some("max_tokens") | Some("model_context_window_exceeded") => (StopReason::Length, None),
        Some("tool_use") => (StopReason::ToolUse, None),
        Some("") | None => (StopReason::Error, None),
        Some(other) => (
            StopReason::Error,
            Some(format!("Provider stopped with: {other}")),
        ),
    }
}

// ---------------------------------------------------------------------------
// Stream processing
// ---------------------------------------------------------------------------

/// Consumes decoded ConverseStream events and drives the [`StreamEvent`]
/// protocol, accumulating the final assistant message. Bedrock block indices
/// live in a side map (rpi's content blocks carry no `index` field), the
/// `partialJson` scratch buffer in another — matching upstream's
/// delete-before-finish semantics.
struct StreamProcessor<'a> {
    output: &'a mut AssistantMessage,
    model: &'a Model,
    /// Content index by Bedrock `contentBlockIndex`.
    blocks_by_bedrock_index: HashMap<u64, usize>,
    /// Tool-call partial JSON by content index.
    partial_json: HashMap<usize, String>,
    /// Scratch buffer for encrypted reasoning deltas (base64-decoded
    /// bytes), joined into `thinkingSignature` on flush — never persisted
    /// (`redactedChunks`, bedrock-converse-stream.ts:108, `d57e531f5`).
    redacted_chunks: HashMap<usize, Vec<u8>>,
    /// Mid-stream failure metadata sink (70bbe47a9): unmodeled error frames
    /// contribute their `:error-code` to the `bedrock_response_failure`
    /// diagnostic.
    failure: Arc<Mutex<BedrockFailureMetadata>>,
}

impl<'a> StreamProcessor<'a> {
    fn new(
        output: &'a mut AssistantMessage,
        model: &'a Model,
        failure: Arc<Mutex<BedrockFailureMetadata>>,
    ) -> Self {
        Self {
            output,
            model,
            blocks_by_bedrock_index: HashMap::new(),
            partial_json: HashMap::new(),
            redacted_chunks: HashMap::new(),
            failure,
        }
    }

    /// One decoded event-stream message (the `getMessageUnmarshaller`
    /// dispatch: exception / error / event).
    fn handle_message(
        &mut self,
        message: &EventStreamMessage,
        events: &AssistantMessageEventStream,
    ) -> Result<(), String> {
        match message.header_str(":message-type") {
            Some("exception") => {
                let code = message.header_str(":exception-type").unwrap_or("");
                let payload: Value = serde_json::from_slice(&message.body).unwrap_or(Value::Null);
                let exception_message =
                    payload.get("message").and_then(Value::as_str).unwrap_or("");
                // The union member name is camelCase; the SDK exception class
                // name (which `formatBedrockError` prefixes on) is PascalCase.
                let mut pascal = code.to_owned();
                if let Some(first) = pascal.get(..1) {
                    pascal.replace_range(..1, &first.to_uppercase());
                }
                Err(format_bedrock_error(
                    Some(&pascal),
                    None,
                    None,
                    exception_message,
                ))
            }
            Some("error") => {
                // Unmodeled error: plain message, no exception prefix.
                // 70bbe47a9: this branch throws a real `Error` named after
                // the frame's `:error-code` upstream — keep it for the
                // diagnostic. A modeled exception frame (the "exception" arm
                // above) surfaces as a bare object literal upstream, so its
                // code is genuinely unavailable and nothing is recorded.
                if let Ok(mut guard) = self.failure.lock() {
                    guard.error_code = message.header_str(":error-code").map(str::to_owned);
                }
                let error_message = message
                    .header_str(":error-message")
                    .unwrap_or("UnknownError");
                Err(format_bedrock_error(None, None, None, error_message))
            }
            Some("event") => {
                let event_type = message.header_str(":event-type").unwrap_or("");
                let payload: Value = serde_json::from_slice(&message.body)
                    .map_err(|error| format!("Could not parse Bedrock event payload: {error}"))?;
                self.handle_event(event_type, &payload, events)
            }
            _ => Err(format!(
                "Unrecognizable event type: {}",
                message.header_str(":event-type").unwrap_or("")
            )),
        }
    }

    /// `ConverseStreamOutput` union dispatch; unknown event types are skipped
    /// (the SDK deserializer yields `$unknown` and the unmarshaller
    /// `continue`s).
    fn handle_event(
        &mut self,
        event_type: &str,
        payload: &Value,
        events: &AssistantMessageEventStream,
    ) -> Result<(), String> {
        match event_type {
            "messageStart" => self.handle_message_start(payload, events),
            "contentBlockStart" => {
                self.handle_content_block_start(payload, events);
                Ok(())
            }
            "contentBlockDelta" => {
                self.handle_content_block_delta(payload, events);
                Ok(())
            }
            "contentBlockStop" => {
                self.handle_content_block_stop(payload, events);
                Ok(())
            }
            "messageStop" => {
                // 637737ca7: preserve the raw provider reason before mapping
                // (JS assigns `undefined` when absent, i.e. `None`).
                self.output.raw_stop_reason = payload
                    .get("stopReason")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let (stop_reason, error_message) =
                    map_stop_reason(payload.get("stopReason").and_then(Value::as_str));
                self.output.stop_reason = stop_reason;
                if let Some(error_message) = error_message {
                    self.output.error_message = Some(error_message);
                }
                Ok(())
            }
            "metadata" => {
                self.handle_metadata(payload);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// `messageStart`: role must be assistant.
    fn handle_message_start(
        &mut self,
        payload: &Value,
        events: &AssistantMessageEventStream,
    ) -> Result<(), String> {
        if payload.get("role").and_then(Value::as_str) != Some("assistant") {
            return Err(
                "Unexpected assistant message start but got user message start instead".to_owned(),
            );
        }
        events.push(StreamEvent::Start {
            partial: Arc::new(self.output.clone()),
        });
        Ok(())
    }

    /// `handleContentBlockStart`: only tool-use starts create a block (text
    /// and thinking blocks are created lazily on their first delta).
    fn handle_content_block_start(
        &mut self,
        payload: &Value,
        events: &AssistantMessageEventStream,
    ) {
        let Some(index) = payload.get("contentBlockIndex").and_then(Value::as_u64) else {
            return;
        };
        let Some(tool_use) = payload.get("start").and_then(|start| start.get("toolUse")) else {
            return;
        };
        self.output
            .content
            .push(AssistantContent::ToolCall(crate::types::ToolCall {
                id: tool_use
                    .get("toolUseId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                name: tool_use
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                arguments: Map::new(),
                thought_signature: None,
                namespace: None,
            }));
        let content_index = self.output.content.len() - 1;
        self.blocks_by_bedrock_index.insert(index, content_index);
        self.partial_json.insert(content_index, String::new());
        events.push(StreamEvent::ToolCallStart {
            content_index,
            partial: Arc::new(self.output.clone()),
        });
    }

    /// `handleContentBlockDelta`.
    fn handle_content_block_delta(
        &mut self,
        payload: &Value,
        events: &AssistantMessageEventStream,
    ) {
        let Some(bedrock_index) = payload.get("contentBlockIndex").and_then(Value::as_u64) else {
            return;
        };
        let Some(delta) = payload.get("delta") else {
            return;
        };
        let block_index = self.blocks_by_bedrock_index.get(&bedrock_index).copied();

        // Text: JS checks `delta?.text !== undefined` — an empty string delta
        // is still processed.
        if let Some(text) = delta.get("text").and_then(Value::as_str) {
            let index = match block_index {
                Some(index) => Some(index),
                None => {
                    // Text blocks get no contentBlockStart upstream; create
                    // the block lazily on the first delta.
                    self.output
                        .content
                        .push(AssistantContent::Text(Default::default()));
                    let index = self.output.content.len() - 1;
                    self.blocks_by_bedrock_index.insert(bedrock_index, index);
                    events.push(StreamEvent::TextStart {
                        content_index: index,
                        partial: Arc::new(self.output.clone()),
                    });
                    Some(index)
                }
            };
            if let Some(index) = index
                && let AssistantContent::Text(text_block) = &mut self.output.content[index]
            {
                text_block.text.push_str(text);
                events.push(StreamEvent::TextDelta {
                    content_index: index,
                    delta: text.to_owned(),
                    partial: Arc::new(self.output.clone()),
                });
            }
            return;
        }

        // Tool use input chunks.
        if let Some(tool_use) = delta.get("toolUse") {
            if let Some(index) = block_index
                && matches!(self.output.content[index], AssistantContent::ToolCall(_))
            {
                let input = tool_use.get("input").and_then(Value::as_str).unwrap_or("");
                let partial = self.partial_json.entry(index).or_default();
                partial.push_str(input);
                if let AssistantContent::ToolCall(call) = &mut self.output.content[index] {
                    call.arguments = parse_streaming_json(Some(partial));
                }
                events.push(StreamEvent::ToolCallDelta {
                    content_index: index,
                    delta: input.to_owned(),
                    partial: Arc::new(self.output.clone()),
                });
            }
            return;
        }

        // Reasoning content (thinking text + signature). JS truthiness:
        // empty strings skip both the append and the event.
        if let Some(reasoning) = delta.get("reasoningContent") {
            let index = match block_index {
                Some(index) => index,
                None => {
                    self.output
                        .content
                        .push(AssistantContent::Thinking(Default::default()));
                    let index = self.output.content.len() - 1;
                    self.blocks_by_bedrock_index.insert(bedrock_index, index);
                    events.push(StreamEvent::ThinkingStart {
                        content_index: index,
                        partial: Arc::new(self.output.clone()),
                    });
                    index
                }
            };
            // Text first (borrow ends before the event push), then signature.
            let text = reasoning
                .get("text")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_owned);
            if let Some(text) = text {
                if let AssistantContent::Thinking(thinking) = &mut self.output.content[index] {
                    thinking.thinking.push_str(&text);
                }
                events.push(StreamEvent::ThinkingDelta {
                    content_index: index,
                    delta: text,
                    partial: Arc::new(self.output.clone()),
                });
            }
            if let Some(signature) = reasoning
                .get("signature")
                .and_then(Value::as_str)
                .filter(|signature| !signature.is_empty())
                && let AssistantContent::Thinking(thinking) = &mut self.output.content[index]
            {
                // `thinkingSignature` holds either an Anthropic signature
                // or an opaque redacted payload, never both: mixing them
                // would corrupt whichever arrived first
                // (bedrock-converse-stream.ts:637, `d57e531f5`).
                if thinking.redacted != Some(true) {
                    let current = thinking.thinking_signature.get_or_insert_with(String::new);
                    current.push_str(signature);
                }
            }
            // Encrypted reasoning from non-Anthropic models on Bedrock (e.g.
            // OpenAI GPT-5.6, `d57e531f5` / #8314). The payload is opaque, so
            // keep it verbatim (base64 on the wire → bytes here) the way the
            // Anthropic path stores redacted thinking, and replay it as
            // `redactedContent` on the next turn.
            if let Some(redacted) = reasoning
                .get("redactedContent")
                .and_then(Value::as_str)
                .filter(|redacted| !redacted.is_empty())
            {
                use base64::Engine as _;
                let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(redacted) else {
                    return;
                };
                let first =
                    if let AssistantContent::Thinking(thinking) = &mut self.output.content[index] {
                        let first = thinking.redacted != Some(true);
                        if first {
                            thinking.redacted = Some(true);
                            thinking.thinking_signature = Some(String::new());
                            thinking.thinking.push_str(REDACTED_THINKING_PLACEHOLDER);
                        }
                        first
                    } else {
                        false
                    };
                if first {
                    events.push(StreamEvent::ThinkingDelta {
                        content_index: index,
                        delta: REDACTED_THINKING_PLACEHOLDER.to_owned(),
                        partial: Arc::new(self.output.clone()),
                    });
                }
                self.redacted_chunks
                    .entry(index)
                    .or_default()
                    .extend_from_slice(&bytes);
            }
        }
    }

    /// `handleContentBlockStop`: finalize the block and drop the scratch
    /// state (upstream `delete block.index` / `delete block.partialJson`).
    fn handle_content_block_stop(&mut self, payload: &Value, events: &AssistantMessageEventStream) {
        let Some(bedrock_index) = payload.get("contentBlockIndex").and_then(Value::as_u64) else {
            return;
        };
        let Some(index) = self.blocks_by_bedrock_index.remove(&bedrock_index) else {
            return;
        };
        // `contentBlockStop` flushes the redacted scratch buffer before the
        // terminal event (bedrock-converse-stream.ts:716).
        if matches!(self.output.content[index], AssistantContent::Thinking(_)) {
            self.flush_redacted_content(index);
        }
        match &mut self.output.content[index] {
            AssistantContent::Text(text) => {
                events.push(StreamEvent::TextEnd {
                    content_index: index,
                    content: text.text.clone(),
                    partial: Arc::new(self.output.clone()),
                });
            }
            AssistantContent::Thinking(thinking) => {
                events.push(StreamEvent::ThinkingEnd {
                    content_index: index,
                    content: thinking.thinking.clone(),
                    partial: Arc::new(self.output.clone()),
                });
            }
            AssistantContent::ToolCall(call) => {
                let partial = self.partial_json.remove(&index).unwrap_or_default();
                call.arguments = parse_streaming_json(Some(&partial));
                events.push(StreamEvent::ToolCallEnd {
                    content_index: index,
                    tool_call: call.clone(),
                    partial: Arc::new(self.output.clone()),
                });
            }
        }
    }

    /// `flushRedactedContent` (bedrock-converse-stream.ts:668, `d57e531f5`):
    /// encode the buffered encrypted reasoning into `thinkingSignature`
    /// (base64 of the concatenated bytes) and drop the scratch buffer, which
    /// must never reach a persisted message.
    fn flush_redacted_content(&mut self, index: usize) {
        let Some(bytes) = self.redacted_chunks.remove(&index) else {
            return;
        };
        use base64::Engine as _;
        if let AssistantContent::Thinking(thinking) = &mut self.output.content[index] {
            thinking.thinking_signature =
                Some(base64::engine::general_purpose::STANDARD.encode(bytes));
        }
    }

    /// Terminal flush — upstream `finalizeStreamingBlock` runs from the done
    /// and error paths as well as `contentBlockStop`, because a stream can
    /// settle without stopping each block (bedrock-converse-stream.ts:325,
    /// :330).
    fn flush_all_redacted(&mut self) {
        let indices: Vec<usize> = self.redacted_chunks.keys().copied().collect();
        for index in indices {
            self.flush_redacted_content(index);
        }
    }

    /// `handleMetadata`: usage accounting (cache read/write split) + cost.
    fn handle_metadata(&mut self, payload: &Value) {
        let Some(usage) = payload.get("usage") else {
            return;
        };
        let number = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
        self.output.usage.input = number("inputTokens");
        self.output.usage.output = number("outputTokens");
        self.output.usage.cache_read = number("cacheReadInputTokens");
        self.output.usage.cache_write = number("cacheWriteInputTokens");
        // #9457 (8a7b0c03d @ d1230ea20): cache writes with a 1h TTL bill at
        // 2x base input — split them out of the aggregate
        // `cacheWriteInputTokens` so `calculateCost` prices the long
        // retention writes at the 1h rate. Absent `cacheDetails` leaves
        // `cacheWrite1h` unset (upstream `undefined`).
        if let Some(details) = usage.get("cacheDetails").and_then(Value::as_array) {
            self.output.usage.cache_write1h = Some(
                details
                    .iter()
                    .filter(|detail| detail.get("ttl").and_then(Value::as_str) == Some("1h"))
                    .map(|detail| {
                        detail
                            .get("inputTokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0)
                    })
                    .sum(),
            );
        }
        let total = number("totalTokens");
        // JS `totalTokens || input + output` (0 is falsy).
        self.output.usage.total_tokens = if total > 0 {
            total
        } else {
            self.output.usage.input + self.output.usage.output
        };
        calculate_cost(self.model, &mut self.output.usage);
    }
}

// ---------------------------------------------------------------------------
// HTTP + orchestration
// ---------------------------------------------------------------------------

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `createOutput`.
fn initial_output(model: &Model) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Pending,
        error_message: None,
        timestamp: now_ms(),
        deferred: None,
        end_turn: None,
        raw_stop_reason: None,
        duration_ms: None,
    }
}

/// `host` header value for the endpoint (with non-default port).
fn host_header(endpoint: &str) -> Result<String, String> {
    let url = url::Url::parse(endpoint).map_err(|error| format!("Invalid URL: {error}"))?;
    let host = url
        .host_str()
        .ok_or_else(|| "Invalid URL: missing host".to_owned())?;
    Ok(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

/// The streaming body: everything that runs inside upstream's async IIFE.
/// Errors return the upstream `formatBedrockError` output; `output` carries
/// the partial message either way. Provider-failure metadata for the
/// `bedrock_response_failure` diagnostic (70bbe47a9) accumulates in
/// `failure`.
async fn run(
    model: &Model,
    context: &TranscriptContext,
    options: &BedrockOptions,
    output: &mut AssistantMessage,
    events: &AssistantMessageEventStream,
    failure: Arc<Mutex<BedrockFailureMetadata>>,
) -> Result<DoneReason, String> {
    let config = resolve_bedrock_config(model, options);
    let cache_retention =
        resolve_cache_retention(options.stream.cache_retention, options.stream.env.as_ref());

    let mut command_input = build_command_input(model, context, options, cache_retention)?;
    if let Some(on_payload) = &options.stream.on_payload
        && let Some(next_payload) = on_payload(command_input.clone(), model).await
    {
        command_input = next_payload;
    }

    // `modelId` is an HTTP label: it goes to the path, not the body.
    let model_id = command_input
        .get("modelId")
        .and_then(Value::as_str)
        .unwrap_or(&model.id)
        .to_owned();
    let mut body = command_input;
    if let Some(object) = body.as_object_mut() {
        object.shift_remove("modelId");
    }
    let body_bytes = serde_json::to_vec(&body)
        .map_err(|error| format!("Could not serialize payload: {error}"))?;

    let region = config.effective_region();
    let endpoint = config
        .endpoint
        .clone()
        .unwrap_or_else(|| derive_regional_endpoint(&region));
    let path = format!(
        "/model/{}/converse-stream",
        extended_encode_uri_component(&model_id)
    );
    let url = format!("{}{path}", endpoint.trim_end_matches('/'));
    let host = host_header(&endpoint)?;

    // Header set: content-type + host, then filtered caller headers
    // (the build-step middleware runs before signing, so these are signed).
    let mut headers: Vec<(String, String)> = vec![
        ("content-type".to_owned(), "application/json".to_owned()),
        ("host".to_owned(), host),
    ];
    headers.extend(filtered_custom_headers(options.stream.headers.as_ref()));

    let signed_headers = if let Some(bearer_token) = &config.bearer_token {
        headers.push((
            sigv4::AUTHORIZATION_HEADER.to_owned(),
            format!("Bearer {bearer_token}"),
        ));
        headers
    } else {
        let credentials = config
            .credentials
            .as_ref()
            .ok_or_else(|| "Could not load credentials from any providers".to_owned())?;
        sigv4::sign_request(
            &sigv4::SigV4Request {
                method: "POST",
                path: &path,
                query: &[],
                headers: &headers,
                payload: &body_bytes,
            },
            credentials,
            &region,
            SIGNING_SERVICE,
            now_secs(),
        )
    };
    let mut header_map = ProviderHeaders::new();
    for (name, value) in signed_headers {
        header_map.insert(name, Some(value));
    }
    let header_map = provider_headers_to_header_map(&header_map)?;

    let mut client_builder =
        crate::api::http_client::adapter_client_builder(options.stream.env.as_ref(), &url)?;
    // Idle-timeout semantics (upstream undici headersTimeout/bodyTimeout;
    // see api::stream_timeouts) — never a total-request deadline.
    if let Some(timeout_ms) = options.stream.timeout_ms {
        client_builder =
            client_builder.connect_timeout(std::time::Duration::from_millis(timeout_ms));
    }
    let client = client_builder.build().map_err(|error| error.to_string())?;

    let response = retry_provider_request(
        || {
            let request = client
                .post(&url)
                .headers(header_map.clone())
                .body(body_bytes.clone());
            let signal = options.stream.signal.clone();
            let failure = Arc::clone(&failure);
            async move {
                // 70bbe47a9: each attempt starts clean — the diagnostic
                // describes the final failure, like the SDK exception that
                // surfaces upstream after its own retries.
                if let Ok(mut guard) = failure.lock() {
                    guard.status = None;
                    guard.error_code = None;
                    guard.request_id = None;
                }
                let send = crate::api::stream_timeouts::send_with_headers_timeout(
                    request.send(),
                    options.stream.timeout_ms,
                );
                let result = match &signal {
                    Some(token) => tokio::select! {
                        outcome = send => outcome,
                        () = token.cancelled() => {
                            return Err(ProviderErrorInfo {
                                status: None,
                                headers: None,
                                message: "Request was aborted".to_owned(),
                            });
                        }
                    },
                    None => send.await,
                };
                match result {
                    crate::api::stream_timeouts::SendOutcome::Ok(response) => {
                        let status = response.status();
                        if status.is_success() {
                            Ok(response)
                        } else {
                            let status = status.as_u16();
                            let response_headers = headers_to_record(response.headers());
                            let body = response.text().await.unwrap_or_default();
                            let exception_name =
                                resolve_error_code(Some(&response_headers), Some(&body));
                            // 70bbe47a9: `$metadata` capture for the
                            // `bedrock_response_failure` diagnostic.
                            if let Ok(mut guard) = failure.lock() {
                                guard.status = Some(status);
                                guard.error_code = exception_name.clone();
                                guard.request_id =
                                    response_headers.get("x-amzn-requestid").cloned();
                            }
                            let message = extract_error_message(Some(&body))
                                .unwrap_or_else(|| format!("Request failed with status {status}"));
                            Err(ProviderErrorInfo {
                                status: Some(status),
                                headers: Some(response_headers),
                                // Surface the raw body (upstream
                                // `formatBedrockError`: `status: body` when
                                // the extracted message does not carry it) —
                                // a gateway 403 must not collapse to the
                                // generic status text.
                                message: format_bedrock_error(
                                    exception_name.as_deref(),
                                    Some(status),
                                    Some(&body),
                                    &message,
                                ),
                            })
                        }
                    }
                    crate::api::stream_timeouts::SendOutcome::Transport(error) => {
                        Err(ProviderErrorInfo {
                            status: error.status().map(|status| status.as_u16()),
                            headers: None,
                            message: format_bedrock_error(
                                None,
                                None,
                                None,
                                &crate::utils::transport_error::format_reqwest_error(&error),
                            ),
                        })
                    }
                    crate::api::stream_timeouts::SendOutcome::HeadersTimeout(message) => {
                        Err(ProviderErrorInfo {
                            status: None,
                            headers: None,
                            message: format_bedrock_error(None, None, None, &message),
                        })
                    }
                }
            }
        },
        ProviderRetryOptions {
            // The AWS SDK defaults to standard-mode retries (2 extra
            // attempts) and ignores `maxRetries` for Bedrock; rpi defaults
            // to the same 2 when the caller leaves it unset (D-026).
            max_retries: Some(options.stream.max_retries.unwrap_or(2)),
            max_retry_delay_ms: options.stream.max_retry_delay_ms,
            ..Default::default()
        },
        options.stream.signal.as_ref(),
    )
    .await
    .map_err(|error| error.message())?;

    // 70bbe47a9: kept so the error path can still correlate a mid-stream
    // failure — exceptions delivered as stream events carry no HTTP metadata
    // of their own.
    if let Ok(mut guard) = failure.lock() {
        guard.response_request_id = response
            .headers()
            .get("x-amzn-requestid")
            .and_then(|value| value.to_str().ok())
            .and_then(normalize_diagnostic_value);
    }

    if let Some(on_response) = &options.stream.on_response {
        // `addResponseHeadersMiddleware` (10acee604 / #8234): Bedrock's
        // modeled `$metadata` only preserves selected fields, so forward the
        // raw response headers — custom gateway headers reach `onResponse`
        // too (toProviderResponse spreads every header, :484).
        let headers: HashMap<String, String> = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.as_str().to_owned(), value.to_owned()))
            })
            .collect();
        on_response(
            ProviderResponse {
                status: response.status().as_u16(),
                headers,
            },
            model,
        )
        .await;
    }

    let mut processor = StreamProcessor::new(output, model, failure);
    let mut decoder = EventStreamDecoder::new();
    // Inter-chunk idle timeout (upstream bodyTimeout; see
    // api::stream_timeouts).
    let mut byte_stream =
        crate::api::stream_timeouts::wrap(response.bytes_stream(), options.stream.timeout_ms);
    let mut outcome: Result<(), String> = Ok(());
    'stream: loop {
        // P1-5: race the body read against the abort token — upstream hands
        // the signal to fetch (cancellation interrupts the body read
        // immediately); a check only after a chunk arrives would stall on an
        // idle body until the idle timeout (5 min default, unbounded when
        // disabled).
        let chunk =
            match next_chunk_or_cancelled(&mut byte_stream, options.stream.signal.as_ref()).await {
                StreamNext::Item(chunk) => chunk,
                StreamNext::Cancelled => {
                    outcome = Err("Request was aborted".to_owned());
                    break 'stream;
                }
            };
        let Some(chunk) = chunk else { break };
        let bytes = match chunk {
            Ok(bytes) => bytes,
            Err(error) => {
                outcome = Err(error.message());
                break 'stream;
            }
        };
        decoder.feed(&bytes);
        while let Some(message) = decoder.next_message() {
            match message {
                Ok(message) => {
                    if let Err(error) = processor.handle_message(&message, events) {
                        outcome = Err(error);
                        break 'stream;
                    }
                }
                Err(error) => {
                    outcome = Err(error);
                    break 'stream;
                }
            }
        }
    }
    if outcome.is_ok()
        && let Err(error) = decoder.finish()
    {
        outcome = Err(error);
    }
    // Terminal flush — a stream can settle without stopping every block, so
    // both the done and error paths finalize the scratch buffers first
    // (upstream finalizeStreamingBlock at :325/:330).
    processor.flush_all_redacted();
    outcome?;

    finalize(options, processor.output)
}

/// The upstream `stream` tail checks after the event stream is consumed.
fn finalize(options: &BedrockOptions, output: &AssistantMessage) -> Result<DoneReason, String> {
    if options
        .stream
        .signal
        .as_ref()
        .is_some_and(|signal| signal.is_cancelled())
    {
        return Err("Request was aborted".to_owned());
    }
    match output.stop_reason {
        // `Deferred` shares the `Pending` arm: no rpi provider produces it
        // (lifecycle is [DEFER], R2.2.1), so it is unreachable here and
        // treated as "stream ended without a usable stop reason".
        StopReason::Pending | StopReason::Deferred => Err(format_bedrock_error(
            None,
            None,
            None,
            "Bedrock stream ended without a stop reason",
        )),
        StopReason::Aborted | StopReason::Error => Err(format_bedrock_error(
            None,
            None,
            None,
            output
                .error_message
                .as_deref()
                .unwrap_or("An unknown error occurred"),
        )),
        StopReason::Stop => Ok(DoneReason::Stop),
        StopReason::Length => Ok(DoneReason::Length),
        StopReason::ToolUse => Ok(DoneReason::ToolUse),
    }
}

/// `stream` (bedrock-converse-stream).
pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: BedrockOptions,
) -> AssistantMessageEventStream {
    let event_stream = AssistantMessageEventStream::new();
    let task_stream = event_stream.clone();
    let model = model.clone();
    let context = context.clone();
    tokio::spawn(async move {
        let signal = options.stream.signal.clone();
        let failure = Arc::new(Mutex::new(BedrockFailureMetadata::default()));
        let mut output = initial_output(&model);
        match run(
            &model,
            &context,
            &options,
            &mut output,
            &task_stream,
            failure.clone(),
        )
        .await
        {
            Ok(reason) => {
                task_stream.push(StreamEvent::Done {
                    reason,
                    message: output,
                });
                task_stream.end(None);
            }
            Err(message) => {
                let aborted = signal.as_ref().is_some_and(|signal| signal.is_cancelled());
                output.stop_reason = if aborted {
                    StopReason::Aborted
                } else {
                    StopReason::Error
                };
                output.error_message = Some(message);
                // 70bbe47a9: structured provider-failure metadata; aborted
                // turns emit no diagnostic.
                if !aborted && let Ok(guard) = failure.lock() {
                    append_bedrock_failure_diagnostic(&mut output, &guard);
                }
                task_stream.push(StreamEvent::Error {
                    reason: if aborted {
                        ErrorReason::Aborted
                    } else {
                        ErrorReason::Error
                    },
                    error: output,
                });
                task_stream.end(None);
            }
        }
    });
    event_stream
}

/// `streamSimple` (bedrock-converse-stream): adaptive-thinking Claude models
/// take the reasoning level as an effort; older Claude models get a fitted
/// thinking budget; other models just pass the level through.
pub fn stream_simple(
    model: &Model,
    context: &TranscriptContext,
    options: Option<SimpleStreamOptions>,
) -> Result<AssistantMessageEventStream, String> {
    let base = build_base_options(model, context, options.as_ref(), None);
    // `base` carries `toolChoice` on every branch upstream
    // (bedrock-converse-stream.ts:518 — rc.12 review: previously dropped).
    let tool_choice = options
        .as_ref()
        .and_then(|o| o.tool_choice)
        .map(|choice| match choice {
            crate::types::SimpleToolChoice::Auto => BedrockToolChoice::Auto,
            crate::types::SimpleToolChoice::None => BedrockToolChoice::None,
        });
    let Some(reasoning) = options.as_ref().and_then(|o| o.reasoning) else {
        return Ok(stream(
            model,
            context,
            BedrockOptions {
                stream: base,
                tool_choice,
                ..BedrockOptions::default()
            },
        ));
    };

    if is_anthropic_claude_model(model) {
        if supports_adaptive_thinking(&model.id, &model.name) {
            return Ok(stream(
                model,
                context,
                BedrockOptions {
                    stream: base,
                    reasoning: Some(reasoning),
                    tool_choice,
                    thinking_budgets: options.as_ref().and_then(|o| o.thinking_budgets.clone()),
                    ..BedrockOptions::default()
                },
            ));
        }

        // `None` max_tokens means the caller did not request an output cap;
        // the helper then fits thinking inside the model cap.
        let adjusted = adjust_max_tokens_for_thinking(
            base.max_tokens,
            model.max_tokens,
            reasoning,
            options.as_ref().and_then(|o| o.thinking_budgets.as_ref()),
        );
        let max_tokens = clamp_max_tokens_to_context(model, context, adjusted.max_tokens);

        let mut budgets = options
            .as_ref()
            .and_then(|o| o.thinking_budgets.clone())
            .unwrap_or_default();
        let clamped_budget = adjusted
            .thinking_budget
            .min(max_tokens.saturating_sub(1024));
        match clamp_reasoning(Some(reasoning)).unwrap_or(reasoning) {
            ThinkingLevel::Minimal => budgets.minimal = Some(clamped_budget),
            ThinkingLevel::Low => budgets.low = Some(clamped_budget),
            ThinkingLevel::Medium => budgets.medium = Some(clamped_budget),
            _ => budgets.high = Some(clamped_budget),
        }

        let mut bedrock_options = BedrockOptions {
            stream: base,
            reasoning: Some(reasoning),
            tool_choice,
            thinking_budgets: Some(budgets),
            ..BedrockOptions::default()
        };
        bedrock_options.stream.max_tokens = Some(max_tokens);
        return Ok(stream(model, context, bedrock_options));
    }

    Ok(stream(
        model,
        context,
        BedrockOptions {
            stream: base,
            reasoning: Some(reasoning),
            tool_choice,
            thinking_budgets: options.as_ref().and_then(|o| o.thinking_budgets.clone()),
            ..BedrockOptions::default()
        },
    ))
}

/// `ProviderStreams` implementation for `ApiKind::BEDROCK_CONVERSE_STREAM`.
///
/// The trait carries plain [`StreamOptions`]; Bedrock-specific extras
/// ([`BedrockOptions`]) reach [`stream`] only through direct calls or via
/// [`stream_simple`] reasoning mapping (design §3.3 collapses per-API extras).
#[derive(Debug, Clone, Copy, Default)]
pub struct BedrockConverseStream;

impl ProviderStreams for BedrockConverseStream {
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        stream(
            model,
            context,
            BedrockOptions {
                stream: options.unwrap_or_default(),
                ..BedrockOptions::default()
            },
        )
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> Result<AssistantMessageEventStream, String> {
        stream_simple(model, context, options)
    }
}
