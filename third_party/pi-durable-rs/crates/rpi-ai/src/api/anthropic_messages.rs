//! Port of `packages/ai/src/api/anthropic-messages.ts` @ pi 0.82.1 (2efa728);
//! stream-termination semantics (rawStopReason, `sensitive` error text)
//! updated to 4181f66 (926eb15c1, 5a2539a7b); mid-conversation effort /
//! thinking-binding / server-side fallback / input-transformations
//! updated to 9841914 (4e69b0c28, eb1f87fa9 — V14-05).
//!
//! Anthropic Messages API adapter: request construction (system prompt,
//! cache control, thinking modes, tools, deferred tool references), Claude
//! Code identity for OAuth tokens, SSE stream decoding into
//! [`StreamEvent`]s, and `stream_simple` reasoning mapping.
//!
//! Transport note (V14-05 FR-E): upstream computes `getBetaFeatures` into
//! the SDK's `betas` body parameter, which the SDK moves into the
//! `anthropic-beta` request header; rpi hand-writes HTTP and emits that
//! header directly — the equivalent wire form (empty set → no header).
//!
//! Intentional differences (upstream deviations):
//! - HTTP is a direct reqwest call, not the `@anthropic-ai/sdk`; the SDK's
//!   `x-stainless-*` telemetry headers and default `User-Agent` are not sent,
//!   and there is no SDK default timeout (callers set
//!   `StreamOptions::timeout_ms`).
//! - The legacy env var is renamed `PI_CACHE_RETENTION` →
//!   `RPI_CACHE_RETENTION` (requirements §5.5).
//! - `AnthropicOptions.client` (SDK client injection, e.g. AnthropicVertex) is
//!   not ported; there is no SDK client to inject.
//! - SSE events are decoded with the shared [`crate::api::sse::SseDecoder`]
//!   (ported from this file upstream).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::LazyLock;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::api::constrained_sampling::{
    get_json_schema_tool_parameters, resolve_json_schema_strict_sampling_with_check,
};
use crate::api::copilot_headers::{build_copilot_dynamic_headers, has_copilot_vision_input};
use crate::api::simple_options::{
    adjust_max_tokens_for_thinking, build_base_options, clamp_max_tokens_to_context,
};
use crate::api::sse::{ServerSentEvent, SseDecoder};
use crate::api::stream_cancel::{StreamNext, next_chunk_or_cancelled};
use crate::auth::anthropic_federation::OAUTH_API_BETA_HEADER;
use crate::models::ProviderStreams;
use crate::types::{
    AnthropicAllowedFallbackModel, AssistantContent, AssistantMessage, CacheRetention, DoneReason,
    ErrorReason, Message, Model, ModelThinkingLevel, ProviderEnv, ProviderHeaders,
    ProviderResponse, SessionAffinityFormat, SimpleStreamOptions, StopReason, StreamEvent,
    StreamOptions, ThinkingLevel, Tool, ToolResultContent, ToolResultMessage, TranscriptContext,
    Usage, UserContent, UserContentBlock,
};
use crate::utils::cost::calculate_cost;
use crate::utils::custom_fetch::send_provider_request;
use crate::utils::error_body::{NormalizedProviderError, format_provider_error};
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::headers::{
    headers_to_record, merge_headers_chain, model_headers, provider_headers_to_header_map,
    rpi_user_agent_headers,
};
use crate::utils::json_parse::{parse_json_with_repair, parse_streaming_json};
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::provider_retry::{
    ProviderErrorInfo, ProviderRetryOptions, retry_provider_request,
};
use crate::utils::sanitize_unicode::sanitize_surrogates;
use crate::utils::transcript::resolve_transcript;
use crate::utils::transform_messages::transform_messages;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const FINE_GRAINED_TOOL_STREAMING_BETA: &str = "fine-grained-tool-streaming-2025-05-14";
const INTERLEAVED_THINKING_BETA: &str = "interleaved-thinking-2025-05-14";
const SERVER_SIDE_FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const MID_CONVERSATION_OUTPUT_CONFIG_BETA: &str = "mid-conversation-output-config-2026-07-01";
const THINKING_BINDING_CONTROLS_BETA: &str = "thinking-binding-controls-2026-08-01";
/// `INLINE_TOOLS_BETA` (b271b0a52, #9548): later tools are defined by value
/// in `tool_addition` blocks; the top-level list stays fixed across tool
/// changes (prompt cache preservation).
const INLINE_TOOLS_BETA: &str = "inline-tools-2026-09-15";

/// Stable deferred tool declared whenever native tool changes are in use
/// (anthropic-messages.ts:190-200 @ #9548). Anthropic adds hidden prompt
/// scaffolding as soon as any tool has `defer_loading`; declaring this
/// placeholder from the first request keeps that scaffolding in the cached
/// prefix, so the first real late tool does not invalidate the cache. It is
/// never activated and the model cannot see it.
const DEFERRED_TOOL_PLACEHOLDER_NAME: &str = "__pi_deferred_placeholder__";
const DEFERRED_TOOL_PLACEHOLDER_DESCRIPTION: &str =
    "Reserved placeholder. Never available. Never call this.";

/// Effective `allowedFallbackModels` read seam (#9294, b03a367a4 @
/// d1230ea20): the composed model already carries the user models.json
/// `modelOverrides.compat.allowedFallbackModels` value when set — an
/// explicit array replaces the catalog value wholesale, and an empty array
/// disables server-side fallback; absent keeps the catalog value. The
/// beta-header gate, request assembly, and fallback cost attribution all
/// read through here (the per-model override seam V15-07's settings layer
/// reuses in shape).
fn allowed_fallback_models(model: &Model) -> Option<&[AnthropicAllowedFallbackModel]> {
    model
        .compat
        .as_ref()?
        .allowed_fallback_models
        .as_deref()
        .filter(|fallbacks| !fallbacks.is_empty())
}

/// `shouldUseServerSideFallbackBeta` (:184-186, eb1f87fa9).
fn should_use_server_side_fallback_beta(model: &Model) -> bool {
    allowed_fallback_models(model).is_some()
}

/// `getBetaFeatures` (:978-1018 @ 9841914, 4e69b0c28): the single beta-set
/// computation. Order: OAuth pair → fine-grained tool streaming →
/// interleaved thinking (now gated on `model.reasoning &&
/// thinking_enabled == Some(true)`) → server-side fallback → the two
/// mid-convo/binding betas; deduplicated preserving first occurrence.
///
/// An explicit `anthropic-beta` entry in `model.headers` or
/// `options.headers` (case-insensitive; options wins if both carry it)
/// **replaces** the computed set — a `null` value clears it to empty, a
/// string is split on commas, trimmed, emptied entries dropped and
/// deduplicated (:985-1000). rpi's `Model.headers` values are plain
/// strings (no null), so the clear rule is reachable only through
/// `options.headers` (`ProviderHeaders` values are `Option<String>`),
/// matching the type shapes on both sides.
///
/// Transport note: upstream sends this list as the SDK `betas` body
/// parameter, which the SDK moves into the `anthropic-beta` request
/// header; rpi hand-writes HTTP and emits the header directly — the
/// equivalent wire form (empty set → no header).
fn get_beta_features(
    model: &Model,
    context: &TranscriptContext,
    is_oauth_token: bool,
    native_tool_changes: bool,
    options: &AnthropicOptions,
) -> Vec<String> {
    // :985-1000 — explicit `anthropic-beta` overrides the computed set.
    let mut configured: Option<Option<String>> = None;
    if let Some(headers) = &model.headers {
        for (name, value) in headers {
            if name.to_lowercase() == "anthropic-beta" {
                configured = Some(Some(value.clone()));
            }
        }
    }
    if let Some(headers) = options.stream.headers.as_ref() {
        for (name, value) in headers {
            if name.to_lowercase() == "anthropic-beta" {
                configured = Some(value.clone());
            }
        }
    }
    if let Some(configured) = configured {
        return match configured {
            // `null` clears the whole set (:989).
            None => Vec::new(),
            Some(features) => {
                let mut seen: HashSet<&str> = HashSet::new();
                features
                    .split(',')
                    .map(str::trim)
                    .filter(|feature| !feature.is_empty() && seen.insert(feature))
                    .map(str::to_owned)
                    .collect()
            }
        };
    }

    let mut features: Vec<String> = Vec::new();
    if is_oauth_token {
        features.push("claude-code-20250219".to_owned());
        features.push("oauth-2025-04-20".to_owned());
    }
    if should_use_fine_grained_tool_streaming_beta(model, context) {
        features.push(FINE_GRAINED_TOOL_STREAMING_BETA.to_owned());
    }
    if model.reasoning
        && options.thinking_enabled == Some(true)
        && options.interleaved_thinking.unwrap_or(true)
        && model
            .compat
            .as_ref()
            .and_then(|compat| compat.force_adaptive_thinking)
            != Some(true)
    {
        features.push(INTERLEAVED_THINKING_BETA.to_owned());
    }
    if should_use_server_side_fallback_beta(model) {
        features.push(SERVER_SIDE_FALLBACK_BETA.to_owned());
    }
    if model
        .compat
        .as_ref()
        .and_then(|compat| compat.supports_mid_convo_effort)
        == Some(true)
    {
        features.push(MID_CONVERSATION_OUTPUT_CONFIG_BETA.to_owned());
        features.push(THINKING_BINDING_CONTROLS_BETA.to_owned());
    }
    if native_tool_changes {
        features.push(INLINE_TOOLS_BETA.to_owned());
    }
    // :1017 — `[...new Set(features)]` (already unique by construction;
    // kept for parity with the upstream dedup step).
    features
}
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Stealth mode: mimic Claude Code's version in the OAuth user agent
/// (3a624b82d: `2.1.251` → `2.1.280`, anthropic-messages.ts:87).
pub const CLAUDE_CODE_VERSION: &str = "2.1.280";

/// Claude Code 2.x tool names (canonical casing).
/// Source: https://cchistory.mariozechner.at/data/prompts-2.1.11.md
const CLAUDE_CODE_TOOLS: [&str; 17] = [
    "Read",
    "Write",
    "Edit",
    "Bash",
    "Grep",
    "Glob",
    "AskUserQuestion",
    "EnterPlanMode",
    "ExitPlanMode",
    "KillShell",
    "NotebookEdit",
    "Skill",
    "Task",
    "TaskOutput",
    "TodoWrite",
    "WebFetch",
    "WebSearch",
];

static CC_TOOL_LOOKUP: LazyLock<HashMap<String, &'static str>> = LazyLock::new(|| {
    CLAUDE_CODE_TOOLS
        .iter()
        .map(|tool| (tool.to_lowercase(), *tool))
        .collect()
});

/// `toClaudeCodeName`: CC canonical casing if it matches (case-insensitive).
pub fn to_claude_code_name(name: &str) -> String {
    CC_TOOL_LOOKUP
        .get(&name.to_lowercase())
        .map(|canonical| (*canonical).to_owned())
        .unwrap_or_else(|| name.to_owned())
}

/// `fromClaudeCodeName`: map a (canonical) CC name back to the tool's own
/// casing from the current tool list.
pub fn from_claude_code_name(name: &str, tools: Option<&[Tool]>) -> String {
    if let Some(tools) = tools
        && !tools.is_empty()
    {
        let lower_name = name.to_lowercase();
        if let Some(matched) = tools
            .iter()
            .find(|tool| tool.name.to_lowercase() == lower_name)
        {
            return matched.name.clone();
        }
    }
    name.to_owned()
}

const ANTHROPIC_MESSAGE_EVENTS: [&str; 6] = [
    "message_start",
    "message_delta",
    "message_stop",
    "content_block_start",
    "content_block_delta",
    "content_block_stop",
];

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// `AnthropicThinkingDisplay = "summarized" | "omitted"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicThinkingDisplay {
    Summarized,
    Omitted,
}

impl AnthropicThinkingDisplay {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Summarized => "summarized",
            Self::Omitted => "omitted",
        }
    }
}

/// `AnthropicOptions.toolChoice`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnthropicToolChoice {
    Auto,
    Any,
    None,
    /// `{ type: "tool", name }` — force a specific tool.
    Tool {
        name: String,
    },
}

/// `AnthropicOptions` — `StreamOptions` plus Anthropic-specific extras.
///
/// `effort` carries the canonical adaptive-thinking effort values
/// (`low|medium|high|xhigh|max`); values from
/// `Model::thinking_level_map` pass through verbatim, matching upstream's
/// unchecked cast.
#[derive(Debug, Clone, Default)]
pub struct AnthropicOptions {
    pub stream: StreamOptions,
    /// Enable extended thinking. `None` omits the `thinking` parameter.
    pub thinking_enabled: Option<bool>,
    /// Token budget for budget-based thinking (older models). Default: 1024.
    pub thinking_budget_tokens: Option<u32>,
    /// Effort level for adaptive-thinking models.
    pub effort: Option<String>,
    /// Thinking content display mode. Default: `summarized` when enabled.
    pub thinking_display: Option<AnthropicThinkingDisplay>,
    /// Whether to request the interleaved-thinking beta header. Default: true.
    pub interleaved_thinking: Option<bool>,
    pub tool_choice: Option<AnthropicToolChoice>,
}

// ---------------------------------------------------------------------------
// Compat
// ---------------------------------------------------------------------------

/// `Required<Omit<AnthropicMessagesCompat, "forceAdaptiveThinking">>` —
/// `sessionAffinityFormat` stays optional (unset means the default
/// `x-session-affinity` header; #9102).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedAnthropicCompat {
    pub supports_eager_tool_input_streaming: bool,
    pub supports_long_cache_retention: bool,
    pub send_session_affinity_headers: bool,
    pub session_affinity_format: Option<SessionAffinityFormat>,
    pub supports_cache_control_on_tools: bool,
    pub supports_temperature: bool,
    pub allow_empty_signature: bool,
    pub supports_strict_tools: bool,
    /// #9548: system messages after the conversation started (default
    /// false; catalog bakes per model).
    pub supports_mid_convo_system_messages: bool,
    /// #9548: the transport accepts `tool_addition`/`tool_removal` blocks
    /// (first-party Anthropic only; proxies reject them).
    pub supports_mid_convo_tool_changes: bool,
}

/// `getAnthropicCompat` (#9102, bbb61e34a: OpenRouter endpoints derive
/// session affinity headers by default, with the OpenRouter header format).
pub fn get_anthropic_compat(model: &Model) -> ResolvedAnthropicCompat {
    let compat = model.compat.as_ref();
    let is_openrouter =
        crate::api::session_affinity::is_openrouter(&model.provider, &model.base_url);
    ResolvedAnthropicCompat {
        supports_eager_tool_input_streaming: compat
            .and_then(|c| c.supports_eager_tool_input_streaming)
            .unwrap_or(true),
        supports_long_cache_retention: compat
            .and_then(|c| c.supports_long_cache_retention)
            .unwrap_or(true),
        send_session_affinity_headers: compat
            .and_then(|c| c.send_session_affinity_headers)
            .unwrap_or(is_openrouter),
        session_affinity_format: compat
            .and_then(|c| c.session_affinity_format)
            .or_else(|| is_openrouter.then_some(SessionAffinityFormat::Openrouter)),
        supports_cache_control_on_tools: compat
            .and_then(|c| c.supports_cache_control_on_tools)
            .unwrap_or(true),
        supports_temperature: compat.and_then(|c| c.supports_temperature).unwrap_or(true),
        allow_empty_signature: compat
            .and_then(|c| c.allow_empty_signature)
            .unwrap_or(false),
        supports_strict_tools: compat
            .and_then(|c| c.supports_strict_tools)
            .unwrap_or(false),
        supports_mid_convo_system_messages: compat
            .and_then(|c| c.supports_mid_convo_system_messages)
            .unwrap_or(false),
        supports_mid_convo_tool_changes: compat
            .and_then(|c| c.supports_mid_convo_tool_changes)
            .unwrap_or(false),
    }
}

// ---------------------------------------------------------------------------
// Cache retention
// ---------------------------------------------------------------------------

/// `resolveCacheRetention`. Defaults to "short"; the `RPI_CACHE_RETENTION`
/// env var (upstream: `PI_CACHE_RETENTION`) opts into "long".
pub fn resolve_cache_retention(
    cache_retention: Option<CacheRetention>,
    env: Option<&ProviderEnv>,
) -> CacheRetention {
    if let Some(cache_retention) = cache_retention {
        return cache_retention;
    }
    if get_provider_env_value("RPI_CACHE_RETENTION", env).as_deref() == Some("long") {
        return CacheRetention::Long;
    }
    CacheRetention::Short
}

/// `getCacheControl`: the `cache_control` JSON block for the resolved
/// retention (`{"type":"ephemeral"}`, plus `"ttl":"1h"` for long retention on
/// supporting models). `None` retention yields `None`.
pub fn get_cache_control(
    model: &Model,
    cache_retention: Option<CacheRetention>,
    env: Option<&ProviderEnv>,
) -> (CacheRetention, Option<Value>) {
    let retention = resolve_cache_retention(cache_retention, env);
    if retention == CacheRetention::None {
        return (retention, None);
    }
    let long = retention == CacheRetention::Long
        && get_anthropic_compat(model).supports_long_cache_retention;
    let cache_control = if long {
        json!({"type": "ephemeral", "ttl": "1h"})
    } else {
        json!({"type": "ephemeral"})
    };
    (retention, Some(cache_control))
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

fn has_header(headers: Option<&ProviderHeaders>, name: &str) -> bool {
    let Some(headers) = headers else { return false };
    let expected = name.to_lowercase();
    headers.iter().any(|(key, value)| {
        key.to_lowercase() == expected
            && value.as_ref().is_some_and(|value| !value.trim().is_empty())
    })
}

/// `assertRequestAuth`: an explicit API key or an auth-bearing header must be
/// present.
pub fn assert_request_auth(
    provider: &str,
    api_key: Option<&str>,
    headers: Option<&ProviderHeaders>,
) -> Result<(), String> {
    if api_key.is_some() {
        return Ok(());
    }
    if has_header(headers, "authorization")
        || has_header(headers, "x-api-key")
        || has_header(headers, "cf-aig-authorization")
    {
        return Ok(());
    }
    Err(format!("No API key for provider: {provider}"))
}

fn is_oauth_token(api_key: &str) -> bool {
    api_key.contains("sk-ant-oat")
}

// Header construction note: `merge_headers_chain` mirrors the
// Anthropic/OpenAI SDK `buildHeaders` net semantics (case-insensitive, later
// sources win, `None` suppresses). Auth headers sit in the base position
// because both SDKs apply `authHeaders` *before* `defaultHeaders` (user
// headers override key-derived auth).

/// `createClient` (header construction only): resolves the auth mode and the
/// full header set. `federation_token` is the workload-identity-federation
/// access token minted by
/// [`crate::auth::anthropic_federation`]: it becomes the `Authorization`
/// bearer and appends the OAuth beta header, like the SDK's own credential
/// provider. Returns the merged headers and whether the token is an OAuth
/// token.
#[allow(clippy::too_many_arguments)] // mirrors the upstream createClient inputs
fn build_request_headers(
    model: &Model,
    context: &TranscriptContext,
    native_tool_changes: bool,
    api_key: Option<&str>,
    options: &AnthropicOptions,
    dynamic_headers: Option<HashMap<String, String>>,
    session_id: Option<&str>,
    federation_token: Option<&str>,
) -> (ProviderHeaders, bool) {
    let compat = get_anthropic_compat(model);
    // The beta set comes from the single `getBetaFeatures` computation
    // (4e69b0c28) — OAuth/Claude-Code identity headers still gate on the
    // token shape.
    let is_oauth = api_key.is_some_and(is_oauth_token);
    let beta_features = get_beta_features(model, context, is_oauth, native_tool_changes, options);
    let beta_header = (!beta_features.is_empty()).then(|| beta_features.join(","));

    let dynamic_headers: Option<ProviderHeaders> = dynamic_headers.map(|headers| {
        headers
            .into_iter()
            .map(|(key, value)| (key, Some(value)))
            .collect()
    });

    let mut base: ProviderHeaders = [
        ("accept".to_owned(), Some("application/json".to_owned())),
        (
            "anthropic-dangerous-direct-browser-access".to_owned(),
            Some("true".to_owned()),
        ),
        (
            "anthropic-version".to_owned(),
            Some(ANTHROPIC_VERSION.to_owned()),
        ),
    ]
    .into();

    // Copilot: Bearer auth, selective betas.
    if model.provider == "github-copilot" {
        if let Some(betas) = &beta_header {
            base.insert("anthropic-beta".to_owned(), Some(betas.clone()));
        }
        if let Some(api_key) = api_key {
            base.insert(
                "authorization".to_owned(),
                Some(format!("Bearer {api_key}")),
            );
        }
        let headers = merge_headers_chain(&[
            rpi_user_agent_headers(),
            Some(base),
            model_headers(model),
            dynamic_headers,
            options.stream.headers.clone(),
        ]);
        return (headers, false);
    }

    // OAuth: Bearer auth, Claude Code identity headers.
    if let Some(api_key) = api_key
        && is_oauth
    {
        if let Some(betas) = &beta_header {
            base.insert("anthropic-beta".to_owned(), Some(betas.clone()));
        }
        base.insert(
            "user-agent".to_owned(),
            Some(format!("claude-cli/{CLAUDE_CODE_VERSION}")),
        );
        base.insert("x-app".to_owned(), Some("cli".to_owned()));
        base.insert(
            "authorization".to_owned(),
            Some(format!("Bearer {api_key}")),
        );
        let headers = merge_headers_chain(&[
            rpi_user_agent_headers(),
            Some(base),
            model_headers(model),
            options.stream.headers.clone(),
        ]);
        return (headers, true);
    }

    // Workload identity federation: bearer token + the OAuth beta header,
    // appended to any model-provided betas like the SDK client's
    // `prepareRequest` (`@anthropic-ai/sdk/lib/client.js:435-453`; the Bearer
    // comes from `authHeaders`, `:355-362`). The upstream fake-SDK test
    // asserts `createParams.betas` does not carry the header at the SDK
    // options layer — rpi has no SDK options object and implements the wire
    // behavior directly.
    if let Some(token) = federation_token {
        let mut betas = beta_features.clone();
        if !betas.iter().any(|feature| feature == OAUTH_API_BETA_HEADER) {
            betas.push(OAUTH_API_BETA_HEADER.to_owned());
        }
        base.insert("anthropic-beta".to_owned(), Some(betas.join(",")));
        base.insert("authorization".to_owned(), Some(format!("Bearer {token}")));
        let session_affinity_headers =
            crate::api::session_affinity::anthropic_session_affinity_headers(
                compat.send_session_affinity_headers,
                compat.session_affinity_format,
                session_id,
            );
        let headers = merge_headers_chain(&[
            rpi_user_agent_headers(),
            Some(base),
            session_affinity_headers,
            model_headers(model),
            options.stream.headers.clone(),
        ]);
        return (headers, false);
    }

    // API key or header-owned auth.
    if let Some(betas) = &beta_header {
        base.insert("anthropic-beta".to_owned(), Some(betas.clone()));
    }
    if let Some(api_key) = api_key {
        base.insert("x-api-key".to_owned(), Some(api_key.to_owned()));
    }
    let session_affinity_headers = crate::api::session_affinity::anthropic_session_affinity_headers(
        compat.send_session_affinity_headers,
        compat.session_affinity_format,
        session_id,
    );
    let headers = merge_headers_chain(&[
        rpi_user_agent_headers(),
        Some(base),
        session_affinity_headers,
        model_headers(model),
        options.stream.headers.clone(),
    ]);
    (headers, false)
}

// ---------------------------------------------------------------------------
// Message conversion
// ---------------------------------------------------------------------------

/// `normalizeToolCallId`: Anthropic-required pattern and length. The allowed
/// set is ASCII, so `chars().take(64)` matches JS `slice(0, 64)`.
pub fn normalize_tool_call_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

/// `convertContentBlocks`: text-only content collapses to a joined string;
/// images force a block array, prepending a placeholder when no text exists.
fn convert_content_blocks(content: &[ToolResultContent]) -> Value {
    let has_images = content
        .iter()
        .any(|block| matches!(block, ToolResultContent::Image(_)));
    if !has_images {
        let joined = content
            .iter()
            .map(|block| match block {
                ToolResultContent::Text(text) => text.text.as_str(),
                // invariant: has_images is false here
                ToolResultContent::Image(_) => "",
            })
            .collect::<Vec<_>>()
            .join("\n");
        return json!(sanitize_surrogates(&joined));
    }

    let mut blocks: Vec<Value> = content
        .iter()
        .map(|block| match block {
            ToolResultContent::Text(text) => {
                json!({"type": "text", "text": sanitize_surrogates(&text.text)})
            }
            ToolResultContent::Image(image) => json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": image.mime_type,
                    "data": image.data,
                },
            }),
        })
        .collect();
    let has_text = blocks
        .iter()
        .any(|block| block.get("type").and_then(Value::as_str) == Some("text"));
    if !has_text {
        blocks.insert(0, json!({"type": "text", "text": "(see attached image)"}));
    }
    Value::Array(blocks)
}

/// `convertToolResult` (post-#9548): a plain tool_result block. Deferred
/// tool loading no longer rides tool results — later tool declarations
/// travel as system-message `tool_addition` blocks instead.
fn convert_tool_result(msg: &ToolResultMessage) -> Value {
    let converted_content = convert_content_blocks(&msg.content);
    json!({
        "type": "tool_result",
        "tool_use_id": msg.tool_call_id,
        "content": converted_content,
        "is_error": msg.is_error,
    })
}

/// `convertMessages`.
/// `ConvertedAnthropicMessages` (:1325-1328): the converted wire messages
/// plus, for managed-effort models, the recorded historical effort level per
/// assistant message index (4e69b0c28).
struct ConvertedAnthropicMessages {
    messages: Vec<Value>,
    assistant_levels: HashMap<usize, String>,
}

/// `isAnthropicEffort` (:1400-1402).
fn is_anthropic_effort(value: Option<&str>) -> bool {
    matches!(value, Some("low" | "medium" | "high" | "xhigh" | "max"))
}

/// `insertThinkingLevelMessages` (:1404-1417): before every historical
/// assistant message whose effort was recorded, insert an effort-only
/// system message; always append the current turn's marker last.
fn insert_thinking_level_messages(
    converted: ConvertedAnthropicMessages,
    active_effort: &str,
) -> Vec<Value> {
    let mut messages: Vec<Value> = Vec::new();
    for (index, message) in converted.messages.into_iter().enumerate() {
        if let Some(historical_effort) = converted.assistant_levels.get(&index) {
            messages.push(json!({
                "role": "system",
                "content": [],
                "output_config": {"effort": historical_effort},
            }));
        }
        messages.push(message);
    }
    messages.push(json!({
        "role": "system",
        "content": [],
        "output_config": {"effort": active_effort},
    }));
    messages
}

/// `convertToolDefinitions` callback (b271b0a52): converts tool declarations
/// into wire definitions for inline `tool_addition` blocks.
type ConvertToolDefinitions<'a> = &'a dyn Fn(&[Tool]) -> Result<Vec<Value>, String>;

/// `convertMessages` (transcript.ts). `convert_tool_definitions` converts
/// tool definitions for native `tool_addition` blocks; `None` keeps
/// mid-conversation tool changes non-native (b271b0a52).
fn convert_messages(
    transformed_messages: &[Message],
    is_oauth_token: bool,
    cache_control: Option<&Value>,
    allow_empty_signature: bool,
    convert_tool_definitions: Option<ConvertToolDefinitions<'_>>,
    managed_provider: Option<&str>,
) -> Result<ConvertedAnthropicMessages, String> {
    let mut params: Vec<Value> = Vec::new();
    let mut assistant_levels: HashMap<usize, String> = HashMap::new();
    // Later system messages are held back and emitted directly before the
    // next assistant message (or at the end of the transcript). Anthropic
    // requires `tool_result` blocks to immediately follow their `tool_use`,
    // so a system message between them is rejected; this also mirrors where
    // the managed-effort system messages are inserted. As a result an update
    // placed before a user message in the transcript lands after it on the
    // wire (#9548, anthropic-messages.ts:1236-1244).
    let mut pending_system_messages: Vec<Value> = Vec::new();

    let mut i = 0;
    while i < transformed_messages.len() {
        let msg = &transformed_messages[i];
        match msg {
            Message::System(system) => {
                // Later system messages only reach this point when the model
                // accepts them natively; otherwise the transcript was
                // collapsed into the leading message before conversion.
                let text = crate::utils::text::render_system_message_update(system);
                let mut blocks: Vec<Value> = Vec::new();
                if !text.is_empty() {
                    blocks.push(json!({
                        "type": "text",
                        "text": sanitize_surrogates(&text),
                    }));
                }
                if let Some(convert_tool_definitions) = convert_tool_definitions {
                    let added = system.tools_added.as_deref().unwrap_or(&[]);
                    // A new definition under the same name replaces the old
                    // one, so no removal is needed (b271b0a52).
                    let redefined: std::collections::HashSet<&str> =
                        added.iter().map(|tool| tool.name.as_str()).collect();
                    if let Some(removed) = &system.tools_removed {
                        for tool in removed {
                            if redefined.contains(tool.name.as_str()) {
                                continue;
                            }
                            blocks.push(json!({
                                "type": "tool_removal",
                                "tool": {
                                    "type": "tool_reference",
                                    "name": if is_oauth_token { to_claude_code_name(&tool.name) } else { tool.name.clone() },
                                },
                            }));
                        }
                    }
                    for definition in convert_tool_definitions(added)? {
                        blocks.push(json!({
                            "type": "tool_addition",
                            "tool": {"type": "tool_definition", "definition": definition},
                        }));
                    }
                }
                if !blocks.is_empty() {
                    pending_system_messages.push(json!({
                        "role": "system",
                        "content": blocks,
                    }));
                }
            }
            Message::User(user) => match &user.content {
                UserContent::Text(text) => {
                    if !text.trim().is_empty() {
                        params.push(json!({
                            "role": "user",
                            "content": sanitize_surrogates(text),
                        }));
                    }
                }
                UserContent::Blocks(content) => {
                    let blocks: Vec<Value> = content
                        .iter()
                        .map(|item| match item {
                            UserContentBlock::Text(text) => json!({
                                "type": "text",
                                "text": sanitize_surrogates(&text.text),
                            }),
                            UserContentBlock::Image(image) => json!({
                                "type": "image",
                                "source": {
                                    "type": "base64",
                                    "media_type": image.mime_type,
                                    "data": image.data,
                                },
                            }),
                        })
                        .filter(|block| {
                            block.get("type").and_then(Value::as_str) != Some("text")
                                || !block["text"].as_str().unwrap_or("").trim().is_empty()
                        })
                        .collect();
                    if blocks.is_empty() {
                        i += 1;
                        continue;
                    }
                    params.push(json!({"role": "user", "content": blocks}));
                }
            },
            Message::Assistant(assistant) => {
                params.append(&mut pending_system_messages);
                let mut blocks: Vec<Value> = Vec::new();
                for block in &assistant.content {
                    match block {
                        AssistantContent::Text(text) => {
                            if text.text.trim().is_empty() {
                                continue;
                            }
                            blocks.push(json!({
                                "type": "text",
                                "text": sanitize_surrogates(&text.text),
                            }));
                        }
                        AssistantContent::Thinking(thinking) => {
                            // Redacted thinking: pass the opaque payload back.
                            if thinking.redacted.unwrap_or(false) {
                                let mut redacted = json!({"type": "redacted_thinking"});
                                if let Some(signature) = &thinking.thinking_signature {
                                    redacted["data"] = json!(signature);
                                }
                                blocks.push(redacted);
                                continue;
                            }
                            let signature = thinking.thinking_signature.as_deref().unwrap_or("");
                            let has_signature = !signature.trim().is_empty();
                            if thinking.thinking.trim().is_empty() && !has_signature {
                                continue;
                            }
                            // Missing/empty signature (e.g. aborted stream):
                            // convert to plain text unless the model is marked
                            // as accepting empty signatures.
                            if !has_signature {
                                if allow_empty_signature {
                                    blocks.push(json!({
                                        "type": "thinking",
                                        "thinking": sanitize_surrogates(&thinking.thinking),
                                        "signature": "",
                                    }));
                                } else {
                                    blocks.push(json!({
                                        "type": "text",
                                        "text": sanitize_surrogates(&thinking.thinking),
                                    }));
                                }
                            } else {
                                blocks.push(json!({
                                    "type": "thinking",
                                    "thinking": sanitize_surrogates(&thinking.thinking),
                                    "signature": signature,
                                }));
                            }
                        }
                        AssistantContent::ToolCall(call) => {
                            blocks.push(json!({
                                "type": "tool_use",
                                "id": call.id,
                                "name": if is_oauth_token { to_claude_code_name(&call.name) } else { call.name.clone() },
                                "input": call.arguments,
                            }));
                        }
                    }
                }
                if blocks.is_empty() {
                    i += 1;
                    continue;
                }
                let message_index = params.len();
                params.push(json!({"role": "assistant", "content": blocks}));
                // :1339-1346 — only same-provider anthropic-messages
                // assistants with a valid recorded effort replay a marker
                // (4e69b0c28).
                if let Some(managed_provider) = managed_provider
                    && assistant.api == crate::types::ApiKind::from("anthropic-messages")
                    && assistant.provider == managed_provider
                    && is_anthropic_effort(assistant.provider_thinking_level.as_deref())
                    && let Some(level) = &assistant.provider_thinking_level
                {
                    assistant_levels.insert(message_index, level.clone());
                }
            }
            Message::ToolResult(_) => {
                // Collect all consecutive toolResult messages (z.ai Anthropic
                // endpoint requires them grouped in one user message).
                let mut tool_results: Vec<Value> = Vec::new();
                let mut j = i;
                while j < transformed_messages.len() {
                    let Message::ToolResult(result) = &transformed_messages[j] else {
                        break;
                    };
                    tool_results.push(convert_tool_result(result));
                    j += 1;
                }
                i = j - 1;
                params.push(json!({"role": "user", "content": tool_results}));
            }
        }
        i += 1;
    }

    params.append(&mut pending_system_messages);

    // Add cache_control to the last user or system message to cache
    // conversation history (#9548 also allows the trailing pending system
    // message to carry the breakpoint; tool_addition/tool_removal blocks
    // included).
    if let (Some(cache_control), Some(last_message)) = (cache_control, params.last_mut())
        && matches!(
            last_message.get("role").and_then(Value::as_str),
            Some("user") | Some("system")
        )
    {
        match last_message.get_mut("content") {
            Some(Value::Array(blocks)) => {
                if let Some(last_block) = blocks.last_mut()
                    && matches!(
                        last_block.get("type").and_then(Value::as_str),
                        Some("text")
                            | Some("image")
                            | Some("tool_result")
                            | Some("tool_addition")
                            | Some("tool_removal")
                    )
                {
                    last_block["cache_control"] = cache_control.clone();
                }
            }
            Some(Value::String(text)) => {
                let text = text.clone();
                last_message["content"] = json!([{
                    "type": "text",
                    "text": text,
                    "cache_control": cache_control,
                }]);
            }
            _ => {}
        }
    }

    Ok(ConvertedAnthropicMessages {
        messages: params,
        assistant_levels,
    })
}

fn should_use_fine_grained_tool_streaming_beta(model: &Model, context: &TranscriptContext) -> bool {
    !crate::utils::transcript::get_current_tools(&context.messages).is_empty()
        && !get_anthropic_compat(model).supports_eager_tool_input_streaming
}

/// Keywords Anthropic strict tool use rejects with a 400 for the whole
/// request (295cc72b0, #9953).
const ANTHROPIC_STRICT_UNSUPPORTED_KEYWORDS: [&str; 11] = [
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "maxItems",
    "uniqueItems",
    "minContains",
    "maxContains",
    "minProperties",
    "maxProperties",
];

const ANTHROPIC_STRICT_STRING_FORMATS: [&str; 10] = [
    "date-time",
    "time",
    "date",
    "duration",
    "email",
    "hostname",
    "uri",
    "ipv4",
    "ipv6",
    "uuid",
];

/// `isAnthropicStrictUnsupportedKeyword` (anthropic-messages.ts @ 295cc72b0):
/// schema keywords Anthropic strict mode rejects.
fn is_anthropic_strict_unsupported_keyword(key: &str, value: &Value) -> bool {
    if ANTHROPIC_STRICT_UNSUPPORTED_KEYWORDS.contains(&key) {
        return true;
    }
    if key == "minItems" {
        return value != &json!(0) && value != &json!(1);
    }
    if key == "format" {
        return !value
            .as_str()
            .is_some_and(|format| ANTHROPIC_STRICT_STRING_FORMATS.contains(&format));
    }
    false
}

/// `convertTools`.
fn convert_tools(
    tools: &[Tool],
    is_oauth_token: bool,
    supports_eager_tool_input_streaming: bool,
    supports_strict_tools: bool,
    cache_control: Option<&Value>,
    defer_loading: bool,
) -> Result<Vec<Value>, String> {
    tools
        .iter()
        .enumerate()
        .map(|(index, tool)| {
            let strict = resolve_json_schema_strict_sampling_with_check(
                tool,
                supports_strict_tools,
                Some(is_anthropic_strict_unsupported_keyword),
            )?;
            // `getJsonSchemaToolParameters` (7915cdac6): the strict-converted
            // parameters feed both the direct schema and the legacy view.
            let schema = get_json_schema_tool_parameters(tool, strict)?;
            let legacy_input_schema = json!({
                "type": "object",
                "properties": schema.get("properties").cloned().unwrap_or_else(|| json!({})),
                "required": schema.get("required").cloned().unwrap_or_else(|| json!([])),
            });
            let input_schema = if strict == Some(true) {
                // { ...parameters, ...legacyInputSchema } — legacy wins.
                let mut base = schema.as_object().cloned().unwrap_or_default();
                if let Value::Object(legacy) = &legacy_input_schema {
                    for (key, value) in legacy {
                        base.insert(key.clone(), value.clone());
                    }
                }
                Value::Object(base)
            } else {
                legacy_input_schema
            };

            let mut converted = json!({
                "name": if is_oauth_token { to_claude_code_name(&tool.name) } else { tool.name.clone() },
                "description": tool.description,
                "input_schema": input_schema,
            });
            if supports_eager_tool_input_streaming {
                converted["eager_input_streaming"] = json!(true);
            }
            if strict == Some(true) {
                converted["strict"] = json!(true);
            }
            if defer_loading {
                converted["defer_loading"] = json!(true);
            }
            if let (Some(cache_control), true) = (cache_control, index == tools.len() - 1) {
                converted["cache_control"] = cache_control.clone();
            }
            Ok(converted)
        })
        .collect()
}

fn text_block(text: &str, cache_control: Option<&Value>) -> Value {
    let mut block = json!({"type": "text", "text": text});
    if let Some(cache_control) = cache_control {
        block["cache_control"] = cache_control.clone();
    }
    block
}

/// `buildParams` (#9548 restructure): the leading system message supplies the
/// system text; conversation messages exclude it; native tool changes keep
/// the initial tools active and defer later ones in place.
fn build_params(
    model: &Model,
    context: &TranscriptContext,
    is_oauth_token: bool,
    native_tool_changes: bool,
    options: &AnthropicOptions,
) -> Result<Value, String> {
    let (_retention, cache_control) = get_cache_control(
        model,
        options.stream.cache_retention,
        options.stream.env.as_ref(),
    );
    let compat = get_anthropic_compat(model);
    let initial_system_message =
        crate::utils::transcript::get_initial_system_message(&context.messages);
    let initial_system_text = initial_system_message
        .map(crate::utils::text::get_system_message_text)
        .unwrap_or_default();
    let transformed_messages = transform_messages(
        &context.messages,
        model,
        Some(&mut |id: &str, _model: &Model, _msg: &AssistantMessage| normalize_tool_call_id(id)),
    );
    // The transformed list mirrors the input order; drop the leading system
    // message if present so the prompt rides `params.system` instead.
    let conversation_messages = match transformed_messages.first() {
        Some(Message::System(_)) => &transformed_messages[1..],
        _ => transformed_messages.as_slice(),
    };
    // Native tool changes keep the request-level tool list fixed and define
    // every later tool by value in a `tool_addition` block, which also
    // expresses same-name redefinitions. Anthropic rejects a tool list where
    // every tool is deferred, so there must be an initial active tool to
    // anchor the placeholder. Otherwise the current tool list is sent
    // (b271b0a52, #9548).
    let initial_tools: Vec<Tool> = initial_system_message
        .and_then(|message| message.tools_added.clone())
        .unwrap_or_default();

    // Managed-effort models (4e69b0c28): replay effort markers around the
    // history and pin the request-level controls below (:1053-1060).
    let managed = model
        .compat
        .as_ref()
        .and_then(|compat| compat.supports_mid_convo_effort)
        == Some(true);
    // Inline tool definitions for `tool_addition` blocks: the same converter
    // the request-level list uses, without cache control or deferral.
    let convert_tool_definitions = |tools: &[Tool]| -> Result<Vec<Value>, String> {
        convert_tools(
            tools,
            is_oauth_token,
            compat.supports_eager_tool_input_streaming,
            compat.supports_strict_tools,
            None,
            false,
        )
    };
    let converted = convert_messages(
        conversation_messages,
        is_oauth_token,
        cache_control.as_ref(),
        compat.allow_empty_signature,
        native_tool_changes.then_some(&convert_tool_definitions as _),
        managed.then_some(model.provider.as_str()),
    )?;
    let messages = if managed {
        insert_thinking_level_messages(converted, options.effort.as_deref().unwrap_or("high"))
    } else {
        converted.messages
    };
    let mut params = json!({
        "model": model.id,
        "messages": messages,
        "max_tokens": options.stream.max_tokens.unwrap_or(model.max_tokens),
        "stream": true,
    });
    // The beta set rides the `anthropic-beta` HEADER only
    // (build_request_headers) — the upstream SDK moves its `betas` body
    // parameter into that header, so the wire body never carries a
    // top-level `betas` array; a strict gateway would reject the unknown
    // body field. Do not re-add `params["betas"]`.

    // For OAuth tokens, we MUST include the Claude Code identity.
    if is_oauth_token {
        let mut system = vec![text_block(
            "You are Claude Code, Anthropic's official CLI for Claude.",
            cache_control.as_ref(),
        )];
        if !initial_system_text.is_empty() {
            system.push(text_block(
                sanitize_surrogates(&initial_system_text),
                cache_control.as_ref(),
            ));
        }
        params["system"] = Value::Array(system);
    } else if !initial_system_text.is_empty() {
        // Add cache control to system prompt for non-OAuth tokens.
        params["system"] = json!([text_block(
            sanitize_surrogates(&initial_system_text),
            cache_control.as_ref(),
        )]);
    }

    // Temperature is incompatible with extended thinking and unsupported on
    // Claude Opus 4.7+ / managed-effort models (:1090-1097).
    if let Some(temperature) = options.stream.temperature
        && options.thinking_enabled != Some(true)
        && !managed
        && compat.supports_temperature
    {
        params["temperature"] = json!(temperature);
    }

    let tool_cache_control = if compat.supports_cache_control_on_tools {
        cache_control.as_ref()
    } else {
        None
    };
    if native_tool_changes {
        // Initial tools stay active with the cache breakpoint on the last
        // one, followed by the placeholder. The list never changes
        // afterwards: later tools are defined by value in `tool_addition`
        // blocks and withdrawn by `tool_removal`, so the cached prefix
        // survives every tool change (b271b0a52, #9548).
        let mut tools = convert_tools(
            &initial_tools,
            is_oauth_token,
            compat.supports_eager_tool_input_streaming,
            compat.supports_strict_tools,
            tool_cache_control,
            false,
        )?;
        tools.push(json!({
            "name": DEFERRED_TOOL_PLACEHOLDER_NAME,
            "description": DEFERRED_TOOL_PLACEHOLDER_DESCRIPTION,
            "input_schema": {"type": "object", "properties": {}, "required": []},
            "defer_loading": true,
        }));
        params["tools"] = Value::Array(tools);
    } else {
        let tools = crate::utils::transcript::get_current_tools(&context.messages);
        if !tools.is_empty() {
            params["tools"] = Value::Array(convert_tools(
                &tools,
                is_oauth_token,
                compat.supports_eager_tool_input_streaming,
                compat.supports_strict_tools,
                tool_cache_control,
                false,
            )?);
        }
    }

    // Configure thinking mode: adaptive, budget-based, or explicitly disabled.
    // Managed effort models always use adaptive thinking so prefix mismatches
    // can be dropped instead of surfacing as persistent 400 responses
    // (:1123-1131, 4e69b0c28).
    if managed {
        let display = options
            .thinking_display
            .unwrap_or(AnthropicThinkingDisplay::Summarized)
            .as_str();
        params["thinking"] = json!({
            "type": "adaptive",
            "display": display,
            "block_binding": {"prefix_mismatch_behavior": "drop_block"},
        });
        params["output_config"] = json!({"effort": "high"});
    } else if model.reasoning {
        if options.thinking_enabled == Some(true) {
            // Default to "summarized" so Opus 4.7 and Mythos Preview behave
            // like older Claude 4 models (whose API default is also
            // "summarized").
            let display = options
                .thinking_display
                .unwrap_or(AnthropicThinkingDisplay::Summarized)
                .as_str();
            if model
                .compat
                .as_ref()
                .and_then(|c| c.force_adaptive_thinking)
                == Some(true)
            {
                // Adaptive thinking: Claude decides when and how much to think.
                params["thinking"] = json!({"type": "adaptive", "display": display});
                if let Some(effort) = &options.effort {
                    params["output_config"] = json!({"effort": effort});
                }
            } else {
                // Budget-based thinking for older models.
                let budget = options
                    .thinking_budget_tokens
                    .filter(|budget| *budget > 0)
                    .unwrap_or(1024);
                params["thinking"] = json!({
                    "type": "enabled",
                    "budget_tokens": budget,
                    "display": display,
                });
            }
        } else if options.thinking_enabled == Some(false)
            && model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.get(&ModelThinkingLevel::Off))
                != Some(&None)
        {
            params["thinking"] = json!({"type": "disabled"});
        }
    }

    if let Some(metadata) = &options.stream.metadata
        && let Some(user_id) = metadata.get("user_id").and_then(Value::as_str)
    {
        params["metadata"] = json!({"user_id": user_id});
    }

    if let Some(tool_choice) = &options.tool_choice {
        params["tool_choice"] = match tool_choice {
            AnthropicToolChoice::Auto => json!({"type": "auto"}),
            AnthropicToolChoice::Any => json!({"type": "any"}),
            AnthropicToolChoice::None => json!({"type": "none"}),
            AnthropicToolChoice::Tool { name } => json!({"type": "tool", "name": name}),
        };
    }

    // Server-side refusal fallback (eb1f87fa9, :1168-1172): only when the
    // compat lists permitted targets — Anthropic rejects `fallbacks` for
    // models with none (user override / disable via #9294 flows through
    // the same seam).
    if let Some(fallbacks) = allowed_fallback_models(model) {
        params["fallbacks"] = Value::Array(
            fallbacks
                .iter()
                .map(|fallback| json!({"model": fallback.model}))
                .collect(),
        );
    }

    Ok(params)
}

// ---------------------------------------------------------------------------
// Stop reason mapping
// ---------------------------------------------------------------------------

/// `mapStopReason`; unknown stop reasons are an error (the API may add new
/// values).
fn map_stop_reason(
    reason: &str,
    stop_details: Option<&Value>,
) -> Result<(StopReason, Option<String>), String> {
    match reason {
        "end_turn" => Ok((StopReason::Stop, None)),
        "max_tokens" => Ok((StopReason::Length, None)),
        "tool_use" => Ok((StopReason::ToolUse, None)),
        "refusal" => Ok((
            StopReason::Error,
            Some(
                stop_details
                    .and_then(|details| details.get("explanation"))
                    .and_then(Value::as_str)
                    .unwrap_or("The model refused to complete the request")
                    .to_owned(),
            ),
        )),
        // Stop is good enough -> resubmit.
        "pause_turn" => Ok((StopReason::Stop, None)),
        // We don't supply stop sequences, so this should never happen.
        "stop_sequence" => Ok((StopReason::Stop, None)),
        // Content flagged by safety filters (not yet in SDK types).
        "sensitive" => Ok((
            StopReason::Error,
            Some("Provider stopped with: sensitive".to_owned()),
        )),
        other => Err(format!("Unhandled stop reason: {other}")),
    }
}

// ---------------------------------------------------------------------------
// Stream processing
// ---------------------------------------------------------------------------

fn json_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_f64().map(|float| float as u64))
}

/// Per-content-block streaming scratch state (upstream stores `index` and
/// `partialJson` on the block itself; here they live alongside
/// `output.content`).
#[derive(Debug, Default)]
struct BlockState {
    event_index: usize,
    partial_json: String,
}

/// Consumes Anthropic SSE events and drives the [`StreamEvent`] protocol,
/// accumulating the final assistant message. Factored out of the HTTP layer
/// so tests can drive it with recorded streams.
struct StreamProcessor<'a> {
    output: &'a mut AssistantMessage,
    model: &'a Model,
    /// `usageModel` (:599-605): the model used for cost calculation — the
    /// requested model, or the fallback model (with its compat-listed cost)
    /// when the server reports a different one (eb1f87fa9; response-model
    /// split per #9188, 1283afd0d).
    usage_model: Model,
    is_oauth_token: bool,
    tools: Option<&'a [Tool]>,
    saw_message_start: bool,
    saw_message_end: bool,
    /// `inputTransformations` (:537, :592-593, :745-746): the latest
    /// `input_transformations` array from `message_start`/`message_delta`
    /// (assignment semantics — the last one wins; 4e69b0c28).
    input_transformations: Option<Value>,
    block_states: Vec<BlockState>,
}

impl<'a> StreamProcessor<'a> {
    fn new(
        output: &'a mut AssistantMessage,
        model: &'a Model,
        is_oauth_token: bool,
        tools: Option<&'a [Tool]>,
    ) -> Self {
        Self {
            output,
            usage_model: model.clone(),
            model,
            is_oauth_token,
            tools,
            saw_message_start: false,
            saw_message_end: false,
            input_transformations: None,
            block_states: Vec::new(),
        }
    }

    fn recompute_total_and_cost(&mut self) {
        // Anthropic doesn't provide total_tokens; compute from components.
        self.output.usage.total_tokens = self.output.usage.input
            + self.output.usage.output
            + self.output.usage.cache_read
            + self.output.usage.cache_write;
        // Fallback responses bill at the fallback model's cost
        // (`usageModel`, eb1f87fa9).
        calculate_cost(&self.usage_model, &mut self.output.usage);
    }

    /// `iterateAnthropicEvents` per-event body: filter, parse, flag, dispatch.
    fn handle_sse(
        &mut self,
        sse: &ServerSentEvent,
        events: &AssistantMessageEventStream,
    ) -> Result<(), String> {
        if sse.event.as_deref() == Some("error") {
            return Err(sse.data.clone());
        }
        let event_name = sse.event.as_deref().unwrap_or("");
        if !ANTHROPIC_MESSAGE_EVENTS.contains(&event_name) {
            return Ok(());
        }
        // P2-4: tolerate keep-alive/dispatch events that carry a known
        // event name but no `data` payload (e.g. `event: ping\ndata:\n\n`
        // from Anthropic-compatible gateways). An empty string can never
        // decode to a valid message event — skip instead of hard-failing
        // the whole stream (upstream JSON.parse("") would throw).
        if sse.data.trim().is_empty() {
            return Ok(());
        }
        let event = parse_json_with_repair(&sse.data).map_err(|error| {
            format!(
                "Could not parse Anthropic SSE event {event_name}: {error}; data={}; raw={}",
                sse.data,
                sse.raw.join("\\n")
            )
        })?;
        let event_type = event.get("type").and_then(Value::as_str);
        if event_type == Some("message_start") {
            self.saw_message_start = true;
        } else if event_type == Some("message_stop") {
            self.saw_message_end = true;
        }
        self.handle_event(&event, events)
    }

    fn handle_event(
        &mut self,
        event: &Value,
        events: &AssistantMessageEventStream,
    ) -> Result<(), String> {
        match event.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                let message = &event["message"];
                if let Some(id) = message.get("id").and_then(Value::as_str) {
                    self.output.response_id = Some(id.to_owned());
                }
                // :592-593 — server-reported input transformations (latest
                // assignment wins; message_delta may replace them later).
                if let Some(transformations) = message.get("input_transformations")
                    && transformations.is_array()
                {
                    self.input_transformations = Some(transformations.clone());
                }
                // :598 — keep the requested model ID on the assistant message
                // and record the provider-reported model separately (#9188,
                // 1283afd0d @ d1230ea20): a relay relabeling the model used to
                // overwrite `output.model`, making the cross-model transform
                // downgrade signed thinking replay to plain text.
                let response_model = message
                    .get("model")
                    .and_then(Value::as_str)
                    .filter(|served| *served != self.model.id);
                if let Some(served) = response_model {
                    self.output.response_model = Some(served.to_owned());
                }
                // :599-605 — cost follows the served model when it matches a
                // permitted fallback target (same provider + exact model);
                // fallback pricing is retained (#9188).
                let fallback_cost = response_model.and_then(|served| {
                    allowed_fallback_models(self.model).and_then(|fallbacks| {
                        fallbacks
                            .iter()
                            .find(|fallback| {
                                fallback.provider == self.model.provider && fallback.model == served
                            })
                            .map(|fallback| fallback.cost.clone())
                    })
                });
                self.usage_model = match response_model.zip(fallback_cost) {
                    Some((served, cost)) => {
                        let mut served_model = self.model.clone();
                        served_model.id = served.to_owned();
                        served_model.cost = cost;
                        served_model
                    }
                    None => self.model.clone(),
                };
                // Capture initial token usage from message_start; this ensures
                // input token counts even if the stream is aborted early.
                let usage = &message["usage"];
                self.output.usage.input = json_u64(&usage["input_tokens"]).unwrap_or(0);
                self.output.usage.output = json_u64(&usage["output_tokens"]).unwrap_or(0);
                self.output.usage.cache_read =
                    json_u64(&usage["cache_read_input_tokens"]).unwrap_or(0);
                self.output.usage.cache_write =
                    json_u64(&usage["cache_creation_input_tokens"]).unwrap_or(0);
                self.output.usage.cache_write1h = Some(
                    json_u64(&usage["cache_creation"]["ephemeral_1h_input_tokens"]).unwrap_or(0),
                );
                self.recompute_total_and_cost();
            }
            Some("content_block_start") => {
                let event_index = json_u64(&event["index"]).unwrap_or(0) as usize;
                let block = &event["content_block"];
                match block.get("type").and_then(Value::as_str) {
                    // :618-623 — a `fallback` marker block is skipped when it
                    // opens the output; anything later is an unsupported
                    // mid-output model switch (eb1f87fa9).
                    Some("fallback") => {
                        if !self.output.content.is_empty() {
                            return Err(
                                "Anthropic performed an unsupported mid-output model fallback"
                                    .to_owned(),
                            );
                        }
                    }
                    Some("text") => {
                        // 59ad3dead: the initial `text` of a content_block_start
                        // is part of the content, not a delta preamble — keep it.
                        let initial_text = block
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        self.output.content.push(AssistantContent::Text(
                            crate::types::TextContent {
                                text: initial_text,
                                text_signature: None,
                            },
                        ));
                        self.block_states.push(BlockState {
                            event_index,
                            ..BlockState::default()
                        });
                        events.push(StreamEvent::TextStart {
                            content_index: self.output.content.len() - 1,
                            partial: Arc::new(self.output.clone()),
                        });
                    }
                    Some("thinking") => {
                        // 59ad3dead: keep the initial `thinking`/`signature`
                        // carried by content_block_start.
                        self.output.content.push(AssistantContent::Thinking(
                            crate::types::ThinkingContent {
                                thinking: block
                                    .get("thinking")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_owned(),
                                thinking_signature: Some(
                                    block
                                        .get("signature")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                        .to_owned(),
                                ),
                                redacted: None,
                            },
                        ));
                        self.block_states.push(BlockState {
                            event_index,
                            ..BlockState::default()
                        });
                        events.push(StreamEvent::ThinkingStart {
                            content_index: self.output.content.len() - 1,
                            partial: Arc::new(self.output.clone()),
                        });
                    }
                    Some("redacted_thinking") => {
                        self.output.content.push(AssistantContent::Thinking(
                            crate::types::ThinkingContent {
                                thinking: "[Reasoning redacted]".to_owned(),
                                thinking_signature: block
                                    .get("data")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned),
                                redacted: Some(true),
                            },
                        ));
                        self.block_states.push(BlockState {
                            event_index,
                            ..BlockState::default()
                        });
                        events.push(StreamEvent::ThinkingStart {
                            content_index: self.output.content.len() - 1,
                            partial: Arc::new(self.output.clone()),
                        });
                    }
                    Some("tool_use") => {
                        let name = block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        self.output.content.push(AssistantContent::ToolCall(
                            crate::types::ToolCall {
                                id: block
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_owned(),
                                name: if self.is_oauth_token {
                                    from_claude_code_name(name, self.tools)
                                } else {
                                    name.to_owned()
                                },
                                arguments: block
                                    .get("input")
                                    .and_then(Value::as_object)
                                    .cloned()
                                    .unwrap_or_default(),
                                thought_signature: None,
                                namespace: None,
                            },
                        ));
                        self.block_states.push(BlockState {
                            event_index,
                            ..BlockState::default()
                        });
                        events.push(StreamEvent::ToolCallStart {
                            content_index: self.output.content.len() - 1,
                            partial: Arc::new(self.output.clone()),
                        });
                    }
                    _ => {}
                }
            }
            Some("content_block_delta") => {
                let event_index = json_u64(&event["index"]).unwrap_or(0) as usize;
                let delta = &event["delta"];
                let Some(content_index) = self
                    .block_states
                    .iter()
                    .position(|state| state.event_index == event_index)
                else {
                    return Ok(());
                };
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        let text = delta.get("text").and_then(Value::as_str).unwrap_or("");
                        if let Some(AssistantContent::Text(block)) =
                            self.output.content.get_mut(content_index)
                        {
                            block.text.push_str(text);
                            events.push(StreamEvent::TextDelta {
                                content_index,
                                delta: text.to_owned(),
                                partial: Arc::new(self.output.clone()),
                            });
                        }
                    }
                    Some("thinking_delta") => {
                        let thinking = delta.get("thinking").and_then(Value::as_str).unwrap_or("");
                        if let Some(AssistantContent::Thinking(block)) =
                            self.output.content.get_mut(content_index)
                        {
                            block.thinking.push_str(thinking);
                            events.push(StreamEvent::ThinkingDelta {
                                content_index,
                                delta: thinking.to_owned(),
                                partial: Arc::new(self.output.clone()),
                            });
                        }
                    }
                    Some("input_json_delta") => {
                        let partial_json = delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        self.block_states[content_index]
                            .partial_json
                            .push_str(partial_json);
                        if let Some(AssistantContent::ToolCall(block)) =
                            self.output.content.get_mut(content_index)
                        {
                            block.arguments = parse_streaming_json(Some(
                                &self.block_states[content_index].partial_json,
                            ));
                            events.push(StreamEvent::ToolCallDelta {
                                content_index,
                                delta: partial_json.to_owned(),
                                partial: Arc::new(self.output.clone()),
                            });
                        }
                    }
                    Some("signature_delta") => {
                        let signature =
                            delta.get("signature").and_then(Value::as_str).unwrap_or("");
                        if let Some(AssistantContent::Thinking(block)) =
                            self.output.content.get_mut(content_index)
                        {
                            block
                                .thinking_signature
                                .get_or_insert_with(String::new)
                                .push_str(signature);
                        }
                    }
                    _ => {}
                }
            }
            Some("content_block_stop") => {
                let event_index = json_u64(&event["index"]).unwrap_or(0) as usize;
                let Some(content_index) = self
                    .block_states
                    .iter()
                    .position(|state| state.event_index == event_index)
                else {
                    return Ok(());
                };
                match self.output.content.get_mut(content_index) {
                    Some(AssistantContent::Text(block)) => {
                        events.push(StreamEvent::TextEnd {
                            content_index,
                            content: block.text.clone(),
                            partial: Arc::new(self.output.clone()),
                        });
                    }
                    Some(AssistantContent::Thinking(block)) => {
                        events.push(StreamEvent::ThinkingEnd {
                            content_index,
                            content: block.thinking.clone(),
                            partial: Arc::new(self.output.clone()),
                        });
                    }
                    Some(AssistantContent::ToolCall(block)) => {
                        block.arguments = parse_streaming_json(Some(
                            &self.block_states[content_index].partial_json,
                        ));
                        events.push(StreamEvent::ToolCallEnd {
                            content_index,
                            tool_call: block.clone(),
                            partial: Arc::new(self.output.clone()),
                        });
                    }
                    None => {}
                }
            }
            Some("message_delta") => {
                let delta = &event["delta"];
                // :745-746 — replaces any message_start-collected array.
                if let Some(transformations) = event.get("input_transformations")
                    && transformations.is_array()
                {
                    self.input_transformations = Some(transformations.clone());
                }
                if let Some(reason) = delta.get("stop_reason").and_then(Value::as_str)
                    && !reason.is_empty()
                {
                    // 926eb15c1: preserve the raw provider reason before
                    // mapping.
                    self.output.raw_stop_reason = Some(reason.to_owned());
                    let (stop_reason, error_message) =
                        map_stop_reason(reason, delta.get("stop_details"))?;
                    self.output.stop_reason = stop_reason;
                    if let Some(error_message) = error_message {
                        self.output.error_message = Some(error_message);
                    }
                }
                // Only update usage fields if present (not null). Preserves
                // input_tokens from message_start when proxies omit it in
                // message_delta.
                if let Some(usage) = event.get("usage") {
                    if let Some(input) = json_u64(&usage["input_tokens"]) {
                        self.output.usage.input = input;
                    }
                    if let Some(output) = json_u64(&usage["output_tokens"]) {
                        self.output.usage.output = output;
                    }
                    if let Some(cache_read) = json_u64(&usage["cache_read_input_tokens"]) {
                        self.output.usage.cache_read = cache_read;
                    }
                    if let Some(cache_write) = json_u64(&usage["cache_creation_input_tokens"]) {
                        self.output.usage.cache_write = cache_write;
                    }
                    // #9210 (`667fc3dd3`): Vercel AI Gateway includes the
                    // 1-hour cache-write TTL breakdown in deltas, though the
                    // SDK only types it on message_start.
                    if let Some(cache_write_1h) =
                        json_u64(&usage["cache_creation"]["ephemeral_1h_input_tokens"])
                    {
                        self.output.usage.cache_write1h = Some(cache_write_1h);
                    }
                    // Reasoning tokens arrive in
                    // `output_tokens_details.thinking_tokens` on the final
                    // message_delta usage (a subset of output_tokens).
                    if let Some(thinking_tokens) =
                        json_u64(&usage["output_tokens_details"]["thinking_tokens"])
                    {
                        self.output.usage.reasoning = Some(thinking_tokens);
                    }
                }
                self.recompute_total_and_cost();
            }
            _ => {}
        }
        Ok(())
    }

    /// Post-stream validation (upstream `iterateAnthropicEvents` tail plus the
    /// stream function's final checks).
    fn finish(self, signal: Option<&CancellationToken>) -> Result<DoneReason, String> {
        if self.saw_message_start && !self.saw_message_end {
            return Err("Anthropic stream ended before message_stop".to_owned());
        }
        if signal.is_some_and(|signal| signal.is_cancelled()) {
            return Err("Request was aborted".to_owned());
        }
        let reason = match self.output.stop_reason {
            // `Deferred` shares the `Pending` arm: no rpi provider produces it
            // (lifecycle is [DEFER], R2.2.1), so it is unreachable here and
            // treated as "stream ended without a usable stop reason".
            StopReason::Pending | StopReason::Deferred => {
                return Err("Anthropic stream ended without a stop reason".to_owned());
            }
            StopReason::Aborted | StopReason::Error => {
                return Err(self
                    .output
                    .error_message
                    .clone()
                    .unwrap_or_else(|| "An unknown error occurred".to_owned()));
            }
            StopReason::Stop => DoneReason::Stop,
            StopReason::Length => DoneReason::Length,
            StopReason::ToolUse => DoneReason::ToolUse,
        };
        // :790-803 (4e69b0c28): surface collected input transformations as a
        // diagnostic on the successful final message — `undefined` fields
        // are dropped from the JSON just like upstream's `?? undefined`.
        if let Some(transformations) = &self.input_transformations
            && let Some(entries) = transformations.as_array()
            && !entries.is_empty()
        {
            let mapped: Vec<Value> = entries
                .iter()
                .map(|entry| {
                    let mut object = serde_json::Map::new();
                    for key in ["type", "path", "reason"] {
                        if let Some(value) = entry.get(key).and_then(Value::as_str) {
                            object.insert(key.to_owned(), json!(value));
                        }
                    }
                    Value::Object(object)
                })
                .collect();
            let mut details = serde_json::Map::new();
            details.insert("transformations".to_owned(), Value::Array(mapped));
            self.output.diagnostics.get_or_insert_with(Vec::new).push(
                crate::types::AssistantMessageDiagnostic {
                    kind: "anthropic_input_transformations".to_owned(),
                    timestamp: now_ms(),
                    error: None,
                    details: Some(details),
                },
            );
        }
        Ok(reason)
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn initial_output(model: &Model) -> AssistantMessage {
    AssistantMessage {
        role: crate::types::AssistantRole::Assistant,
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

/// The streaming body: everything that runs inside upstream's async IIFE.
/// Errors return the upstream `Error.message`; `output` carries the partial
/// message either way.
async fn run(
    model: &Model,
    context: &TranscriptContext,
    options: &AnthropicOptions,
    output: &mut AssistantMessage,
    events: &AssistantMessageEventStream,
) -> Result<DoneReason, String> {
    let api_key = options.stream.api_key.as_deref();
    let federation = crate::auth::anthropic_federation::get_anthropic_federation(
        &model.provider,
        &model.base_url,
        api_key,
        options.stream.headers.as_ref(),
        options.stream.env.as_ref(),
    );
    // Federation replaces the static credential: skip the "no API key" guard
    // and mint the short-lived access token before building the headers.
    if federation.is_none() {
        assert_request_auth(&model.provider, api_key, options.stream.headers.as_ref())?;
    }
    let federation_token = match &federation {
        Some(config) => Some(
            crate::auth::anthropic_federation::get_access_token(config)
                .await
                .map_err(|mint_error| mint_error.message)?,
        ),
        None => None,
    };

    let compat = get_anthropic_compat(model);
    let current_tools = crate::utils::transcript::get_current_tools(&context.messages);
    // Native tool changes: the transport accepts inline `tool_addition`
    // blocks (b271b0a52), the transcript carries an initial active tool to
    // anchor the placeholder, and same-name redefinitions are expressed by
    // value. Otherwise the current tool list is sent (#9548).
    let native_tool_changes = compat.supports_mid_convo_system_messages
        && compat.supports_mid_convo_tool_changes
        && crate::utils::transcript::get_initial_system_message(&context.messages)
            .and_then(|message| message.tools_added.as_ref())
            .is_some_and(|tools| !tools.is_empty());

    let dynamic_headers = if model.provider == "github-copilot" {
        let has_images = has_copilot_vision_input(&context.messages);
        Some(build_copilot_dynamic_headers(&context.messages, has_images))
    } else {
        None
    };

    let (retention, _cache_control) = get_cache_control(
        model,
        options.stream.cache_retention,
        options.stream.env.as_ref(),
    );
    let cache_session_id = if retention == CacheRetention::None {
        None
    } else {
        options.stream.session_id.as_deref()
    };

    let (headers, is_oauth_token) = build_request_headers(
        model,
        context,
        native_tool_changes,
        api_key,
        options,
        dynamic_headers,
        cache_session_id,
        federation_token.as_deref(),
    );

    let mut params = build_params(model, context, is_oauth_token, native_tool_changes, options)?;
    if let Some(on_payload) = &options.stream.on_payload
        && let Some(mut next_params) = on_payload(params.clone(), model).await
    {
        // :571-572 — the SDK transport always streams; a caller-supplied
        // payload cannot turn that off.
        next_params["stream"] = json!(true);
        params = next_params;
    }

    let url = format!("{}/v1/messages", model.base_url.trim_end_matches('/'));
    let header_map = provider_headers_to_header_map(&headers)?;
    let mut client_builder =
        crate::api::http_client::adapter_client_builder(options.stream.env.as_ref(), &url)?;
    // Idle-timeout semantics (upstream undici headersTimeout/bodyTimeout;
    // see api::stream_timeouts): connect_timeout bounds the connection, the
    // headers wait is bounded in send_provider_request, and the body stream
    // is wrapped in an inter-chunk idle timeout — an actively streaming
    // response is never killed by its total duration.
    if let Some(timeout_ms) = options.stream.timeout_ms {
        client_builder =
            client_builder.connect_timeout(std::time::Duration::from_millis(timeout_ms));
    }
    let client = client_builder.build().map_err(|error| error.to_string())?;

    let retry_options = ProviderRetryOptions {
        max_retries: options.stream.max_retries,
        max_retry_delay_ms: options.stream.max_retry_delay_ms,
        ..Default::default()
    };
    let initial_federation_token = federation_token.clone();
    let response = retry_provider_request(
        || {
            let client = client.clone();
            let url = url.clone();
            let params = params.clone();
            let header_map = header_map.clone();
            let federation = federation.clone();
            let signal = options.stream.signal.clone();
            let fetch = options.stream.fetch.clone();
            let timeout_ms = options.stream.timeout_ms;
            let mut authorization = initial_federation_token
                .as_deref()
                .map(|token| format!("Bearer {token}"));
            async move {
                let mut did_refresh_for_401 = false;
                loop {
                    let mut request_headers = header_map.clone();
                    if let Some(value) = &authorization {
                        match reqwest::header::HeaderValue::from_str(value) {
                            Ok(value) => {
                                // `insert` replaces the mapped header so the
                                // retried request carries exactly one token.
                                request_headers.insert(reqwest::header::AUTHORIZATION, value);
                            }
                            Err(header_error) => {
                                return Err(ProviderErrorInfo {
                                    status: None,
                                    headers: None,
                                    message: format!(
                                        "Invalid federation authorization header: {header_error}"
                                    ),
                                });
                            }
                        }
                    }
                    let request = client.post(&url).headers(request_headers).json(&params);
                    // 027a58479 (R2.7.4): per-request custom fetch channel; `None`
                    // keeps the reqwest default path unchanged.
                    let result =
                        send_provider_request(request, fetch.as_ref(), signal.as_ref(), timeout_ms)
                            .await;
                    let response = match result {
                        Ok(response) => response,
                        Err(error) => return Err(error.into_provider_error_info()),
                    };
                    let status = response.status();
                    // Reactive refresh (SDK `shouldRetry`): a 401 from a
                    // token-authenticated request invalidates the cache and
                    // retries once with a freshly exchanged token.
                    if status == reqwest::StatusCode::UNAUTHORIZED
                        && !did_refresh_for_401
                        && let Some(config) = &federation
                    {
                        did_refresh_for_401 = true;
                        crate::auth::anthropic_federation::invalidate_access_token(config).await;
                        match crate::auth::anthropic_federation::get_access_token(config).await {
                            Ok(token) => {
                                authorization = Some(format!("Bearer {token}"));
                                continue;
                            }
                            Err(refresh_error) => {
                                return Err(ProviderErrorInfo {
                                    status: Some(401),
                                    headers: None,
                                    message: refresh_error.message,
                                });
                            }
                        }
                    }
                    if status.is_success() {
                        return Ok(response);
                    }
                    let status = status.as_u16();
                    let response_headers = headers_to_record(response.headers());
                    let body = response.text().await.unwrap_or_default();
                    let normalized = NormalizedProviderError::new(
                        Some(status),
                        Some(body),
                        format!("Request failed with status {status}"),
                    );
                    return Err(ProviderErrorInfo {
                        status: Some(status),
                        headers: Some(response_headers),
                        message: format_provider_error(&normalized, None),
                    });
                }
            }
        },
        retry_options,
        options.stream.signal.as_ref(),
    )
    .await
    .map_err(|error| error.message())?;

    if let Some(on_response) = &options.stream.on_response {
        on_response(
            ProviderResponse {
                status: response.status().as_u16(),
                headers: headers_to_record(response.headers()),
            },
            model,
        )
        .await;
    }

    events.push(StreamEvent::Start {
        partial: Arc::new(output.clone()),
    });

    let mut processor = StreamProcessor::new(
        output,
        model,
        is_oauth_token,
        Some(current_tools.as_slice()),
    );
    let mut decoder = SseDecoder::new();
    // Inter-chunk idle timeout (upstream bodyTimeout): actively streaming
    // responses never expire; only a silent stream does.
    let mut byte_stream =
        crate::api::stream_timeouts::wrap(response.bytes_stream(), options.stream.timeout_ms);
    loop {
        // P1-5: race the body read against the abort token — upstream hands
        // the signal to fetch (cancellation interrupts the body read
        // immediately); a check only after a chunk arrives would stall on an
        // idle body until the idle timeout (5 min default, unbounded when
        // disabled).
        let chunk =
            match next_chunk_or_cancelled(&mut byte_stream, options.stream.signal.as_ref()).await {
                StreamNext::Item(chunk) => chunk,
                StreamNext::Cancelled => return Err("Request was aborted".to_owned()),
            };
        let Some(chunk) = chunk else { break };
        let bytes = chunk.map_err(|error| error.message())?;
        for sse in decoder.feed(&bytes)? {
            processor.handle_sse(&sse, events)?;
        }
    }
    // P1 (rc.12 review): `finish()` fails when the unterminated tail crosses
    // the line cap after the end-of-stream U+FFFD flush (buffer at the cap +
    // an incomplete UTF-8 sequence) — propagate like every other malformed-
    // stream error instead of panicking the adapter task (a panic here never
    // reaches the event stream, so the turn would hang without a terminal
    // event).
    for sse in decoder.finish()? {
        processor.handle_sse(&sse, events)?;
    }
    processor.finish(options.stream.signal.as_ref())
}

/// `stream` (anthropic-messages). The provider-facing transcript is
/// normalized upstream; the entry resolves the mid-conversation policy per
/// model compat (#9548, anthropic-messages.ts:511-517).
pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: AnthropicOptions,
) -> AssistantMessageEventStream {
    let event_stream = AssistantMessageEventStream::new();
    let task_stream = event_stream.clone();
    let model = model.clone();
    let context = resolve_transcript(
        context,
        get_anthropic_compat(&model).supports_mid_convo_system_messages,
    );
    tokio::spawn(async move {
        let signal = options.stream.signal.clone();
        let mut output = initial_output(&model);
        // :513-520 (4e69b0c28): managed models record the turn's effort at
        // stream start (`options.effort ?? "high"`); other models leave the
        // field absent — never an empty string.
        if model
            .compat
            .as_ref()
            .and_then(|compat| compat.supports_mid_convo_effort)
            == Some(true)
        {
            output.provider_thinking_level =
                Some(options.effort.clone().unwrap_or_else(|| "high".to_owned()));
        }
        match run(&model, &context, &options, &mut output, &task_stream).await {
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

/// `mapThinkingLevelToEffort`: ThinkingLevel → Anthropic effort for adaptive
/// thinking. Note: effort "max" is available on all adaptive-thinking Claude
/// models, while native "xhigh" is only on Opus 4.7/4.8, Sonnet 5, Fable 5.
fn map_thinking_level_to_effort(model: &Model, level: Option<ThinkingLevel>) -> String {
    if let Some(level) = level {
        let key = match level {
            ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
            ThinkingLevel::Low => ModelThinkingLevel::Low,
            ThinkingLevel::Medium => ModelThinkingLevel::Medium,
            ThinkingLevel::High => ModelThinkingLevel::High,
            ThinkingLevel::Xhigh => ModelThinkingLevel::Xhigh,
            ThinkingLevel::Max => ModelThinkingLevel::Max,
        };
        if let Some(Some(mapped)) = model
            .thinking_level_map
            .as_ref()
            .and_then(|map| map.get(&key))
        {
            return mapped.clone();
        }
    }
    match level {
        Some(ThinkingLevel::Minimal) | Some(ThinkingLevel::Low) => "low",
        Some(ThinkingLevel::Medium) => "medium",
        _ => "high",
    }
    .to_owned()
}

/// `SimpleToolChoice` → [`AnthropicToolChoice`] (the simple union has no
/// forced-tool variant; upstream forwards the literal verbatim,
/// anthropic-messages.ts:858).
fn simple_tool_choice_to_anthropic(choice: crate::types::SimpleToolChoice) -> AnthropicToolChoice {
    match choice {
        crate::types::SimpleToolChoice::Auto => AnthropicToolChoice::Auto,
        crate::types::SimpleToolChoice::None => AnthropicToolChoice::None,
    }
}

/// `streamSimple` (anthropic-messages).
pub fn stream_simple(
    model: &Model,
    context: &TranscriptContext,
    options: Option<SimpleStreamOptions>,
) -> Result<AssistantMessageEventStream, String> {
    // Auth check at the entry, before any stream is constructed
    // (8b5899dce: anthropic-messages.ts streamSimple asserts request auth
    // first and throws synchronously — the Rust equivalent is `Err`).
    // Workload identity federation replaces the static credential, so the
    // guard is skipped when it resolves (the token is minted in `run`).
    let federation = crate::auth::anthropic_federation::get_anthropic_federation(
        &model.provider,
        &model.base_url,
        options.as_ref().and_then(|o| o.stream.api_key.as_deref()),
        options.as_ref().and_then(|o| o.stream.headers.as_ref()),
        options.as_ref().and_then(|o| o.stream.env.as_ref()),
    );
    if federation.is_none() {
        assert_request_auth(
            &model.provider,
            options.as_ref().and_then(|o| o.stream.api_key.as_deref()),
            options.as_ref().and_then(|o| o.stream.headers.as_ref()),
        )?;
    }

    let api_key = options.as_ref().and_then(|o| o.stream.api_key.clone());
    let base = build_base_options(model, context, options.as_ref(), api_key);
    // `base` carries `toolChoice` on every branch upstream — mirror that
    // for each construction site below (rc.12 review: previously dropped).
    let tool_choice = options
        .as_ref()
        .and_then(|o| o.tool_choice)
        .map(simple_tool_choice_to_anthropic);
    let Some(reasoning) = options.as_ref().and_then(|o| o.reasoning) else {
        return Ok(stream(
            model,
            context,
            AnthropicOptions {
                stream: base,
                thinking_enabled: Some(false),
                tool_choice,
                ..AnthropicOptions::default()
            },
        ));
    };

    // Models with adaptive thinking use an effort level; older models use
    // budget-based thinking.
    if model
        .compat
        .as_ref()
        .and_then(|c| c.force_adaptive_thinking)
        == Some(true)
    {
        let effort = map_thinking_level_to_effort(model, Some(reasoning));
        return Ok(stream(
            model,
            context,
            AnthropicOptions {
                stream: base,
                thinking_enabled: Some(true),
                effort: Some(effort),
                tool_choice,
                ..AnthropicOptions::default()
            },
        ));
    }

    let adjusted = adjust_max_tokens_for_thinking(
        base.max_tokens,
        model.max_tokens,
        reasoning,
        options.as_ref().and_then(|o| o.thinking_budgets.as_ref()),
    );
    let max_tokens = clamp_max_tokens_to_context(model, context, adjusted.max_tokens);

    let mut anthropic_options = AnthropicOptions {
        stream: base,
        thinking_enabled: Some(true),
        thinking_budget_tokens: Some(
            adjusted
                .thinking_budget
                .min(max_tokens.saturating_sub(1024)),
        ),
        tool_choice,
        ..AnthropicOptions::default()
    };
    anthropic_options.stream.max_tokens = Some(max_tokens);
    Ok(stream(model, context, anthropic_options))
}

/// `ProviderStreams` implementation for `ApiKind::ANTHROPIC_MESSAGES`.
///
/// The trait carries plain [`StreamOptions`]; Anthropic-specific extras
/// ([`AnthropicOptions`]) reach [`stream`] only through direct calls or via
/// [`stream_simple`] reasoning mapping (design §3.3 collapses per-API extras).
#[derive(Debug, Clone, Copy, Default)]
pub struct AnthropicMessages;

impl ProviderStreams for AnthropicMessages {
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        stream(
            model,
            context,
            AnthropicOptions {
                stream: options.unwrap_or_default(),
                ..AnthropicOptions::default()
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

#[cfg(test)]
pub(crate) mod tests {
    use futures::StreamExt;
    use serde_json::json;

    use super::*;
    use crate::types::{ApiKind, AssistantRole, Context, Usage};

    pub(crate) fn make_model(extra: serde_json::Value) -> Model {
        let mut value = json!({
            "id": "claude-sonnet-4-5", "name": "Sonnet", "api": "anthropic-messages",
            "provider": "anthropic", "baseUrl": "https://api.anthropic.com",
            "reasoning": true, "input": ["text"],
            "cost": {"input": 3.0, "output": 15.0, "cacheRead": 0.3, "cacheWrite": 3.75},
            "contextWindow": 200000, "maxTokens": 64000
        });
        value
            .as_object_mut()
            .expect("object")
            .extend(extra.as_object().cloned().unwrap_or_default());
        serde_json::from_value(value).expect("model")
    }

    pub(crate) fn assistant_message(
        content: Vec<AssistantContent>,
        provider: &str,
        model_id: &str,
    ) -> Message {
        Message::Assistant(AssistantMessage {
            role: AssistantRole::Assistant,
            content,
            api: ApiKind::from(ApiKind::ANTHROPIC_MESSAGES),
            provider: provider.to_owned(),
            model: model_id.to_owned(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
            deferred: None,
            end_turn: None,
            raw_stop_reason: None,
            duration_ms: None,
        })
    }

    pub(crate) fn user_text(text: &str) -> Message {
        serde_json::from_value(json!({
            "role": "user", "content": text, "timestamp": 0
        }))
        .expect("user")
    }

    fn tool(name: &str) -> Tool {
        serde_json::from_value(json!({
            "name": name, "description": "d",
            "parameters": {"type": "object", "properties": {"x": {"type": "string"}}, "required": ["x"]}
        }))
        .expect("tool")
    }

    fn tool_result(tool_call_id: &str, content: &str) -> Message {
        serde_json::from_value(json!({
            "role": "toolResult", "toolCallId": tool_call_id, "toolName": "t",
            "content": [{"type": "text", "text": content}],
            "isError": false, "timestamp": 0
        }))
        .expect("toolResult")
    }

    pub(crate) fn context(messages: Vec<Message>, tools: Option<Vec<Tool>>) -> TranscriptContext {
        crate::utils::transcript::normalize_context(&Context {
            system_prompt: None,
            messages,
            tools,
        })
    }

    // -----------------------------------------------------------------------
    // Pure helpers
    // -----------------------------------------------------------------------

    #[test]
    fn test_normalize_tool_call_id() {
        assert_eq!(
            normalize_tool_call_id("call_abc|item_xyz"),
            "call_abc_item_xyz"
        );
        assert_eq!(normalize_tool_call_id("fc_🙈x"), "fc__x");
        let long = "a".repeat(100);
        assert_eq!(normalize_tool_call_id(&long).len(), 64);
    }

    #[test]
    fn test_claude_code_name_mapping() {
        assert_eq!(to_claude_code_name("bash"), "Bash");
        assert_eq!(to_claude_code_name("BASH"), "Bash");
        assert_eq!(to_claude_code_name("custom_tool"), "custom_tool");

        let tools = vec![tool("BASH")];
        assert_eq!(from_claude_code_name("Bash", Some(&tools)), "BASH");
        assert_eq!(from_claude_code_name("Bash", None), "Bash");
        assert_eq!(from_claude_code_name("unknown", Some(&tools)), "unknown");
    }

    #[test]
    fn test_resolve_cache_retention() {
        assert_eq!(resolve_cache_retention(None, None), CacheRetention::Short);
        assert_eq!(
            resolve_cache_retention(Some(CacheRetention::None), None),
            CacheRetention::None
        );
        let env: ProviderEnv = [("RPI_CACHE_RETENTION".to_owned(), "long".to_owned())].into();
        assert_eq!(
            resolve_cache_retention(None, Some(&env)),
            CacheRetention::Long
        );
        // Explicit option wins over env.
        assert_eq!(
            resolve_cache_retention(Some(CacheRetention::Short), Some(&env)),
            CacheRetention::Short
        );
    }

    #[test]
    fn test_get_cache_control() {
        let model = make_model(json!({}));
        let (_, cc) = get_cache_control(&model, None, None);
        assert_eq!(cc, Some(json!({"type": "ephemeral"})));

        let (_, cc) = get_cache_control(&model, Some(CacheRetention::Long), None);
        assert_eq!(cc, Some(json!({"type": "ephemeral", "ttl": "1h"})));

        // Long retention unsupported by the model: plain ephemeral.
        let model = make_model(json!({"compat": {"supportsLongCacheRetention": false}}));
        let (_, cc) = get_cache_control(&model, Some(CacheRetention::Long), None);
        assert_eq!(cc, Some(json!({"type": "ephemeral"})));

        let (retention, cc) = get_cache_control(&model, Some(CacheRetention::None), None);
        assert_eq!(retention, CacheRetention::None);
        assert_eq!(cc, None);
    }

    #[test]
    fn test_get_anthropic_compat_defaults() {
        let compat = get_anthropic_compat(&make_model(json!({})));
        assert!(compat.supports_eager_tool_input_streaming);
        assert!(compat.supports_long_cache_retention);
        assert!(!compat.send_session_affinity_headers);
        assert!(compat.supports_cache_control_on_tools);
        assert!(compat.supports_temperature);
        assert!(!compat.allow_empty_signature);
        assert!(!compat.supports_strict_tools);
        // #9548: mid-convo faces default false; catalog data gates them.
        assert!(!compat.supports_mid_convo_system_messages);
        assert!(!compat.supports_mid_convo_tool_changes);
    }

    #[test]
    fn test_assert_request_auth() {
        assert!(assert_request_auth("p", Some("sk"), None).is_ok());
        let headers: ProviderHeaders =
            [("Authorization".to_owned(), Some("Bearer x".to_owned()))].into();
        assert!(assert_request_auth("p", None, Some(&headers)).is_ok());
        let empty: ProviderHeaders = [("authorization".to_owned(), Some("  ".to_owned()))].into();
        assert_eq!(
            assert_request_auth("p", None, Some(&empty)),
            Err("No API key for provider: p".to_owned())
        );
        assert_eq!(
            assert_request_auth("p", None, None),
            Err("No API key for provider: p".to_owned())
        );
    }

    #[test]
    fn test_build_request_headers_api_key() {
        let model = make_model(json!({}));
        let options_headers: ProviderHeaders =
            [("X-Custom".to_owned(), Some("1".to_owned()))].into();
        let options = AnthropicOptions {
            stream: crate::types::StreamOptions {
                request: crate::types::ProviderRequestOptions {
                    headers: Some(options_headers),
                    ..Default::default()
                },
                ..Default::default()
            },
            thinking_enabled: Some(true),
            ..Default::default()
        };
        let (headers, is_oauth) = build_request_headers(
            &model,
            &crate::utils::transcript::normalize_context(&Context {
                system_prompt: None,
                messages: vec![],
                tools: None,
            }),
            false,
            Some("sk-ant-api03-key"),
            &options,
            None,
            Some("session-1"),
            None,
        );
        assert!(!is_oauth);
        assert_eq!(
            headers.get("x-api-key").and_then(|v| v.as_deref()),
            Some("sk-ant-api03-key")
        );
        assert_eq!(
            headers.get("anthropic-version").and_then(|v| v.as_deref()),
            Some(ANTHROPIC_VERSION)
        );
        // Interleaved-thinking beta is requested by default.
        assert_eq!(
            headers.get("anthropic-beta").and_then(|v| v.as_deref()),
            Some(INTERLEAVED_THINKING_BETA)
        );
        // sendSessionAffinityHeaders defaults to false.
        assert!(!headers.contains_key("x-session-affinity"));
        assert_eq!(
            headers.get("X-Custom").and_then(|v| v.as_deref()),
            Some("1")
        );
    }

    #[test]
    fn test_build_request_headers_oauth() {
        let model = make_model(json!({"compat": {"supportsEagerToolInputStreaming": false}}));
        let options = AnthropicOptions {
            thinking_enabled: Some(true),
            ..Default::default()
        };
        let (headers, is_oauth) = build_request_headers(
            &model,
            &crate::utils::transcript::normalize_context(&Context {
                system_prompt: None,
                messages: vec![],
                tools: Some(vec![tool("search")]),
            }),
            false,
            Some("sk-ant-oat01-token"),
            &options,
            None,
            None,
            None,
        );
        assert!(is_oauth);
        assert_eq!(
            headers.get("authorization").and_then(|v| v.as_deref()),
            Some("Bearer sk-ant-oat01-token")
        );
        assert_eq!(
            headers.get("user-agent").and_then(|v| v.as_deref()),
            // 3a624b82d: the reported version follows upstream
            // `claudeCodeVersion` (`2.1.280`, anthropic-messages.ts:87).
            Some("claude-cli/2.1.280")
        );
        assert_eq!(headers.get("x-app").and_then(|v| v.as_deref()), Some("cli"));
        let beta = headers
            .get("anthropic-beta")
            .and_then(|v| v.as_deref())
            .expect("beta");
        assert!(beta.starts_with("claude-code-20250219,oauth-2025-04-20,"));
        assert!(beta.contains(FINE_GRAINED_TOOL_STREAMING_BETA));
        assert!(beta.contains(INTERLEAVED_THINKING_BETA));
    }

    #[test]
    fn test_build_request_headers_adaptive_skips_interleaved_beta() {
        let model = make_model(json!({"compat": {"forceAdaptiveThinking": true}}));
        let (headers, _) = build_request_headers(
            &model,
            &crate::utils::transcript::normalize_context(&Context::default()),
            false,
            Some("sk-key"),
            &AnthropicOptions::default(),
            None,
            None,
            None,
        );
        assert!(!headers.contains_key("anthropic-beta"));
    }

    #[test]
    fn test_build_request_headers_session_affinity() {
        let model = make_model(json!({"compat": {"sendSessionAffinityHeaders": true}}));
        let (headers, _) = build_request_headers(
            &model,
            &crate::utils::transcript::normalize_context(&Context::default()),
            false,
            Some("sk-key"),
            &AnthropicOptions::default(),
            None,
            Some("sess-9"),
            None,
        );
        assert_eq!(
            headers.get("x-session-affinity").and_then(|v| v.as_deref()),
            Some("sess-9")
        );
    }

    /// #9102 (bbb61e34a, FR-C R1): OpenRouter anthropic-compatible models
    /// derive session affinity headers by default and select the
    /// `x-session-id` format — port of upstream `fireworks-models.test.ts`
    /// "sends only x-session-id for OpenRouter models" /
    /// "omits OpenRouter session headers when cacheRetention is none" /
    /// "allows OpenRouter session headers to be disabled" (header-level
    /// assertions; the cacheRetention gate itself lives in `stream`, covered
    /// by `test_stream_cache_session_id_none` semantics below).
    #[test]
    fn test_build_request_headers_openrouter_session_affinity() {
        let openrouter = make_model(json!({
            "id": "anthropic/claude-opus-4.8",
            "provider": "openrouter",
            "baseUrl": "https://openrouter.ai/api"
        }));

        // Default: on, x-session-id only.
        let compat = get_anthropic_compat(&openrouter);
        assert!(compat.send_session_affinity_headers);
        assert_eq!(
            compat.session_affinity_format,
            Some(SessionAffinityFormat::Openrouter)
        );
        let (headers, _) = build_request_headers(
            &openrouter,
            &crate::utils::transcript::normalize_context(&Context::default()),
            false,
            Some("sk-key"),
            &AnthropicOptions::default(),
            None,
            Some("openrouter-session-1"),
            None,
        );
        assert_eq!(
            headers.get("x-session-id").and_then(|v| v.as_deref()),
            Some("openrouter-session-1")
        );
        assert!(!headers.contains_key("x-session-affinity"));

        // Explicit opt-out: `compat.sendSessionAffinityHeaders: false`.
        let disabled = make_model(json!({
            "id": "anthropic/claude-opus-4.8",
            "provider": "openrouter",
            "baseUrl": "https://openrouter.ai/api",
            "compat": {"sendSessionAffinityHeaders": false}
        }));
        let compat = get_anthropic_compat(&disabled);
        assert!(!compat.send_session_affinity_headers);
        let (headers, _) = build_request_headers(
            &disabled,
            &crate::utils::transcript::normalize_context(&Context::default()),
            false,
            Some("sk-key"),
            &AnthropicOptions::default(),
            None,
            Some("openrouter-session-3"),
            None,
        );
        assert!(!headers.contains_key("x-session-id"));
        assert!(!headers.contains_key("x-session-affinity"));

        // Non-OpenRouter anthropic models stay off by default (Fireworks
        // catalog entries opt in explicitly with the unset format →
        // x-session-affinity).
        let fireworks = make_model(json!({
            "id": "accounts/fireworks/models/deepseek-v4-flash-0731",
            "provider": "fireworks",
            "baseUrl": "https://api.fireworks.ai",
            "compat": {"sendSessionAffinityHeaders": true}
        }));
        let compat = get_anthropic_compat(&fireworks);
        assert!(compat.send_session_affinity_headers);
        assert_eq!(compat.session_affinity_format, None);
        let (headers, _) = build_request_headers(
            &fireworks,
            &crate::utils::transcript::normalize_context(&Context::default()),
            false,
            Some("sk-key"),
            &AnthropicOptions::default(),
            None,
            Some("fireworks-session"),
            None,
        );
        assert_eq!(
            headers.get("x-session-affinity").and_then(|v| v.as_deref()),
            Some("fireworks-session")
        );
    }

    // -----------------------------------------------------------------------
    // Message conversion
    // -----------------------------------------------------------------------

    #[test]
    fn test_convert_messages_basic_and_cache_control() {
        let messages = vec![
            user_text("hello"),
            assistant_message(
                vec![AssistantContent::Text(crate::types::TextContent {
                    text: "hi there".to_owned(),
                    text_signature: None,
                })],
                "anthropic",
                "claude-sonnet-4-5",
            ),
            user_text("again"),
        ];
        let cc = json!({"type": "ephemeral"});
        let converted =
            convert_messages(&messages, false, Some(&cc), false, None, None).expect("convert");
        let params = converted.messages;
        assert_eq!(params.len(), 3);
        assert_eq!(params[0], json!({"role": "user", "content": "hello"}));
        assert_eq!(
            params[1],
            json!({"role": "assistant", "content": [{"type": "text", "text": "hi there"}]})
        );
        // cache_control lands on the last user message, wrapping string content.
        assert_eq!(
            params[2],
            json!({"role": "user", "content": [{"type": "text", "text": "again", "cache_control": {"type": "ephemeral"}}]})
        );
    }

    #[test]
    fn test_convert_messages_skips_empty() {
        let messages = vec![
            user_text("   "),
            assistant_message(vec![], "anthropic", "claude-sonnet-4-5"),
            user_text("real"),
        ];
        let converted =
            convert_messages(&messages, false, None, false, None, None).expect("convert");
        let params = converted.messages;
        assert_eq!(params.len(), 1);
        assert_eq!(params[0]["content"], json!("real"));
    }

    #[test]
    fn test_convert_messages_thinking_signature() {
        let thinking = |thinking: &str, signature: Option<&str>| {
            AssistantContent::Thinking(crate::types::ThinkingContent {
                thinking: thinking.to_owned(),
                thinking_signature: signature.map(str::to_owned),
                redacted: None,
            })
        };
        let messages = vec![assistant_message(
            vec![
                thinking("signed thought", Some("sig")),
                thinking("unsigned thought", None),
                thinking("   ", None),
                thinking("", Some("")),
            ],
            "anthropic",
            "claude-sonnet-4-5",
        )];
        let converted =
            convert_messages(&messages, false, None, false, None, None).expect("convert");
        let params = converted.messages;
        let blocks = params[0]["content"].as_array().expect("blocks");
        assert_eq!(
            blocks[0],
            json!({"type": "thinking", "thinking": "signed thought", "signature": "sig"})
        );
        // Missing signature → plain text.
        assert_eq!(
            blocks[1],
            json!({"type": "text", "text": "unsigned thought"})
        );
        // Empty thinking without signature is dropped entirely.
        assert_eq!(blocks.len(), 2);

        // allowEmptySignature keeps the block as thinking with "".
        let converted =
            convert_messages(&messages, false, None, true, None, None).expect("convert");
        let params = converted.messages;
        let blocks = params[0]["content"].as_array().expect("blocks");
        assert_eq!(
            blocks[1],
            json!({"type": "thinking", "thinking": "unsigned thought", "signature": ""})
        );
    }

    #[test]
    fn test_convert_messages_redacted_thinking() {
        let messages = vec![assistant_message(
            vec![AssistantContent::Thinking(crate::types::ThinkingContent {
                thinking: "[Reasoning redacted]".to_owned(),
                thinking_signature: Some("opaque".to_owned()),
                redacted: Some(true),
            })],
            "anthropic",
            "claude-sonnet-4-5",
        )];
        let converted =
            convert_messages(&messages, false, None, false, None, None).expect("convert");
        let params = converted.messages;
        assert_eq!(
            params[0]["content"][0],
            json!({"type": "redacted_thinking", "data": "opaque"})
        );
    }

    #[test]
    fn test_convert_messages_tool_results_grouped() {
        let messages = vec![
            tool_result("call_1", "one"),
            tool_result("call_2", "two"),
            user_text("next"),
        ];
        let converted =
            convert_messages(&messages, false, None, false, None, None).expect("convert");
        let params = converted.messages;
        assert_eq!(params.len(), 2);
        let content = params[0]["content"].as_array().expect("content");
        assert_eq!(
            content[0],
            json!({"type": "tool_result", "tool_use_id": "call_1", "content": "one", "is_error": false})
        );
        assert_eq!(content[1]["tool_use_id"], json!("call_2"));
        assert_eq!(params[0]["role"], json!("user"));
        assert_eq!(params[1]["content"], json!("next"));
    }

    #[test]
    fn test_convert_messages_oauth_tool_names() {
        let messages = vec![assistant_message(
            vec![AssistantContent::ToolCall(crate::types::ToolCall {
                id: "toolu_1".to_owned(),
                name: "bash".to_owned(),
                arguments: serde_json::Map::new(),
                thought_signature: None,
                namespace: None,
            })],
            "anthropic",
            "claude-sonnet-4-5",
        )];
        let converted =
            convert_messages(&messages, true, None, false, None, None).expect("convert");
        let params = converted.messages;
        assert_eq!(params[0]["content"][0]["name"], json!("Bash"));
        let converted =
            convert_messages(&messages, false, None, false, None, None).expect("convert");
        let params = converted.messages;
        assert_eq!(params[0]["content"][0]["name"], json!("bash"));
    }

    #[test]
    fn test_convert_tools() {
        let tools = vec![tool("a"), tool("b")];
        let cc = json!({"type": "ephemeral"});
        let converted = convert_tools(&tools, false, true, false, Some(&cc), false).expect("tools");
        assert_eq!(converted.len(), 2);
        assert_eq!(
            converted[0]["input_schema"],
            json!({"type": "object", "properties": {"x": {"type": "string"}}, "required": ["x"]})
        );
        assert_eq!(converted[0]["eager_input_streaming"], json!(true));
        assert!(converted[0].get("cache_control").is_none());
        // cache_control only on the last tool.
        assert_eq!(converted[1]["cache_control"], cc);

        // Deferred loading; no eager streaming flag when unsupported.
        let converted = convert_tools(&tools[..1], false, false, false, None, true).expect("tools");
        assert_eq!(converted[0]["defer_loading"], json!(true));
        assert!(converted[0].get("eager_input_streaming").is_none());
    }

    #[test]
    fn test_convert_tools_strict() {
        let strict_tool: Tool = serde_json::from_value(json!({
            "name": "s", "description": "d",
            "parameters": {"type": "object", "properties": {"x": {"type": "string"}}, "required": ["x"], "additionalProperties": false},
            "constrainedSampling": {"type": "json_schema", "strict": "prefer"}
        }))
        .expect("tool");
        // Strict supported: full schema is kept and `strict: true` added.
        let converted = convert_tools(
            std::slice::from_ref(&strict_tool),
            false,
            true,
            true,
            None,
            false,
        )
        .expect("tools");
        assert_eq!(converted[0]["strict"], json!(true));
        assert_eq!(
            converted[0]["input_schema"]["additionalProperties"],
            json!(false)
        );
        // Strict unsupported with "prefer": silently degrades.
        let converted = convert_tools(
            std::slice::from_ref(&strict_tool),
            false,
            true,
            false,
            None,
            false,
        )
        .expect("tools");
        assert!(converted[0].get("strict").is_none());
        assert!(
            converted[0]["input_schema"]
                .get("additionalProperties")
                .is_none()
        );

        // "require" unsupported → error.
        let require_tool: Tool = serde_json::from_value(json!({
            "name": "r", "description": "d",
            "parameters": {"type": "object", "properties": {}, "required": []},
            "constrainedSampling": {"type": "json_schema", "strict": "require"}
        }))
        .expect("tool");
        assert!(convert_tools(&[require_tool], false, true, false, None, false).is_err());
    }

    /// #9953 (`295cc72b0`): "prefer" tools whose schema uses keywords
    /// Anthropic strict mode rejects are sent non-strict instead of failing
    /// the whole request.
    #[test]
    fn test_convert_tools_strict_unsupported_keywords_fall_back() {
        let strict_tool = |properties: Value| -> Tool {
            serde_json::from_value(json!({
                "name": "lookup", "description": "d",
                "parameters": {
                    "type": "object",
                    "properties": properties,
                    "required": ["value"],
                    "additionalProperties": false
                },
                "constrainedSampling": {"type": "json_schema", "strict": "prefer"}
            }))
            .expect("tool")
        };

        let unsupported = [
            json!({"value": {"type": "integer", "minimum": 1, "maximum": 300000}}),
            json!({"value": {"type": "string", "format": "regex"}}),
            json!({"value": {"type": "array", "items": {"type": "string"}, "minItems": 2}}),
        ];
        for properties in unsupported {
            let tool = strict_tool(properties.clone());
            let converted =
                convert_tools(std::slice::from_ref(&tool), false, true, true, None, false)
                    .expect("tools");
            assert!(
                converted[0].get("strict").is_none(),
                "keyword schema must fall back to non-strict: {properties}"
            );
        }

        // Supported keywords stay strict.
        let supported = strict_tool(json!({
            "value": {
                "type": "string", "minLength": 1, "maxLength": 1000,
                "pattern": "^[a-z]+$", "format": "uri"
            }
        }));
        let converted = convert_tools(
            std::slice::from_ref(&supported),
            false,
            true,
            true,
            None,
            false,
        )
        .expect("tools");
        assert_eq!(converted[0]["strict"], json!(true));
    }

    // -----------------------------------------------------------------------
    // build_params
    // -----------------------------------------------------------------------

    #[test]
    fn test_build_params_oauth_system_and_identity() {
        let model = make_model(json!({}));
        let ctx = crate::utils::transcript::normalize_context(&Context {
            system_prompt: Some("You are rpi.".to_owned()),
            messages: vec![user_text("hi")],
            tools: Some(vec![tool("bash")]),
        });
        let options = AnthropicOptions::default();
        let params = build_params(&model, &ctx, true, false, &options).expect("params");

        let system = params["system"].as_array().expect("system");
        assert_eq!(
            system[0]["text"],
            json!("You are Claude Code, Anthropic's official CLI for Claude.")
        );
        assert_eq!(system[0]["cache_control"], json!({"type": "ephemeral"}));
        assert_eq!(system[1]["text"], json!("You are rpi."));
        // OAuth canonicalizes tool names.
        assert_eq!(params["tools"][0]["name"], json!("Bash"));
    }

    /// FR-G R2 (eb1f87fa9): `fallbacks` is sent only when the compat lists
    /// permitted targets, as `[{model}]` entries.
    #[test]
    fn test_build_params_fallbacks_only_with_allowed_models() {
        let model = make_model(json!({
            "compat": {"allowedFallbackModels": [
                {"provider": "anthropic", "model": "claude-haiku-9", "cost": {"input": 1.0, "output": 2.0, "cacheRead": 0.0, "cacheWrite": 0.0}},
                {"provider": "anthropic", "model": "claude-sonnet-4-5-mini", "cost": {"input": 0.5, "output": 1.0, "cacheRead": 0.0, "cacheWrite": 0.0}}
            ]}
        }));
        let ctx = context(vec![user_text("hi")], None);
        let params =
            build_params(&model, &ctx, false, false, &AnthropicOptions::default()).expect("params");
        assert_eq!(
            params["fallbacks"],
            json!([{"model": "claude-haiku-9"}, {"model": "claude-sonnet-4-5-mini"}])
        );

        // Absent or empty → the field is omitted entirely.
        let empty = make_model(json!({"compat": {"allowedFallbackModels": []}}));
        let params =
            build_params(&empty, &ctx, false, false, &AnthropicOptions::default()).expect("params");
        assert!(params.get("fallbacks").is_none());
        let plain = make_model(json!({}));
        let params =
            build_params(&plain, &ctx, false, false, &AnthropicOptions::default()).expect("params");
        assert!(params.get("fallbacks").is_none());
    }

    /// FR-C R2 (4e69b0c28): managed models never send `temperature`, even
    /// with thinking off and a temperature requested.
    #[test]
    fn test_build_params_managed_never_sends_temperature() {
        let model = make_model(json!({"compat": {"supportsMidConvoEffort": true}}));
        let ctx = context(vec![user_text("hi")], None);
        let mut options = AnthropicOptions::default();
        options.stream.temperature = Some(0.5);
        let params = build_params(&model, &ctx, false, false, &options).expect("params");
        assert!(params.get("temperature").is_none());
    }

    #[test]
    fn test_build_params_temperature_gating() {
        let model = make_model(json!({}));
        let ctx = context(vec![user_text("hi")], None);
        let mut options = AnthropicOptions::default();
        options.stream.temperature = Some(0.5);

        let params = build_params(&model, &ctx, false, false, &options).expect("params");
        assert_eq!(params["temperature"], json!(0.5));

        // Temperature is incompatible with extended thinking.
        options.thinking_enabled = Some(true);
        let params = build_params(&model, &ctx, false, false, &options).expect("params");
        assert!(params.get("temperature").is_none());

        // ... and unsupported on models without temperature support.
        let model = make_model(json!({"compat": {"supportsTemperature": false}}));
        options.thinking_enabled = None;
        let params = build_params(&model, &ctx, false, false, &options).expect("params");
        assert!(params.get("temperature").is_none());
    }

    #[test]
    fn test_build_params_thinking_modes() {
        let ctx = context(vec![user_text("hi")], None);

        // Budget-based thinking.
        let model = make_model(json!({}));
        let mut options = AnthropicOptions {
            thinking_enabled: Some(true),
            thinking_budget_tokens: Some(2048),
            ..AnthropicOptions::default()
        };
        let params = build_params(&model, &ctx, false, false, &options).expect("params");
        assert_eq!(
            params["thinking"],
            json!({"type": "enabled", "budget_tokens": 2048, "display": "summarized"})
        );

        // Adaptive thinking with effort.
        let model = make_model(json!({"compat": {"forceAdaptiveThinking": true}}));
        options.effort = Some("high".to_owned());
        let params = build_params(&model, &ctx, false, false, &options).expect("params");
        assert_eq!(
            params["thinking"],
            json!({"type": "adaptive", "display": "summarized"})
        );
        assert_eq!(params["output_config"], json!({"effort": "high"}));

        // Explicitly disabled.
        let mut options = AnthropicOptions {
            thinking_enabled: Some(false),
            ..AnthropicOptions::default()
        };
        let params = build_params(&model, &ctx, false, false, &options).expect("params");
        assert_eq!(params["thinking"], json!({"type": "disabled"}));

        // thinkingLevelMap.off === null keeps thinking omitted.
        let model = make_model(json!({"thinkingLevelMap": {"off": null}}));
        let params = build_params(&model, &ctx, false, false, &options).expect("params");
        assert!(params.get("thinking").is_none());

        // thinkingEnabled None → no thinking param at all.
        options.thinking_enabled = None;
        let params = build_params(&model, &ctx, false, false, &options).expect("params");
        assert!(params.get("thinking").is_none());
    }

    #[test]
    fn test_build_params_metadata_and_tool_choice() {
        let model = make_model(json!({}));
        let ctx = context(vec![user_text("hi")], None);
        let mut options = AnthropicOptions {
            tool_choice: Some(AnthropicToolChoice::Any),
            ..AnthropicOptions::default()
        };
        options
            .stream
            .metadata
            .get_or_insert_with(serde_json::Map::new)
            .insert("user_id".to_owned(), json!("u-1"));
        let params = build_params(&model, &ctx, false, false, &options).expect("params");
        assert_eq!(params["metadata"], json!({"user_id": "u-1"}));
        assert_eq!(params["tool_choice"], json!({"type": "any"}));

        options.tool_choice = Some(AnthropicToolChoice::Tool {
            name: "search".to_owned(),
        });
        let params = build_params(&model, &ctx, false, false, &options).expect("params");
        assert_eq!(
            params["tool_choice"],
            json!({"type": "tool", "name": "search"})
        );
    }

    #[test]
    fn test_build_params_normalizes_cross_model_tool_call_ids() {
        let model = make_model(json!({}));
        let messages = vec![
            // Assistant message from a different model: id normalization applies.
            assistant_message(
                vec![AssistantContent::ToolCall(crate::types::ToolCall {
                    id: "call_abc|item_xyz".to_owned(),
                    name: "bash".to_owned(),
                    arguments: serde_json::Map::new(),
                    thought_signature: None,
                    namespace: None,
                })],
                "openai",
                "gpt-5",
            ),
            tool_result("call_abc|item_xyz", "ok"),
        ];
        let ctx = context(messages, None);
        let params =
            build_params(&model, &ctx, false, false, &AnthropicOptions::default()).expect("params");
        let assistant = &params["messages"][0];
        assert_eq!(assistant["content"][0]["id"], json!("call_abc_item_xyz"));
        // The tool result id is remapped to the normalized id.
        let result = &params["messages"][1];
        assert_eq!(
            result["content"][0]["tool_use_id"],
            json!("call_abc_item_xyz")
        );
    }

    /// #9548: without mid-conversation support the transcript collapses —
    /// every declared tool stays top-level (no defer_loading) and tool
    /// results carry plain content (the tool_reference discovery blocks are
    /// gone with the addedToolNames mechanism).
    #[test]
    fn test_build_params_collapsed_tools_without_mid_convo() {
        let model = make_model(json!({}));
        let messages = vec![
            assistant_message(
                vec![AssistantContent::ToolCall(crate::types::ToolCall {
                    id: "toolu_1".to_owned(),
                    name: "read".to_owned(),
                    arguments: serde_json::Map::new(),
                    thought_signature: None,
                    namespace: None,
                })],
                "anthropic",
                "claude-sonnet-4-5",
            ),
            tool_result("toolu_1", "loaded search"),
        ];
        let ctx = context(messages, Some(vec![tool("read"), tool("search")]));
        let params =
            build_params(&model, &ctx, false, false, &AnthropicOptions::default()).expect("params");
        let tools = params["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["name"], json!("read"));
        assert_eq!(tools[1]["name"], json!("search"));
        assert!(tools.iter().all(|tool| tool.get("defer_loading").is_none()));

        // Plain tool_result content — no tool_reference blocks.
        let content = params["messages"][1]["content"]
            .as_array()
            .expect("content");
        assert_eq!(
            content[0],
            json!({
                "type": "tool_result",
                "tool_use_id": "toolu_1",
                "content": "loaded search",
                "is_error": false,
                "cache_control": {"type": "ephemeral"}
            })
        );
    }

    // -----------------------------------------------------------------------
    // build_params
    // -----------------------------------------------------------------------

    /// FR-G R2 (eb1f87fa9): `fallbacks` is sent only when the compat lists
    /// permitted targets, as `[{model}]` entries.
    /// FR-C R2 (4e69b0c28): managed models never send `temperature`, even
    /// with thinking off and a temperature requested.
    #[test]
    fn test_build_params_oauth_used_name_canonicalized_in_tool_list() {
        // Post-#9548: the addedToolNames discovery mechanism is gone — the
        // OAuth canonicalization applies to every declared tool name (a prior
        // assistant call already used the canonical "Read"), and the request
        // list carries the canonical name with no deferral split.
        let model = make_model(json!({}));
        let messages = vec![
            assistant_message(
                vec![AssistantContent::ToolCall(crate::types::ToolCall {
                    id: "toolu_1".to_owned(),
                    name: "Read".to_owned(),
                    arguments: serde_json::Map::new(),
                    thought_signature: None,
                    namespace: None,
                })],
                "anthropic",
                "claude-sonnet-4-5",
            ),
            tool_result("toolu_1", "loaded"),
        ];
        let ctx = context(messages, Some(vec![tool("base_tool"), tool("read")]));
        let params =
            build_params(&model, &ctx, true, false, &AnthropicOptions::default()).expect("params");
        let tools = params["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["name"], json!("base_tool"));
        assert_eq!(tools[1]["name"], json!("Read"));
        assert!(tools.iter().all(|t| t.get("defer_loading").is_none()));
        let body = serde_json::to_string(&params).expect("serialize");
        assert!(!body.contains("tool_reference"));
    }

    #[test]
    fn test_build_params_oauth_canonicalizes_declared_names() {
        // Post-#9548: the addedToolNames discovery mechanism is gone — the
        // OAuth canonicalization still applies to every declared tool name
        // (request-level list), with no defer_loading splits.
        let model = make_model(json!({}));
        let messages = vec![
            assistant_message(
                vec![AssistantContent::ToolCall(crate::types::ToolCall {
                    id: "toolu_1".to_owned(),
                    name: "base_tool".to_owned(),
                    arguments: serde_json::Map::new(),
                    thought_signature: None,
                    namespace: None,
                })],
                "anthropic",
                "claude-sonnet-4-5",
            ),
            tool_result("toolu_1", "loaded"),
        ];
        let ctx = context(messages, Some(vec![tool("base_tool"), tool("read")]));
        let params =
            build_params(&model, &ctx, true, false, &AnthropicOptions::default()).expect("params");
        let tools = params["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["name"], json!("base_tool"));
        assert_eq!(tools[1]["name"], json!("Read"));
        assert!(tools.iter().all(|tool| tool.get("defer_loading").is_none()));
        assert!(!params.to_string().contains("tool_reference"));
    }

    // -----------------------------------------------------------------------
    // map_stop_reason
    // -----------------------------------------------------------------------

    #[test]
    fn test_map_stop_reason() {
        assert_eq!(
            map_stop_reason("end_turn", None),
            Ok((StopReason::Stop, None))
        );
        assert_eq!(
            map_stop_reason("max_tokens", None),
            Ok((StopReason::Length, None))
        );
        assert_eq!(
            map_stop_reason("tool_use", None),
            Ok((StopReason::ToolUse, None))
        );
        assert_eq!(
            map_stop_reason("pause_turn", None),
            Ok((StopReason::Stop, None))
        );
        assert_eq!(
            map_stop_reason("stop_sequence", None),
            Ok((StopReason::Stop, None))
        );
        assert_eq!(
            map_stop_reason("sensitive", None),
            Ok((
                StopReason::Error,
                Some("Provider stopped with: sensitive".to_owned())
            ))
        );
        assert_eq!(
            map_stop_reason("refusal", None),
            Ok((
                StopReason::Error,
                Some("The model refused to complete the request".to_owned())
            ))
        );
        let details = json!({"explanation": "custom reason"});
        assert_eq!(
            map_stop_reason("refusal", Some(&details)),
            Ok((StopReason::Error, Some("custom reason".to_owned())))
        );
        assert_eq!(
            map_stop_reason("brand_new", None),
            Err("Unhandled stop reason: brand_new".to_owned())
        );
    }

    // -----------------------------------------------------------------------
    // SSE stream processing
    // -----------------------------------------------------------------------

    fn drive_sse(
        model: &Model,
        bytes: &[u8],
    ) -> (
        Vec<StreamEvent>,
        Result<DoneReason, String>,
        AssistantMessage,
    ) {
        let events = AssistantMessageEventStream::new();
        let mut output = initial_output(model);
        let reason = {
            let mut processor = StreamProcessor::new(&mut output, model, false, None);
            let mut decoder = SseDecoder::new();
            let mut result = Ok(());
            for sse in decoder.feed(bytes).unwrap() {
                if let Err(error) = processor.handle_sse(&sse, &events) {
                    result = Err(error);
                    break;
                }
            }
            if result.is_ok() {
                for sse in decoder.finish().unwrap() {
                    if let Err(error) = processor.handle_sse(&sse, &events) {
                        result = Err(error);
                        break;
                    }
                }
            }
            match result {
                Ok(()) => processor.finish(None),
                Err(error) => Err(error),
            }
        };
        events.end(None);
        let collected: Vec<StreamEvent> = futures::executor::block_on(events.collect());
        (collected, reason, output)
    }

    /// FR-G R3 (eb1f87fa9) + #9188 (1283afd0d): `message_start` may report
    /// a fallback model; the requested id stays on `output.model` (signed
    /// thinking replay stays valid), the served model is recorded on
    /// `response_model`, and cost follows the fallback target's
    /// compat-listed pricing.
    #[test]
    fn message_start_fallback_model_bills_fallback_cost() {
        let model = make_model(json!({
            "compat": {
                "allowedFallbackModels": [
                    {"provider": "anthropic", "model": "claude-haiku-9", "cost": {
                        "input": 2_000_000.0, "output": 4_000_000.0,
                        "cacheRead": 0.0, "cacheWrite": 0.0
                    }}
                ]
            }
        }));
        let sse = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_f\",\"model\":\"claude-haiku-9\",\"usage\":{\"input_tokens\":100,\"output_tokens\":50}}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":100,\"output_tokens\":50}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let (_events, reason, output) = drive_sse(&model, sse.as_bytes());
        assert_eq!(reason, Ok(DoneReason::Stop));
        // #9188: the requested model id is authoritative on the message.
        assert_eq!(output.model, "claude-sonnet-4-5");
        assert_eq!(output.response_model.as_deref(), Some("claude-haiku-9"));
        // 100 × 2,000,000/1e6 = 200; 50 × 4,000,000/1e6 = 200 — the
        // requested model's rates (3/15) would have given 0.3/0.75.
        assert_eq!(output.usage.cost.input, 200.0);
        assert_eq!(output.usage.cost.output, 200.0);
    }

    /// FR-G R3: an unknown served model keeps the requested model's rates
    /// (recorded on `response_model`, #9188).
    #[test]
    fn message_start_unknown_model_keeps_requested_rates() {
        let model = make_model(json!({}));
        let sse = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_f\",\"model\":\"claude-unknown-9\",\"usage\":{\"input_tokens\":1000000,\"output_tokens\":0}}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":1000000,\"output_tokens\":0}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let (_events, reason, output) = drive_sse(&model, sse.as_bytes());
        assert_eq!(reason, Ok(DoneReason::Stop));
        assert_eq!(output.model, "claude-sonnet-4-5");
        assert_eq!(output.response_model.as_deref(), Some("claude-unknown-9"));
        // Requested model rates: 3.0/M input.
        assert_eq!(output.usage.cost.input, 3.0);
    }

    /// #9188 (1283afd0d): a relay relabeling the response model must not
    /// break signed thinking replay — the assistant message keeps the
    /// requested model id, so the same-model transform preserves the
    /// thinking block with its signature.
    #[test]
    fn renamed_response_model_keeps_signed_thinking_replayable() {
        let model = make_model(json!({}));
        let sse = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_response_model\",\"model\":\"kimi-for-coding\",\"usage\":{\"input_tokens\":100,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"reasoning\",\"signature\":\"signature\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":100,\"output_tokens\":20}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let (_events, reason, output) = drive_sse(&model, sse.as_bytes());
        assert_eq!(reason, Ok(DoneReason::Stop));
        assert_eq!(output.model, "claude-sonnet-4-5");
        assert_eq!(output.response_model.as_deref(), Some("kimi-for-coding"));

        // Replay through the transform keeps the signed thinking block.
        let messages = vec![user_text("Hello"), Message::Assistant(output)];
        let transformed = transform_messages(&messages, &model, None);
        let Message::Assistant(replayed) = transformed
            .iter()
            .find(|message| matches!(message, Message::Assistant(_)))
            .expect("assistant message")
        else {
            unreachable!()
        };
        assert_eq!(replayed.content.len(), 1);
        match &replayed.content[0] {
            AssistantContent::Thinking(thinking) => {
                assert_eq!(thinking.thinking, "reasoning");
                assert_eq!(thinking.thinking_signature.as_deref(), Some("signature"));
            }
            other => panic!("expected thinking block, got {other:?}"),
        }
    }

    /// FR-G R4 (:618-623): a `fallback` marker block opening the output is
    /// skipped; arriving mid-output is an error.
    #[test]
    fn fallback_block_first_skipped_mid_output_errors() {
        let model = make_model(json!({}));
        // First block: the fallback marker is skipped silently.
        let sse = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"fallback\"}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"ok\"}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let (events, reason, output) = drive_sse(&model, sse.as_bytes());
        assert_eq!(reason, Ok(DoneReason::Stop));
        assert_eq!(output.content.len(), 1);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, StreamEvent::TextStart { .. }))
        );

        // Mid-output: hard error with the upstream message.
        let sse = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"first\"}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"fallback\"}}\n\n",
        );
        let (_events, reason, _output) = drive_sse(&model, sse.as_bytes());
        assert_eq!(
            reason,
            Err("Anthropic performed an unsupported mid-output model fallback".to_owned())
        );
    }

    /// FR-F (4e69b0c28): `input_transformations` from `message_start` and
    /// `message_delta` surface as an `anthropic_input_transformations`
    /// diagnostic; `message_delta` replaces the earlier array; absent arrays
    /// leave no diagnostic.
    #[test]
    fn input_transformations_become_diagnostics() {
        let model = make_model(json!({}));
        let sse = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"usage\":{\"input_tokens\":1,\"output_tokens\":0},\"input_transformations\":[{\"type\":\"thinking_dropped\",\"path\":\"/content/0\"}]}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":1,\"output_tokens\":1},\"input_transformations\":[{\"type\":\"thinking_dropped\",\"path\":\"/content/0\"},{\"reason\":\"prefix_mismatch\"}]}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let (_events, reason, output) = drive_sse(&model, sse.as_bytes());
        assert_eq!(reason, Ok(DoneReason::Stop));
        let diagnostics = output.diagnostics.as_deref().expect("diagnostics");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].kind, "anthropic_input_transformations");
        let details = diagnostics[0].details.as_ref().expect("details");
        assert_eq!(
            details.get("transformations"),
            Some(&json!([
                {"type": "thinking_dropped", "path": "/content/0"},
                {"reason": "prefix_mismatch"},
            ])),
            "the message_delta array replaces the message_start one; absent              keys (like `type` on the second entry) are dropped"
        );

        // No transformations anywhere → no diagnostics.
        let sse = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let (_events, reason, output) = drive_sse(&model, sse.as_bytes());
        assert_eq!(reason, Ok(DoneReason::Stop));
        assert!(output.diagnostics.is_none());
    }

    const RECORDED_STREAM: &str = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_123\",\"usage\":{\"input_tokens\":10,\"output_tokens\":1,\"cache_read_input_tokens\":5,\"cache_creation_input_tokens\":2,\"cache_creation\":{\"ephemeral_1h_input_tokens\":1}}}}\n",
        "\n",
        "event: ping\n",
        "data: {\"type\":\"ping\"}\n",
        "\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n",
        "\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"let me \"}}\n",
        "\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig1\"}}\n",
        "\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
        "\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
        "\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n",
        "\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":1}\n",
        "\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"bash\",\"input\":{}}}\n",
        "\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"command\\\":\"}}\n",
        "\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"ls\\\"}\"}}\n",
        "\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":2}\n",
        "\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":25,\"output_tokens_details\":{\"thinking_tokens\":10}}}\n",
        "\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n",
        "\n",
    );

    #[test]
    fn test_stream_processor_recorded_flow() {
        let model = make_model(json!({}));
        let (events, reason, output) = drive_sse(&model, RECORDED_STREAM.as_bytes());

        assert_eq!(reason, Ok(DoneReason::ToolUse));
        assert_eq!(output.response_id.as_deref(), Some("msg_123"));
        assert_eq!(output.stop_reason, StopReason::ToolUse);
        assert_eq!(output.usage.input, 10);
        assert_eq!(output.usage.output, 25);
        assert_eq!(output.usage.cache_read, 5);
        assert_eq!(output.usage.cache_write, 2);
        assert_eq!(output.usage.cache_write1h, Some(1));
        assert_eq!(output.usage.reasoning, Some(10));
        assert_eq!(output.usage.total_tokens, 42);

        // Content: thinking (with signature), text, tool call.
        assert_eq!(output.content.len(), 3);
        let AssistantContent::Thinking(thinking) = &output.content[0] else {
            panic!("expected thinking");
        };
        assert_eq!(thinking.thinking, "let me ");
        assert_eq!(thinking.thinking_signature.as_deref(), Some("sig1"));
        let AssistantContent::ToolCall(call) = &output.content[2] else {
            panic!("expected tool call");
        };
        assert_eq!(call.id, "toolu_1");
        assert_eq!(call.name, "bash");
        assert_eq!(call.arguments.get("command"), Some(&json!("ls")));

        // Event sequence (ping is filtered out).
        let kinds: Vec<&str> = events
            .iter()
            .map(|event| match event {
                StreamEvent::ThinkingStart { .. } => "thinking_start",
                StreamEvent::ThinkingDelta { .. } => "thinking_delta",
                StreamEvent::ThinkingEnd { .. } => "thinking_end",
                StreamEvent::TextStart { .. } => "text_start",
                StreamEvent::TextDelta { .. } => "text_delta",
                StreamEvent::TextEnd { .. } => "text_end",
                StreamEvent::ToolCallStart { .. } => "toolcall_start",
                StreamEvent::ToolCallDelta { .. } => "toolcall_delta",
                StreamEvent::ToolCallEnd { .. } => "toolcall_end",
                _ => "other",
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "thinking_start",
                "thinking_delta",
                "thinking_end",
                "text_start",
                "text_delta",
                "text_end",
                "toolcall_start",
                "toolcall_delta",
                "toolcall_delta",
                "toolcall_end",
            ]
        );
    }

    /// #9210 (`667fc3dd3`): Vercel AI Gateway reports the 1-hour
    /// cache-write breakdown in the streaming `message_delta` rather than
    /// `message_start`; it must price at the 2x input rate.
    #[test]
    fn test_stream_processor_prices_delta_only_1h_cache_writes() {
        let model = make_model(json!({}));
        let bytes = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_test\",\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}\n",
            "\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":3,\"output_tokens\":4,\"cache_creation_input_tokens\":6535,\"cache_creation\":{\"ephemeral_5m_input_tokens\":0,\"ephemeral_1h_input_tokens\":6535}}}\n",
            "\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n",
            "\n",
        );
        let (_, reason, output) = drive_sse(&model, bytes.as_bytes());
        assert_eq!(reason, Ok(DoneReason::Stop));
        assert_eq!(output.usage.cache_write, 6535);
        assert_eq!(output.usage.cache_write1h, Some(6535));
        assert!(
            (output.usage.cost.cache_write - 6535.0 * model.cost.rates.input * 2.0 / 1_000_000.0)
                .abs()
                < 1e-12,
            "1h writes price at 2x input: {}",
            output.usage.cost.cache_write
        );
    }

    /// Ported intent of `anthropic-sse-parsing.test.ts` @ 4181f66:
    /// "preserves content from content_block_start events" (59ad3dead,
    /// #7358). The initial `text`/`thinking`/`signature` carried by
    /// content_block_start must survive into the final message.
    #[test]
    fn test_stream_processor_preserves_content_block_start_content() {
        let model = make_model(json!({}));
        let bytes = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_initial_content\",\"usage\":{\"input_tokens\":12,\"output_tokens\":0,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}}\n",
            "\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"Initial text\"}}\n",
            "\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" plus delta\"}}\n",
            "\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
            "\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"Initial thinking\",\"signature\":\"initial signature\"}}\n",
            "\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\" plus delta\"}}\n",
            "\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"signature_delta\",\"signature\":\" plus delta\"}}\n",
            "\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":1}\n",
            "\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":12,\"output_tokens\":5,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}\n",
            "\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n",
            "\n",
        );
        let (_, reason, output) = drive_sse(&model, bytes.as_bytes());
        assert_eq!(reason, Ok(DoneReason::Stop));

        assert_eq!(output.content.len(), 2);
        let AssistantContent::Text(text) = &output.content[0] else {
            panic!("expected text block, got {:?}", output.content[0]);
        };
        assert_eq!(text.text, "Initial text plus delta");
        let AssistantContent::Thinking(thinking) = &output.content[1] else {
            panic!("expected thinking block, got {:?}", output.content[1]);
        };
        assert_eq!(thinking.thinking, "Initial thinking plus delta");
        assert_eq!(
            thinking.thinking_signature.as_deref(),
            Some("initial signature plus delta")
        );
    }

    #[test]
    fn test_stream_processor_sse_error_event() {
        let model = make_model(json!({}));
        let (_, reason, _) = drive_sse(
            &model,
            b"event: error\ndata: {\"type\":\"error\",\"error\":{\"message\":\"overloaded\"}}\n\n",
        );
        assert_eq!(
            reason,
            Err("{\"type\":\"error\",\"error\":{\"message\":\"overloaded\"}}".to_owned())
        );
    }

    #[test]
    fn test_stream_processor_ended_before_message_stop() {
        let model = make_model(json!({}));
        let bytes = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"usage\":{\"input_tokens\":1}}}\n",
            "\n"
        );
        let (_, reason, _) = drive_sse(&model, bytes.as_bytes());
        assert_eq!(
            reason,
            Err("Anthropic stream ended before message_stop".to_owned())
        );
    }

    #[test]
    fn test_stream_processor_missing_stop_reason() {
        let model = make_model(json!({}));
        let (_, reason, _) = drive_sse(&model, b"");
        assert_eq!(
            reason,
            Err("Anthropic stream ended without a stop reason".to_owned())
        );
    }

    #[test]
    fn test_stream_processor_refusal_maps_to_error() {
        let model = make_model(json!({}));
        let bytes = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"usage\":{\"input_tokens\":1}}}\n",
            "\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\",\"stop_details\":{\"explanation\":\"cannot help\"}}}\n",
            "\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n",
            "\n"
        );
        let (_, reason, output) = drive_sse(&model, bytes.as_bytes());
        assert_eq!(reason, Err("cannot help".to_owned()));
        assert_eq!(output.stop_reason, StopReason::Error);
        assert_eq!(output.error_message.as_deref(), Some("cannot help"));
        // 926eb15c1: the raw provider reason is preserved.
        assert_eq!(output.raw_stop_reason.as_deref(), Some("refusal"));
    }

    /// anthropic-sse-parsing.test.ts: "preserves sensitive stop reasons with a
    /// descriptive error message" (926eb15c1; text unified by 5a2539a7b @
    /// 4181f66).
    #[test]
    fn preserves_sensitive_stop_reasons_with_a_descriptive_error_message() {
        let model = make_model(json!({}));
        let bytes = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_sensitive\",\"usage\":{\"input_tokens\":12,\"output_tokens\":0,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}}\n",
            "\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"sensitive\"},\"usage\":{\"input_tokens\":12,\"output_tokens\":0,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}\n",
            "\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n",
            "\n"
        );
        let (_, reason, output) = drive_sse(&model, bytes.as_bytes());
        assert_eq!(reason, Err("Provider stopped with: sensitive".to_owned()));
        assert_eq!(output.stop_reason, StopReason::Error);
        assert_eq!(output.raw_stop_reason.as_deref(), Some("sensitive"));
        assert_eq!(
            output.error_message.as_deref(),
            Some("Provider stopped with: sensitive")
        );
    }

    // -----------------------------------------------------------------------
    // stream_simple
    // -----------------------------------------------------------------------

    /// 8b5899dce: `streamSimple` throws before returning a stream when
    /// request auth is missing (types.ts:325-330) — the Rust equivalent is
    /// the synchronous `Err` return, not an in-stream error event.
    #[test]
    fn test_stream_simple_missing_auth_fails_synchronously() {
        let model = make_model(json!({}));
        assert_eq!(
            stream_simple(&model, &context(vec![user_text("hi")], None), None).err(),
            Some("No API key for provider: anthropic".to_owned())
        );
    }

    /// rc.12 review (P1): a malformed body whose unterminated tail sits at
    /// the 1 MiB line cap with an incomplete UTF-8 sequence at EOF makes
    /// `SseDecoder::finish` fail — the adapter must surface that as a
    /// terminal `Error` event (fail-fast, rc.11 P2-4 semantics) instead of
    /// panicking the transport task (a panic never reaches the event
    /// stream — the shared `Arc<Inner>` sender keeps the channel open, so
    /// the turn would hang without a terminal event).
    #[tokio::test]
    async fn stream_line_cap_finish_failure_terminates_with_error_event() {
        use crate::api::sse::MAX_SSE_LINE_BYTES;
        use crate::types::{FetchError, FetchRequest, FetchResponse};

        // `data: ` + 'x' * (MAX - 7) + b'\xF0' — exactly MAX bytes, no
        // newline, ending in an incomplete UTF-8 sequence.
        let mut body = Vec::with_capacity(MAX_SSE_LINE_BYTES);
        body.extend_from_slice(b"data: ");
        body.resize(MAX_SSE_LINE_BYTES - 1, b'x');
        body.push(0xF0);
        assert_eq!(body.len(), MAX_SSE_LINE_BYTES);

        let fetch: crate::types::FetchFn = std::sync::Arc::new(move |_request: FetchRequest| {
            let body = body.clone();
            Box::pin(async move {
                Ok::<_, FetchError>(FetchResponse {
                    status: 200,
                    headers: vec![("content-type".to_owned(), "text/event-stream".to_owned())],
                    body: Box::pin(futures::stream::once(
                        async move { Ok::<_, FetchError>(body) },
                    )),
                })
            })
        });

        let model = make_model(json!({}));
        let ctx = context(vec![user_text("hi")], None);
        let stream = stream(
            &model,
            &ctx,
            AnthropicOptions {
                stream: crate::types::StreamOptions {
                    request: crate::types::ProviderRequestOptions {
                        api_key: Some("sk-test".to_owned()),
                        fetch: Some(fetch),
                        ..crate::types::ProviderRequestOptions::default()
                    },
                    ..crate::types::StreamOptions::default()
                },
                ..AnthropicOptions::default()
            },
        );
        let events: Vec<StreamEvent> =
            tokio::time::timeout(std::time::Duration::from_secs(10), stream.collect())
                .await
                .expect("stream must terminate instead of hanging");
        match events.last() {
            Some(StreamEvent::Error { error, .. }) => assert!(
                error
                    .error_message
                    .as_deref()
                    .is_some_and(|message| message.contains("SSE line exceeds")),
                "terminal Error must carry the line-cap message: {events:?}"
            ),
            other => panic!("terminal Error event expected, got {other:?} ({events:?})"),
        }
    }
}

#[cfg(test)]
mod mid_convo_effort_tests {
    //! Port of `packages/ai/test/anthropic-mid-conversation-effort.test.ts`
    //! @ 9841914 (4e69b0c28) — intents transplanted, wire params asserted
    //! field-by-field via the on_payload capture seam (upstream captures the
    //! SDK payload and throws; rpi captures and lets the unreachable
    //! `http://127.0.0.1:9` endpoint fail the request — the error message
    //! still carries the stream-start-initialized output).
    use std::sync::{Arc, Mutex};

    use serde_json::{Value, json};

    use super::tests::{assistant_message, context, make_model, user_text};
    use super::*;
    use crate::types::Context;

    fn managed_model(provider: &str) -> Model {
        make_model(json!({
            "id": "claude-fable-5-1",
            "name": "Claude Fable 5.1",
            "provider": provider,
            "baseUrl": "http://127.0.0.1:9",
            "compat": {
                "forceAdaptiveThinking": true,
                "supportsMidConvoEffort": true
            },
            "thinkingLevelMap": {
                "off": null, "minimal": "low", "low": "low",
                "medium": "medium", "high": "high", "max": "max"
            }
        }))
    }

    fn assistant_with_level(model: &Model, level: Option<&str>) -> Message {
        let mut message = assistant_message(
            vec![
                AssistantContent::Thinking(crate::types::ThinkingContent {
                    thinking: "reasoning".to_owned(),
                    thinking_signature: Some("signature".to_owned()),
                    redacted: None,
                }),
                AssistantContent::Text(crate::types::TextContent {
                    text: "answer".to_owned(),
                    text_signature: None,
                }),
            ],
            &model.provider,
            &model.id,
        );
        if let Message::Assistant(message) = &mut message
            && let Some(level) = level
        {
            message.provider_thinking_level = Some(level.to_owned());
        }
        message
    }

    async fn capture(
        model: &Model,
        context: &crate::types::TranscriptContext,
        effort: Option<&str>,
    ) -> (Value, AssistantMessage) {
        let payload = Arc::new(Mutex::new(Value::Null));
        let slot = payload.clone();
        let options = AnthropicOptions {
            stream: crate::types::StreamOptions {
                request: crate::types::ProviderRequestOptions {
                    api_key: Some("test-key".to_owned()),
                    on_payload: Some(Arc::new(move |value, _model| {
                        let slot = slot.clone();
                        Box::pin(async move {
                            *slot.lock().unwrap_or_else(|e| e.into_inner()) = value;
                            None
                        })
                    })),
                    ..Default::default()
                },
                cache_retention: Some(CacheRetention::None),
                ..Default::default()
            },
            thinking_enabled: Some(true),
            effort: effort.map(str::to_owned),
            ..Default::default()
        };
        let event_stream = stream(model, context, options);
        let message = event_stream
            .result()
            .await
            .expect("error message from unreachable endpoint");
        let captured = payload.lock().unwrap_or_else(|e| e.into_inner()).clone();
        (captured, message)
    }

    fn effort_markers(payload: &Value) -> Vec<&Value> {
        payload["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .filter(|message| message["role"] == json!("system"))
            .collect()
    }

    /// "reconstructs an exact historical marker prefix and appends the
    /// current marker".
    #[tokio::test]
    async fn reconstructs_exact_historical_marker_prefix_and_appends_current() {
        let model = managed_model("anthropic");
        let (first, first_message) =
            capture(&model, &context(vec![user_text("one")], None), Some("low")).await;
        assert_eq!(
            first["messages"],
            json!([
                {"role": "user", "content": "one"},
                {"role": "system", "content": [], "output_config": {"effort": "low"}},
            ])
        );
        let (second, _second_message) = capture(
            &model,
            &context(
                vec![
                    user_text("one"),
                    assistant_with_level(&model, Some("low")),
                    user_text("two"),
                ],
                None,
            ),
            Some("high"),
        )
        .await;
        let first_messages = first["messages"].as_array().expect("messages").clone();
        let second_messages = second["messages"].as_array().expect("messages").clone();
        assert_eq!(
            &second_messages[..first_messages.len()],
            &first_messages[..],
            "the historical prefix must be reconstructed exactly"
        );
        assert_eq!(
            second_messages.last(),
            Some(&json!({
                "role": "system", "content": [], "output_config": {"effort": "high"}
            }))
        );
        // Request-level controls are pinned regardless of the turn's effort.
        assert_eq!(first["output_config"], json!({"effort": "high"}));
        assert_eq!(second["output_config"], json!({"effort": "high"}));
        assert_eq!(
            second["thinking"],
            json!({
                "type": "adaptive",
                "display": "summarized",
                "block_binding": {"prefix_mismatch_behavior": "drop_block"},
            })
        );
        assert_eq!(
            first_message.provider_thinking_level.as_deref(),
            Some("low")
        );
    }

    /// "preserves native effort %s" over all five levels.
    #[tokio::test]
    async fn preserves_native_effort_all_levels() {
        for effort in ["low", "medium", "high", "xhigh", "max"] {
            let model = managed_model("anthropic");
            let (payload, message) =
                capture(&model, &context(vec![user_text("one")], None), Some(effort)).await;
            assert_eq!(
                effort_markers(&payload),
                vec![&json!({
                    "role": "system", "content": [], "output_config": {"effort": effort}
                })],
                "effort {effort}"
            );
            assert_eq!(message.provider_thinking_level.as_deref(), Some(effort));
        }
    }

    /// "defaults omitted effort to high and still enables drop_block".
    #[tokio::test]
    async fn defaults_omitted_effort_to_high_with_drop_block() {
        let (payload, message) = capture(
            &managed_model("anthropic"),
            &context(vec![user_text("one")], None),
            None,
        )
        .await;
        assert_eq!(
            payload["messages"].as_array().expect("messages").last(),
            Some(&json!({
                "role": "system", "content": [], "output_config": {"effort": "high"}
            }))
        );
        assert_eq!(
            payload["thinking"]["block_binding"]["prefix_mismatch_behavior"],
            json!("drop_block")
        );
        assert_eq!(message.provider_thinking_level.as_deref(), Some("high"));
    }

    /// "does not invent markers for legacy or other-provider assistants".
    #[tokio::test]
    async fn no_markers_for_legacy_or_other_provider_assistants() {
        let model = managed_model("anthropic");
        let mut other_provider_model = managed_model("other-provider");
        other_provider_model.provider = "other-provider".to_owned();
        let legacy = assistant_with_level(&model, None);
        let other_provider = assistant_with_level(&other_provider_model, Some("low"));
        // FR-H R3: an old openrouter session recorded Claude messages under
        // `openai-completions`; such assistants carry no anthropic-messages
        // api marker even with a level set.
        let mut legacy_api = assistant_with_level(&model, Some("low"));
        if let Message::Assistant(message) = &mut legacy_api {
            message.api = crate::types::ApiKind::from("openai-completions");
        }
        let (payload, _) = capture(
            &model,
            &context(
                vec![
                    user_text("one"),
                    legacy,
                    user_text("two"),
                    other_provider,
                    user_text("three"),
                    legacy_api,
                ],
                None,
            ),
            Some("medium"),
        )
        .await;
        assert_eq!(
            effort_markers(&payload),
            vec![&json!({
                "role": "system", "content": [], "output_config": {"effort": "medium"}
            })]
        );
    }

    /// "leaves unsupported models on top-level effort" — no markers, no
    /// block_binding, output_config carries the requested effort, and the
    /// message records no provider level.
    #[tokio::test]
    async fn unsupported_models_stay_on_top_level_effort() {
        let model = make_model(json!({
            "id": "claude-fable-5-1",
            "baseUrl": "http://127.0.0.1:9",
            "compat": {"forceAdaptiveThinking": true}
        }));
        let (payload, message) =
            capture(&model, &context(vec![user_text("one")], None), Some("low")).await;
        assert_eq!(
            payload["messages"],
            json!([{"role": "user", "content": "one"}])
        );
        assert_eq!(payload["output_config"], json!({"effort": "low"}));
        assert_eq!(
            payload["thinking"],
            json!({"type": "adaptive", "display": "summarized"})
        );
        assert_eq!(message.provider_thinking_level, None);
    }

    /// "sends the effort and binding beta headers" — asserted at the two
    /// layers rpi has (the beta-set computation and the emitted header).
    #[test]
    fn sends_effort_and_binding_beta_headers() {
        let model = managed_model("anthropic");
        let options = AnthropicOptions {
            thinking_enabled: Some(true),
            ..Default::default()
        };
        let betas = get_beta_features(
            &model,
            &crate::utils::transcript::normalize_context(&Context::default()),
            false,
            false,
            &options,
        );
        assert!(betas.contains(&"mid-conversation-output-config-2026-07-01".to_owned()));
        assert!(betas.contains(&"thinking-binding-controls-2026-08-01".to_owned()));
        let (headers, _) = build_request_headers(
            &model,
            &crate::utils::transcript::normalize_context(&Context::default()),
            false,
            Some("test-key"),
            &options,
            None,
            None,
            None,
        );
        let beta = headers
            .get("anthropic-beta")
            .and_then(|value| value.as_deref())
            .expect("anthropic-beta header");
        assert!(beta.contains("mid-conversation-output-config-2026-07-01"));
        assert!(beta.contains("thinking-binding-controls-2026-08-01"));
    }

    /// #9294 (b03a367a4 @ d1230ea20): the server-side-fallback beta header
    /// follows the effective `allowedFallbackModels` — set (non-empty)
    /// sends it; empty (user-disabled) or absent does not. The fallbacks
    /// body field itself is covered by
    /// `test_build_params_fallbacks_only_with_allowed_models`.
    #[test]
    fn server_side_fallback_beta_header_follows_effective_fallbacks() {
        let with_fallbacks = make_model(json!({
            "compat": {"allowedFallbackModels": [
                {"provider": "anthropic", "model": "claude-haiku-9", "cost": {"input": 1.0, "output": 2.0, "cacheRead": 0.0, "cacheWrite": 0.0}}
            ]}
        }));
        let betas = get_beta_features(
            &with_fallbacks,
            &crate::utils::transcript::normalize_context(&Context::default()),
            false,
            false,
            &AnthropicOptions::default(),
        );
        assert!(betas.contains(&SERVER_SIDE_FALLBACK_BETA.to_owned()));

        for compat in [json!({"allowedFallbackModels": []}), json!({})] {
            let model = make_model(json!({"compat": compat}));
            let betas = get_beta_features(
                &model,
                &crate::utils::transcript::normalize_context(&Context::default()),
                false,
                false,
                &AnthropicOptions::default(),
            );
            assert!(
                !betas.contains(&SERVER_SIDE_FALLBACK_BETA.to_owned()),
                "no fallback beta for compat {compat}"
            );
        }
    }

    /// Header override rules (:985-1000): an explicit `anthropic-beta`
    /// replaces the computed set; `null` clears it; comma lists are
    /// trimmed/deduplicated; options win over model headers.
    #[test]
    fn beta_header_overrides_replace_computed_set() {
        let model = managed_model("anthropic");
        let options = AnthropicOptions {
            thinking_enabled: Some(true),
            ..Default::default()
        };
        let base = crate::utils::transcript::normalize_context(&Context::default());

        // String override wins and is normalized.
        let mut with_model_headers = managed_model("anthropic");
        with_model_headers.headers = Some(
            [("Anthropic-Beta".to_owned(), " a ,,b ".to_owned())]
                .into_iter()
                .collect(),
        );
        assert_eq!(
            get_beta_features(&with_model_headers, &base, false, false, &options),
            vec!["a".to_owned(), "b".to_owned()]
        );

        // null clears (options headers are the only null-capable channel).
        let null_options = AnthropicOptions {
            stream: crate::types::StreamOptions {
                request: crate::types::ProviderRequestOptions {
                    headers: Some([("anthropic-beta".to_owned(), None)].into_iter().collect()),
                    ..Default::default()
                },
                ..Default::default()
            },
            thinking_enabled: Some(true),
            ..Default::default()
        };
        assert!(get_beta_features(&model, &base, false, false, &null_options).is_empty());

        // options beat model headers; dedup keeps first occurrence.
        let mut model_headers = managed_model("anthropic");
        model_headers.headers = Some(
            [("anthropic-beta".to_owned(), "x".to_owned())]
                .into_iter()
                .collect(),
        );
        let override_options = AnthropicOptions {
            stream: crate::types::StreamOptions {
                request: crate::types::ProviderRequestOptions {
                    headers: Some(
                        [("anthropic-beta".to_owned(), Some(" b , b ,a ".to_owned()))]
                            .into_iter()
                            .collect(),
                    ),
                    ..Default::default()
                },
                ..Default::default()
            },
            thinking_enabled: Some(true),
            ..Default::default()
        };
        assert_eq!(
            get_beta_features(&model_headers, &base, false, false, &override_options),
            vec!["b".to_owned(), "a".to_owned()]
        );
    }
}

#[cfg(test)]
mod header_semantics_tests {
    use serde_json::json;

    use super::tests::make_model;
    use super::*;
    use crate::types::Context;

    #[test]
    fn test_user_headers_override_key_derived_auth() {
        // SDK semantics: defaultHeaders are applied AFTER authHeaders, so a
        // user-supplied x-api-key/authorization overrides the key-derived one
        // (case-insensitive).
        let model = make_model(json!({}));
        let options_headers: ProviderHeaders =
            [("X-API-Key".to_owned(), Some("user-key".to_owned()))].into();
        let (headers, _) = build_request_headers(
            &model,
            &crate::utils::transcript::normalize_context(&Context::default()),
            false,
            Some("sk-key"),
            &AnthropicOptions {
                stream: crate::types::StreamOptions {
                    request: crate::types::ProviderRequestOptions {
                        headers: Some(options_headers),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
            None,
            None,
            None,
        );
        assert!(!headers.contains_key("x-api-key"));
        assert_eq!(
            headers.get("X-API-Key").and_then(|v| v.as_deref()),
            Some("user-key")
        );
    }

    #[test]
    fn test_none_value_suppresses_key_derived_auth() {
        let model = make_model(json!({}));
        let options_headers: ProviderHeaders = [("x-api-key".to_owned(), None)].into();
        let (headers, _) = build_request_headers(
            &model,
            &crate::utils::transcript::normalize_context(&Context::default()),
            false,
            Some("sk-key"),
            &AnthropicOptions {
                stream: crate::types::StreamOptions {
                    request: crate::types::ProviderRequestOptions {
                        headers: Some(options_headers),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
            None,
            None,
            None,
        );
        // The suppression marker replaces the key-derived header; it is
        // dropped at the HTTP boundary.
        assert_eq!(headers.get("x-api-key"), Some(&None));
        let map = provider_headers_to_header_map(&headers).expect("header map");
        assert!(map.get("x-api-key").is_none());
    }
}

#[cfg(test)]
mod fireworks_mid_convo_tools_tests {
    //! Port of the Fireworks Messages-model replay face after #9548
    //! (`9e05370b2`): the d92eb8d4b-era `supportsToolReferences` deferred
    //! loading (addedToolNames + tool_reference blocks) is gone — tool
    //! changes ride transcript system messages instead. The unsigned
    //! thinking replay (allowEmptySignature) survives; asserted via the
    //! on_payload capture seam (upstream captures the SDK payload; rpi
    //! captures at the same point and lets the unreachable
    //! `http://127.0.0.1:9` endpoint fail the request).
    use std::sync::{Arc, Mutex};

    use serde_json::{Value, json};

    use super::tests::{assistant_message, user_text};
    use super::*;
    use crate::types::{AssistantContent, Context, ToolResultContent};

    fn fireworks_catalog_model() -> Model {
        let mut model = crate::generated::get_builtin_model(
            "fireworks",
            "accounts/fireworks/models/deepseek-v4p1-flash",
        )
        .expect("catalog model")
        .clone();
        model.base_url = "http://127.0.0.1:9".to_owned();
        model
    }

    async fn capture(model: &Model, context: &crate::types::TranscriptContext) -> Value {
        let payload = Arc::new(Mutex::new(Value::Null));
        let slot = payload.clone();
        let options = AnthropicOptions {
            stream: crate::types::StreamOptions {
                request: crate::types::ProviderRequestOptions {
                    api_key: Some("test-key".to_owned()),
                    on_payload: Some(Arc::new(move |value, _model| {
                        let slot = slot.clone();
                        Box::pin(async move {
                            *slot.lock().unwrap_or_else(|e| e.into_inner()) = value;
                            None
                        })
                    })),
                    ..Default::default()
                },
                cache_retention: Some(CacheRetention::None),
                ..Default::default()
            },
            ..Default::default()
        };
        let event_stream = stream(model, context, options);
        let _ = event_stream
            .result()
            .await
            .expect("error message from unreachable endpoint");

        payload.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Unsigned thinking replays with `signature: ""` (allowEmptySignature,
    /// catalog face from #9323); a plain tool result carries no
    /// tool_reference blocks post-#9548.
    #[tokio::test]
    async fn replays_unsigned_thinking_with_plain_tool_results() {
        let model = fireworks_catalog_model();
        let assistant = assistant_message(
            vec![
                AssistantContent::Thinking(crate::types::ThinkingContent {
                    thinking: "Find a lookup tool.".to_owned(),
                    thinking_signature: Some(String::new()),
                    redacted: None,
                }),
                AssistantContent::ToolCall(crate::types::ToolCall {
                    id: "search1".to_owned(),
                    name: "ToolSearch".to_owned(),
                    arguments: json!({"query": "lookup"})
                        .as_object()
                        .cloned()
                        .unwrap_or_default(),
                    thought_signature: None,
                    namespace: None,
                }),
            ],
            &model.provider,
            &model.id,
        );
        let mut assistant = match assistant {
            Message::Assistant(mut message) => {
                message.stop_reason = StopReason::ToolUse;
                message
            }
            _ => unreachable!("assistant"),
        };
        let _ = &mut assistant;
        let tool_result = Message::ToolResult(crate::types::ToolResultMessage {
            role: crate::types::ToolResultRole::ToolResult,
            tool_call_id: "search1".to_owned(),
            tool_name: "ToolSearch".to_owned(),
            content: vec![ToolResultContent::Text(crate::types::TextContent {
                text: "Found lookup.".to_owned(),
                text_signature: None,
            })],
            details: None,
            usage: None,
            nested_calls: None,
            is_error: false,
            timestamp: 0,
            duration_ms: None,
        });
        let context = crate::utils::transcript::normalize_context(&Context {
            system_prompt: None,
            messages: vec![
                user_text("Look up alpha."),
                Message::Assistant(assistant),
                tool_result,
            ],
            tools: None,
        });
        let payload = capture(&model, &context).await;

        // Unsigned thinking replays with `signature: ""`.
        assert_eq!(
            payload["messages"][1]["content"][0],
            json!({"type": "thinking", "thinking": "Find a lookup tool.", "signature": ""})
        );
        // Plain tool_result — the tool_reference discovery block is gone
        // with the mechanism (#9548).
        assert_eq!(
            payload["messages"][2]["content"],
            json!([
                {
                    "type": "tool_result",
                    "tool_use_id": "search1",
                    "content": "Found lookup.",
                    "is_error": false
                }
            ])
        );
    }
}

#[cfg(test)]
mod vercel_unsigned_thinking_tests {
    //! Port of `packages/ai/test/anthropic-empty-thinking-signature-compat.test.ts`
    //! "allows empty thinking signatures for every Vercel AI Gateway model"
    //! (#9676, 3955b27a1) — adapter-level replay behavior. The Vercel catalog
    //! `compat.allowEmptySignature: true` data lands with the V15-03 regen;
    //! this asserts the replay semantics with an explicit compat overlay.
    use std::sync::{Arc, Mutex};

    use serde_json::{Value, json};

    use super::tests::{assistant_message, context, make_model, user_text};
    use super::*;
    use crate::types::AssistantContent;

    async fn capture(model: &Model) -> Value {
        let payload = Arc::new(Mutex::new(Value::Null));
        let slot = payload.clone();
        let assistant = assistant_message(
            vec![AssistantContent::Thinking(crate::types::ThinkingContent {
                thinking: "internal reasoning".to_owned(),
                thinking_signature: Some("".to_owned()),
                redacted: None,
            })],
            &model.provider,
            &model.id,
        );
        let options = AnthropicOptions {
            stream: crate::types::StreamOptions {
                request: crate::types::ProviderRequestOptions {
                    api_key: Some("test-key".to_owned()),
                    on_payload: Some(Arc::new(move |value, _model| {
                        let slot = slot.clone();
                        Box::pin(async move {
                            *slot.lock().unwrap_or_else(|e| e.into_inner()) = value;
                            None
                        })
                    })),
                    ..Default::default()
                },
                cache_retention: Some(CacheRetention::None),
                ..Default::default()
            },
            ..Default::default()
        };
        let context = context(vec![user_text("hi"), assistant, user_text("again")], None);
        let event_stream = stream(model, &context, options);
        let _ = event_stream
            .result()
            .await
            .expect("error message from unreachable endpoint");

        payload.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// #9676: Vercel AI Gateway emits unsigned thinking for translated
    /// models; with `allowEmptySignature` the empty signature survives replay
    /// (instead of converting thinking to a text block).
    #[tokio::test]
    async fn replays_empty_signature_thinking_for_vercel_gateway_models() {
        let model = make_model(json!({
            "id": "anthropic/claude-sonnet-4.6",
            "provider": "vercel-ai-gateway",
            "baseUrl": "http://127.0.0.1:9",
            "compat": {"allowEmptySignature": true}
        }));
        let payload = capture(&model).await;
        assert_eq!(
            payload["messages"][1]["content"],
            json!([{"type": "thinking", "thinking": "internal reasoning", "signature": ""}])
        );

        // Without the compat flag the unsigned thinking is NOT replayed as a
        // thinking block (converted to text upstream semantics).
        let model = make_model(json!({
            "id": "anthropic/claude-sonnet-4.6",
            "provider": "vercel-ai-gateway",
            "baseUrl": "http://127.0.0.1:9"
        }));
        let payload = capture(&model).await;
        let block_type = payload["messages"][1]["content"][0]["type"].as_str();
        assert_ne!(
            block_type,
            Some("thinking"),
            "unsigned thinking must not replay as thinking without compat"
        );
    }
}

#[cfg(test)]
mod transcript_tool_changes_tests {
    //! Port of the Anthropic face of `packages/ai/test/transcript-tool-changes.test.ts`
    //! (#9548, 9e05370b2): native mid-conversation system messages with
    //! `tool_addition`/`tool_removal` blocks, the deferred placeholder, the
    //! capability gating, and the non-native collapse fallback. Asserted via
    //! the on_payload capture seam (same pattern as the sibling modules:
    //! capture, then let the unreachable endpoint fail the request).
    use std::sync::{Arc, Mutex};

    use serde_json::{Value, json};

    use super::tests::{context, make_model, user_text};
    use super::*;
    use crate::types::{SystemMessage, TranscriptContext};

    fn tool_declaration(name: &str) -> Tool {
        // Upstream `tool(name)`: `{name, description: `${name} tool`, parameters: {}}`.
        serde_json::from_value(json!({
            "name": name,
            "description": format!("{name} tool"),
            "parameters": {"type": "object", "properties": {}}
        }))
        .expect("tool")
    }

    fn system_message(value: Value) -> Message {
        let system: SystemMessage = serde_json::from_value(value).expect("system message");
        Message::System(system)
    }

    /// Upstream `context` (transcript-tool-changes.test.ts:36-49): a leading
    /// declaration, one user turn, then an update with a section patch
    /// (`docs: null` removal) plus a tool swap.
    fn base_context() -> TranscriptContext {
        context(
            vec![
                system_message(json!({
                    "role": "system",
                    "content": "base prompt",
                    "sections": {
                        "rules": "<rules>\nold rules\n</rules>",
                        "docs": "<docs>\nread docs\n</docs>"
                    },
                    "toolsAdded": [tool_declaration("base_tool")],
                    "timestamp": 0
                })),
                user_text("before"),
                system_message(json!({
                    "role": "system",
                    "content": "updated guidance",
                    "sections": {
                        "rules": "<rules>\nnew rules\n</rules>",
                        "docs": null
                    },
                    "toolsRemoved": [{"name": "base_tool"}],
                    "toolsAdded": [tool_declaration("late_tool")],
                    "timestamp": 2
                })),
            ],
            None,
        )
    }

    fn native_model() -> Model {
        let mut model = make_model(json!({
            "id": "claude-opus-5",
            "name": "Claude Opus 5",
            "compat": {
                "supportsMidConvoSystemMessages": true,
                "supportsMidConvoToolChanges": true
            }
        }));
        model.base_url = "http://127.0.0.1:9".to_owned();
        model
    }

    async fn capture(model: &Model, context: &TranscriptContext) -> Value {
        let payload = Arc::new(Mutex::new(Value::Null));
        let slot = payload.clone();
        let options = AnthropicOptions {
            stream: crate::types::StreamOptions {
                request: crate::types::ProviderRequestOptions {
                    api_key: Some("test-key".to_owned()),
                    on_payload: Some(Arc::new(move |value, _model| {
                        let slot = slot.clone();
                        Box::pin(async move {
                            *slot.lock().unwrap_or_else(|e| e.into_inner()) = value;
                            None
                        })
                    })),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let event_stream = stream(model, context, options);
        // The endpoint is unreachable; the payload was captured first.
        let _ = event_stream.result().await;

        payload.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// "sends Anthropic updates and tool changes in native system messages"
    /// (transcript-tool-changes.test.ts:82).
    #[tokio::test]
    async fn sends_updates_and_tool_changes_in_native_system_messages() {
        let payload = capture(&native_model(), &base_context()).await;

        // The beta set rides the `anthropic-beta` header, NEVER the body
        // (the SDK moves `betas` into the header; a body-level array is
        // an unknown field a strict gateway may reject).
        assert!(
            payload.get("betas").is_none(),
            "body must not carry a top-level betas array: {}",
            payload["betas"]
        );
        let (headers, _) = build_request_headers(
            &native_model(),
            &base_context(),
            true,
            Some("sk-key"),
            &AnthropicOptions::default(),
            None,
            None,
            None,
        );
        let betas = headers
            .get("anthropic-beta")
            .and_then(|v| v.as_deref())
            .unwrap_or_default();
        assert!(
            betas
                .split(',')
                .any(|beta| beta == "inline-tools-2026-09-15"),
            "inline tool definitions beta present in the header: {betas:?}"
        );
        assert!(
            !betas
                .split(',')
                .any(|beta| beta == "mid-conversation-tool-changes-2026-07-01"),
            "the replaced beta must be gone: {betas:?}"
        );
        assert_eq!(
            payload["system"][0]["text"],
            "base prompt\n\n<rules>\nold rules\n</rules>\n\n<docs>\nread docs\n</docs>"
        );

        // The top-level list holds only the initial tools (with the cache
        // breakpoint) and the deferred placeholder; later tools never appear
        // there — the list is fixed so the cached prefix survives.
        let tools = payload["tools"].as_array().expect("tools");
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool["name"].as_str())
                .collect::<Vec<_>>(),
            vec![
                Some("base_tool"),
                Some(super::DEFERRED_TOOL_PLACEHOLDER_NAME)
            ]
        );
        assert_eq!(tools[0]["cache_control"], json!({"type": "ephemeral"}));
        assert!(tools[0].get("defer_loading").is_none());
        assert_eq!(tools[1]["defer_loading"], json!(true));
        assert!(tools[1].get("cache_control").is_none());

        let messages = payload["messages"].as_array().expect("messages");
        let update = messages.last().expect("trailing system update");
        assert_eq!(update["role"], "system");
        let block_types: Vec<&str> = update["content"]
            .as_array()
            .expect("content blocks")
            .iter()
            .map(|block| block["type"].as_str().expect("type"))
            .collect();
        assert_eq!(block_types, vec!["text", "tool_removal", "tool_addition"]);
        assert_eq!(update["content"][1]["tool"]["type"], "tool_reference");
        assert_eq!(update["content"][1]["tool"]["name"], "base_tool");
        // Later tools are defined by value; the block (not the definition)
        // carries the cache breakpoint.
        assert_eq!(update["content"][2]["tool"]["type"], "tool_definition");
        assert_eq!(
            update["content"][2]["tool"]["definition"]["name"],
            "late_tool"
        );
        assert_eq!(
            update["content"][2]["tool"]["definition"]["description"],
            "late_tool tool"
        );
        assert!(
            update["content"][2]["tool"]["definition"]
                .get("defer_loading")
                .is_none()
        );
        assert!(
            update["content"][2]["tool"]["definition"]
                .get("cache_control")
                .is_none()
        );
        assert_eq!(
            update["content"][2]["cache_control"],
            json!({"type": "ephemeral"})
        );
        let text = update["content"][0]["text"].as_str().expect("text");
        assert!(text.contains("updated guidance"), "{text}");
        assert!(text.contains("<rules>\nnew rules\n</rules>"), "{text}");
        assert!(
            text.contains("Removed system prompt section \"docs\"."),
            "{text}"
        );

        // The placeholder is declared before any change so its scaffolding
        // is cached from request one (upstream: initial-only transcript).
        let initial = context(
            vec![
                system_message(json!({
                    "role": "system",
                    "content": "base prompt",
                    "toolsAdded": [tool_declaration("base_tool")],
                    "timestamp": 0
                })),
                user_text("before"),
            ],
            None,
        );
        let initial_payload = capture(&native_model(), &initial).await;
        let names: Vec<&str> = initial_payload["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .map(|tool| tool["name"].as_str().expect("name"))
            .collect();
        assert_eq!(
            names,
            vec!["base_tool", super::DEFERRED_TOOL_PLACEHOLDER_NAME]
        );
    }

    /// "redefines an Anthropic tool by value under the same name"
    /// (transcript-tool-changes.test.ts @ b271b0a52): the top-level list keeps
    /// the original definition (cached prefix survives) and the new definition
    /// replaces the old one in place; no removal block is sent for it.
    #[tokio::test]
    async fn redefines_a_tool_by_value_under_the_same_name() {
        let redefined = serde_json::from_value::<Tool>(json!({
            "name": "base_tool",
            "description": "changed",
            "parameters": {"type": "object", "properties": {}}
        }))
        .expect("tool");
        let context = context(
            vec![
                system_message(json!({
                    "role": "system",
                    "content": "base prompt",
                    "toolsAdded": [tool_declaration("base_tool")],
                    "timestamp": 0
                })),
                user_text("before"),
                system_message(json!({
                    "role": "system",
                    "content": "",
                    "toolsRemoved": [{"name": "base_tool"}],
                    "toolsAdded": [redefined],
                    "timestamp": 2
                })),
            ],
            None,
        );
        let payload = capture(&native_model(), &context).await;
        assert!(
            payload.get("betas").is_none(),
            "the body never carries a betas array (header-only transport)"
        );
        let (headers, _) = build_request_headers(
            &native_model(),
            &context,
            true,
            Some("sk-key"),
            &AnthropicOptions::default(),
            None,
            None,
            None,
        );
        let betas = headers
            .get("anthropic-beta")
            .and_then(|v| v.as_deref())
            .unwrap_or_default();
        assert!(
            betas
                .split(',')
                .any(|beta| beta == "inline-tools-2026-09-15"),
            "inline tool definitions beta present in the header: {betas:?}"
        );
        // The top-level declaration keeps the original definition.
        let tools = payload["tools"].as_array().expect("tools");
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool["name"].as_str())
                .collect::<Vec<_>>(),
            vec![
                Some("base_tool"),
                Some(super::DEFERRED_TOOL_PLACEHOLDER_NAME)
            ]
        );
        assert_eq!(tools[0]["description"], "base_tool tool");
        // The new definition replaces the old one in place; only the addition
        // block is sent (no `tool_removal` for a redefined name).
        let messages = payload["messages"].as_array().expect("messages");
        let update = messages.last().expect("trailing system update");
        let blocks = update["content"].as_array().expect("content blocks");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "tool_addition");
        assert_eq!(blocks[0]["tool"]["type"], "tool_definition");
        assert_eq!(blocks[0]["tool"]["definition"]["name"], "base_tool");
        assert_eq!(blocks[0]["tool"]["definition"]["description"], "changed");
    }

    /// "sends the current Anthropic tool list without an initial tool"
    /// (transcript-tool-changes.test.ts @ b271b0a52): Anthropic rejects a
    /// tool list where every tool (here: the placeholder) is deferred, so a
    /// transcript with no initial tool falls back to the current list.
    #[tokio::test]
    async fn sends_current_tool_list_without_an_initial_tool() {
        let context = context(
            vec![
                system_message(json!({
                    "role": "system",
                    "content": "base prompt",
                    "timestamp": 0
                })),
                user_text("before"),
                system_message(json!({
                    "role": "system",
                    "content": "updated guidance",
                    "toolsAdded": [tool_declaration("late_tool")],
                    "timestamp": 2
                })),
            ],
            None,
        );
        let payload = capture(&native_model(), &context).await;
        assert!(
            payload.get("betas").is_none(),
            "the body never carries a betas array (header-only transport)"
        );
        let (headers, _) = build_request_headers(
            &native_model(),
            &context,
            false,
            Some("sk-key"),
            &AnthropicOptions::default(),
            None,
            None,
            None,
        );
        let betas = headers
            .get("anthropic-beta")
            .and_then(|v| v.as_deref())
            .unwrap_or_default();
        assert!(
            !betas
                .split(',')
                .any(|beta| beta == "inline-tools-2026-09-15"),
            "native tool changes must not engage without an initial tool: {betas:?}"
        );
        let tools = payload["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "late_tool");
        assert_eq!(tools[0]["cache_control"], json!({"type": "ephemeral"}));
        assert!(tools[0].get("defer_loading").is_none());
        // The update still rides an in-place system message (the model
        // accepts mid-conversation system messages) — text only.
        let messages = payload["messages"].as_array().expect("messages");
        let update = messages.last().expect("trailing system update");
        assert_eq!(update["role"], "system");
        let block_types: Vec<&str> = update["content"]
            .as_array()
            .expect("content blocks")
            .iter()
            .map(|block| block["type"].as_str().expect("type"))
            .collect();
        assert_eq!(block_types, vec!["text"]);
    }

    /// "folds Anthropic updates into the system prompt without native
    /// support" (transcript-tool-changes.test.ts:154).
    #[tokio::test]
    async fn folds_updates_into_system_prompt_without_native_support() {
        let mut model = make_model(json!({}));
        model.base_url = "http://127.0.0.1:9".to_owned();
        let payload = capture(&model, &base_context()).await;

        assert!(
            payload.get("betas").is_none(),
            "the body never carries a betas array (header-only transport)"
        );
        assert_eq!(
            payload["system"][0]["text"],
            "base prompt\n\nupdated guidance\n\n<rules>\nnew rules\n</rules>"
        );
        let tools = payload["tools"].as_array().expect("tools");
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool["name"].as_str())
                .collect::<Vec<_>>(),
            vec![Some("late_tool")]
        );
        let roles: Vec<&str> = payload["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .map(|message| message["role"].as_str().expect("role"))
            .collect();
        assert_eq!(roles, vec!["user"]);
    }

    /// "requires both Anthropic capabilities for native tool changes"
    /// (transcript-tool-changes.test.ts:177): `supportsMidConvoToolChanges`
    /// alone is not enough — without mid-conversation system messages the
    /// transcript collapses.
    #[tokio::test]
    async fn requires_both_capabilities_for_native_tool_changes() {
        let mut model = make_model(json!({
            "id": "claude-opus-5",
            "name": "Claude Opus 5",
            "compat": {"supportsMidConvoToolChanges": true}
        }));
        model.base_url = "http://127.0.0.1:9".to_owned();
        let payload = capture(&model, &base_context()).await;

        assert!(
            payload.get("betas").is_none(),
            "the body never carries a betas array (header-only transport)"
        );
        let tools = payload["tools"].as_array().expect("tools");
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool["name"].as_str())
                .collect::<Vec<_>>(),
            vec![Some("late_tool")]
        );
        let roles: Vec<&str> = payload["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .map(|message| message["role"].as_str().expect("role"))
            .collect();
        assert_eq!(roles, vec!["user"]);
    }
}
