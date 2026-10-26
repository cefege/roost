//! Port of `packages/ai/src/api/openai-completions.ts` @ pi 0.82.1 (2efa728);
//! stream-termination semantics (rawStopReason, `supportsFinishReason`,
//! function-vs-empty-custom tool-call deltas) updated to 4181f66 (fe1c9b6d5,
//! 2c3041242, 34239180a).
//!
//! OpenAI Chat Completions adapter: compat auto-detection (`detect_compat` /
//! `get_compat`), message conversion (tool-call id normalization, thinking
//! replay, tool-result grouping, Kimi deferred tools), Anthropic-style cache
//! control markers, the ten `thinkingFormat` request variants, SSE chunk
//! processing (text / reasoning / function & custom grammar tool calls /
//! encrypted reasoning details), and `stream_simple` reasoning mapping.
//!
//! Intentional differences (upstream deviations):
//! - HTTP is a direct reqwest call, not the `openai` SDK; the SDK's
//!   `x-stainless-*` telemetry headers, default `User-Agent` and platform
//!   headers are not sent, and there is no SDK default timeout (callers set
//!   `StreamOptions::timeout_ms`).
//! - SSE chunks are parsed with strict `serde_json` (the SDK uses
//!   `JSON.parse`, not a repairing parser); parse failures read
//!   `Could not parse OpenAI SSE chunk: {error}; data={data}` (the SDK would
//!   surface a `SyntaxError` message instead).
//! - The legacy env var is renamed `PI_CACHE_RETENTION` →
//!   `RPI_CACHE_RETENTION` (requirements §5.5); the resolution helper is
//!   shared with the anthropic-messages adapter
//!   ([`crate::api::anthropic_messages::resolve_cache_retention`]).
//! - `streamSimple`'s `toolChoice` smuggling (upstream reads `toolChoice` off
//!   the `SimpleStreamOptions` object) is not ported: rpi's
//!   `SimpleStreamOptions` struct has no such field. Tool choice is available
//!   via [`OpenAICompletionsOptions::tool_choice`] on direct [`stream`] calls.
//! - OpenRouter's `error.metadata.raw` detail is extracted from the HTTP error
//!   body JSON (upstream reads it off the SDK error object).

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::api::anthropic_messages::resolve_cache_retention;
use crate::api::constrained_sampling::{
    GrammarToolInputJsonBuffer, append_grammar_tool_input_json_delta,
    create_grammar_tool_input_properties, get_grammar_tool_input, get_json_schema_tool_parameters,
    resolve_grammar_constrained_sampling, resolve_json_schema_strict_sampling,
};
use crate::api::copilot_headers::{build_copilot_dynamic_headers, has_copilot_vision_input};
use crate::api::openai_prompt_cache::clamp_openai_prompt_cache_key;
use crate::api::simple_options::{MIN_ANSWER_TOKENS, build_base_options, resolve_sampling_params};
use crate::api::sse::{ServerSentEvent, SseDecoder};
use crate::api::stream_cancel::{StreamNext, next_chunk_or_cancelled};
use crate::models::{ProviderStreams, clamp_thinking_level};
use crate::types::{
    AssistantContent, AssistantMessage, CacheControlFormat, CacheRetention, ChatTemplateKwargValue,
    ChatTemplateKwargVarKind, DoneReason, ErrorReason, InputModality, MaxTokensField, Message,
    Model, ModelThinkingLevel, OpenRouterRouting, ProviderHeaders, ProviderResponse, Role,
    SessionAffinityFormat, SimpleStreamOptions, StopReason, StreamEvent, StreamOptions,
    TextContent, ThinkingBudgets, ThinkingContent, ThinkingFormat, Tool, ToolCall,
    ToolResultContent, TranscriptContext, Usage, UserContent, UserContentBlock,
    VercelGatewayRouting,
};
use crate::utils::cost::calculate_cost;
use crate::utils::custom_fetch::send_provider_request;
use crate::utils::error_body::{NormalizedProviderError, format_provider_error};
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::hash::short_hash;
use crate::utils::headers::{
    headers_to_record, merge_headers_chain, model_headers, provider_headers_to_header_map,
    rpi_user_agent_headers,
};
use crate::utils::json_parse::parse_streaming_json;
use crate::utils::provider_retry::{
    ProviderErrorInfo, ProviderRetryOptions, retry_provider_request,
};
use crate::utils::sanitize_unicode::sanitize_surrogates;
use crate::utils::transform_messages::transform_messages;

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// `OpenAICompletionsOptions` — `StreamOptions` plus completions-specific
/// extras. `reasoning_effort` keys directly into
/// `Model::thinking_level_map`; `tool_choice` is the raw OpenAI
/// `ChatCompletionToolChoiceOption` JSON value.
#[derive(Debug, Clone, Default)]
pub struct OpenAICompletionsOptions {
    pub stream: StreamOptions,
    pub reasoning_effort: Option<ModelThinkingLevel>,
    pub tool_choice: Option<Value>,
    /// Token budgets per thinking level. Only used when
    /// `compat.supports_thinking_token_budget` is set (d07889da0).
    pub thinking_budgets: Option<ThinkingBudgets>,
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

/// `getClientApiKey`: an explicit API key wins; an auth-bearing header makes
/// the key unnecessary ("unused" placeholder, matching upstream — the SDK
/// requires *some* key string). Otherwise the request cannot be authenticated.
pub fn get_client_api_key(
    provider: &str,
    api_key: Option<&str>,
    headers: Option<&ProviderHeaders>,
) -> Result<String, String> {
    if let Some(api_key) = api_key {
        return Ok(api_key.to_owned());
    }
    if has_header(headers, "authorization") || has_header(headers, "cf-aig-authorization") {
        return Ok("unused".to_owned());
    }
    Err(format!("No API key for provider: {provider}"))
}

// ---------------------------------------------------------------------------
// Tool history / deferred tools
// ---------------------------------------------------------------------------

/// `hasToolHistory`: true when the conversation contains tool calls or tool
/// results (Anthropic via proxy requires the `tools` param to be present then).
pub fn has_tool_history(messages: &[Message]) -> bool {
    messages.iter().any(|msg| match msg {
        Message::ToolResult(_) => true,
        Message::Assistant(assistant) => assistant
            .content
            .iter()
            .any(|block| matches!(block, AssistantContent::ToolCall(_))),
        Message::User(_) | Message::System(_) => false,
    })
}

/// `getToolsByName`: tools from `tools` matching `names`, in `names` order.
/// Upstream builds `new Map(tools.map((tool) => [tool.name, tool]))` — a
/// later duplicate name overwrites an earlier one.
pub fn get_tools_by_name(tools: Option<&[Tool]>, names: &[String]) -> Vec<Tool> {
    let Some(tools) = tools else {
        return Vec::new();
    };
    let mut by_name: HashMap<&str, &Tool> = HashMap::new();
    for tool in tools {
        by_name.insert(tool.name.as_str(), tool);
    }
    names
        .iter()
        .filter_map(|name| by_name.get(name.as_str()).map(|tool| (*tool).clone()))
        .collect()
}

// ---------------------------------------------------------------------------
// Compat detection and resolution
// ---------------------------------------------------------------------------

/// `Required<OpenAICompletionsCompat>` with `cacheControlFormat` kept
/// optional (upstream `ResolvedOpenAICompletionsCompat`; the old
/// `deferredToolsMode` optional left with #9548).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedOpenAICompletionsCompat {
    pub supports_store: bool,
    /// b23741269 (#8275): resolved budget field name (`field > bool alias`).
    pub thinking_token_budget_field: Option<crate::types::ThinkingTokenBudgetField>,
    /// 256f63024 (#9004): vLLM scheduler priority (top-level `priority`).
    pub vllm_priority: Option<f64>,
    pub supports_developer_role: bool,
    pub supports_reasoning_effort: bool,
    pub supports_usage_in_streaming: bool,
    /// 2c3041242: whether streamed responses include `finish_reason`. When
    /// false, `stop`/`toolUse` is inferred from content at stream end.
    pub supports_finish_reason: bool,
    pub max_tokens_field: MaxTokensField,
    pub requires_tool_result_name: bool,
    pub requires_assistant_after_tool_result: bool,
    pub requires_thinking_as_text: bool,
    pub requires_reasoning_content_on_assistant_messages: bool,
    pub thinking_format: ThinkingFormat,
    pub open_router_routing: OpenRouterRouting,
    pub vercel_gateway_routing: VercelGatewayRouting,
    pub chat_template_kwargs: std::collections::BTreeMap<String, ChatTemplateKwargValue>,
    /// d07889da0-era field (Baseten provider commit c1019d920): threaded
    /// through `detectCompat`/`getCompat`; the `baseten` thinking branch that
    /// consumes it is T26 scope.
    pub chat_template_args: std::collections::BTreeMap<String, ChatTemplateKwargValue>,
    pub zai_tool_stream: bool,
    /// d07889da0 (#7638): vLLM top-level `thinking_token_budget` opt-in.
    pub supports_thinking_token_budget: bool,
    pub supports_strict_mode: bool,
    pub supports_open_ai_grammar_tools: bool,
    pub cache_control_format: Option<CacheControlFormat>,
    pub send_session_affinity_headers: bool,
    /// #9548: later system messages sent in place.
    pub supports_mid_convo_system_messages: bool,
    /// #9548: tool additions can ride system messages (Kimi K3 family).
    /// Replaces the old `deferredToolsMode: "kimi"` catalog face.
    pub supports_mid_convo_tool_additions: bool,
    pub session_affinity_format: SessionAffinityFormat,
    pub supports_long_cache_retention: bool,
}

/// `detectCompat`: auto-detect compatibility settings from provider name and
/// baseUrl. Used as the base when `model.compat` is not set; explicit
/// `model.compat` entries override these detected values.
pub fn detect_compat(model: &Model) -> ResolvedOpenAICompletionsCompat {
    let provider = model.provider.as_str();
    let base_url = model.base_url.as_str();

    let is_zai = provider == "zai"
        || provider == "zai-coding-cn"
        || base_url.contains("api.z.ai")
        || base_url.contains("open.bigmodel.cn");
    let is_together = provider == "together"
        || base_url.contains("api.together.ai")
        || base_url.contains("api.together.xyz");
    let is_moonshot = provider == "moonshotai"
        || provider == "moonshotai-cn"
        || base_url.contains("api.moonshot.");
    let is_openrouter = crate::api::session_affinity::is_openrouter(provider, base_url);
    let is_cloudflare_workers_ai =
        provider == "cloudflare-workers-ai" || base_url.contains("api.cloudflare.com");
    let is_cloudflare_ai_gateway =
        provider == "cloudflare-ai-gateway" || base_url.contains("gateway.ai.cloudflare.com");
    let is_nvidia = provider == "nvidia" || base_url.contains("integrate.api.nvidia.com");
    let is_ant_ling = provider == "ant-ling" || base_url.contains("api.ant-ling.com");
    // #9804 (af7359b90): Cerebras rejects mixed strict/non-strict tools with
    // a 400, so it never declares strict support — since #9816 the runtime
    // default below is false for everyone and Cerebras/Moonshot/Together/
    // Cloudflare/NVIDIA keep their explicit `supportsStrictMode: false`
    // catalog metadata (baked by the generator's
    // detectOpenAICompletionsCompat).
    let is_cerebras = provider == "cerebras" || base_url.contains("cerebras.ai");

    let is_non_standard = is_nvidia
        || is_cerebras
        || provider == "xai"
        || base_url.contains("api.x.ai")
        || is_together
        || base_url.contains("chutes.ai")
        || base_url.to_lowercase().contains("deepseek.com")
        || is_zai
        || is_moonshot
        || provider == "opencode"
        || base_url.contains("opencode.ai")
        || is_cloudflare_workers_ai
        || is_cloudflare_ai_gateway
        || is_ant_ling;

    // c185d4123: DeepSeek endpoints take `max_tokens`, not
    // `max_completion_tokens` (declared before `use_max_tokens`, upstream
    // declaration order).
    let is_deepseek = provider == "deepseek" || base_url.to_lowercase().contains("deepseek.com");
    // 2fe21b407 (#7174) + c185d4123: Z.AI and DeepSeek join the `max_tokens`
    // list (both ignore `max_completion_tokens`).
    let use_max_tokens = base_url.contains("chutes.ai")
        || is_deepseek
        || is_moonshot
        || is_cloudflare_ai_gateway
        || is_together
        || is_nvidia
        || is_ant_ling
        || is_zai;

    let is_grok = provider == "xai" || base_url.contains("api.x.ai");
    let is_openrouter_developer_role_model =
        is_openrouter && (model.id.starts_with("anthropic/") || model.id.starts_with("openai/"));
    let cache_control_format = if provider == "openrouter" && model.id.starts_with("anthropic/") {
        Some(CacheControlFormat::Anthropic)
    } else {
        None
    };

    ResolvedOpenAICompletionsCompat {
        supports_store: !is_non_standard,
        supports_developer_role: is_openrouter_developer_role_model
            || (!is_non_standard && !is_openrouter),
        supports_reasoning_effort: !is_grok
            && !is_zai
            && !is_moonshot
            && !is_together
            && !is_cloudflare_ai_gateway
            && !is_nvidia
            && !is_ant_ling,
        supports_usage_in_streaming: true,
        supports_finish_reason: true,
        max_tokens_field: if use_max_tokens {
            MaxTokensField::MaxTokens
        } else {
            MaxTokensField::MaxCompletionTokens
        },
        requires_tool_result_name: false,
        requires_assistant_after_tool_result: false,
        requires_thinking_as_text: false,
        requires_reasoning_content_on_assistant_messages: is_deepseek,
        thinking_format: if is_deepseek {
            ThinkingFormat::Deepseek
        } else if is_zai {
            ThinkingFormat::Zai
        } else if is_together {
            ThinkingFormat::Together
        } else if is_ant_ling {
            ThinkingFormat::AntLing
        } else if is_openrouter {
            ThinkingFormat::Openrouter
        } else {
            ThinkingFormat::Openai
        },
        open_router_routing: OpenRouterRouting::default(),
        vercel_gateway_routing: VercelGatewayRouting::default(),
        chat_template_kwargs: Default::default(),
        chat_template_args: Default::default(),
        zai_tool_stream: false,
        supports_thinking_token_budget: false,
        thinking_token_budget_field: None,
        vllm_priority: None,
        // #9816 (890f92088, openai-completions.ts:1667-1668): OpenAI
        // compatibility alone does not imply strict JSON-schema tool
        // support — the runtime fallback defaults to non-strict. Known
        // capable models carry explicit `supportsStrictMode` catalog
        // metadata that overrides this via [`get_compat`].
        supports_strict_mode: false,
        supports_open_ai_grammar_tools: false,
        cache_control_format,
        // #9102 (bbb61e34a): OpenRouter endpoints derive session affinity
        // headers by default (`x-session-id`, see [`session_affinity`]).
        send_session_affinity_headers: is_openrouter,
        supports_mid_convo_system_messages: false,
        supports_mid_convo_tool_additions: false,
        session_affinity_format: if is_openrouter {
            SessionAffinityFormat::Openrouter
        } else {
            SessionAffinityFormat::Openai
        },
        supports_long_cache_retention: !(is_together
            || is_cloudflare_workers_ai
            || is_cloudflare_ai_gateway
            || is_nvidia
            || is_ant_ling),
    }
}

/// `getCompat`: auto-detect from provider/URL, then override with explicit
/// `model.compat` entries. Note `openRouterRouting` defaults to `{}` (not the
/// detected value), matching upstream.
pub fn get_compat(model: &Model) -> ResolvedOpenAICompletionsCompat {
    let detected = detect_compat(model);
    let Some(compat) = model.compat.as_ref() else {
        return detected;
    };
    ResolvedOpenAICompletionsCompat {
        supports_store: compat.supports_store.unwrap_or(detected.supports_store),
        supports_developer_role: compat
            .supports_developer_role
            .unwrap_or(detected.supports_developer_role),
        supports_reasoning_effort: compat
            .supports_reasoning_effort
            .unwrap_or(detected.supports_reasoning_effort),
        supports_usage_in_streaming: compat
            .supports_usage_in_streaming
            .unwrap_or(detected.supports_usage_in_streaming),
        supports_finish_reason: compat
            .supports_finish_reason
            .unwrap_or(detected.supports_finish_reason),
        max_tokens_field: compat.max_tokens_field.unwrap_or(detected.max_tokens_field),
        requires_tool_result_name: compat
            .requires_tool_result_name
            .unwrap_or(detected.requires_tool_result_name),
        requires_assistant_after_tool_result: compat
            .requires_assistant_after_tool_result
            .unwrap_or(detected.requires_assistant_after_tool_result),
        requires_thinking_as_text: compat
            .requires_thinking_as_text
            .unwrap_or(detected.requires_thinking_as_text),
        requires_reasoning_content_on_assistant_messages: compat
            .requires_reasoning_content_on_assistant_messages
            .unwrap_or(detected.requires_reasoning_content_on_assistant_messages),
        thinking_format: compat.thinking_format.unwrap_or(detected.thinking_format),
        open_router_routing: compat.open_router_routing.clone().unwrap_or_default(),
        vercel_gateway_routing: compat
            .vercel_gateway_routing
            .clone()
            .unwrap_or(detected.vercel_gateway_routing),
        chat_template_kwargs: compat
            .chat_template_kwargs
            .clone()
            .unwrap_or(detected.chat_template_kwargs),
        chat_template_args: compat
            .chat_template_args
            .clone()
            .unwrap_or(detected.chat_template_args),
        zai_tool_stream: compat.zai_tool_stream.unwrap_or(detected.zai_tool_stream),
        supports_thinking_token_budget: compat
            .supports_thinking_token_budget
            .unwrap_or(detected.supports_thinking_token_budget),
        thinking_token_budget_field: compat.thinking_token_budget_field,
        vllm_priority: compat.vllm_priority,
        supports_strict_mode: compat
            .supports_strict_mode
            .unwrap_or(detected.supports_strict_mode),
        supports_open_ai_grammar_tools: compat
            .supports_open_ai_grammar_tools
            .unwrap_or(detected.supports_open_ai_grammar_tools),
        cache_control_format: compat
            .cache_control_format
            .or(detected.cache_control_format),
        send_session_affinity_headers: compat
            .send_session_affinity_headers
            .unwrap_or(detected.send_session_affinity_headers),
        supports_mid_convo_system_messages: compat
            .supports_mid_convo_system_messages
            .unwrap_or(detected.supports_mid_convo_system_messages),
        supports_mid_convo_tool_additions: compat
            .supports_mid_convo_tool_additions
            .unwrap_or(detected.supports_mid_convo_tool_additions),
        session_affinity_format: compat
            .session_affinity_format
            .unwrap_or(detected.session_affinity_format),
        supports_long_cache_retention: compat
            .supports_long_cache_retention
            .unwrap_or(detected.supports_long_cache_retention),
    }
}

// ---------------------------------------------------------------------------
// Client headers
// ---------------------------------------------------------------------------

/// `createClient` (header construction only). The base carries the SDK's
/// `Accept: application/json` default and the key-derived `Authorization`
/// header; both sit in the base position because the SDK applies
/// `authHeaders` *before* `defaultHeaders` (user headers override key-derived
/// auth, case-insensitively — see `merge_headers_chain`).
pub fn build_client_headers(
    model: &Model,
    context: &TranscriptContext,
    api_key: &str,
    options_headers: Option<&ProviderHeaders>,
    session_id: Option<&str>,
    compat: &ResolvedOpenAICompletionsCompat,
) -> ProviderHeaders {
    let base: ProviderHeaders = [
        ("accept".to_owned(), Some("application/json".to_owned())),
        (
            "authorization".to_owned(),
            Some(format!("Bearer {api_key}")),
        ),
    ]
    .into();

    let copilot_headers: Option<ProviderHeaders> =
        (model.provider == "github-copilot").then(|| {
            let has_images = has_copilot_vision_input(&context.messages);
            build_copilot_dynamic_headers(&context.messages, has_images)
                .into_iter()
                .map(|(key, value)| (key, Some(value)))
                .collect()
        });

    let session_affinity_headers: Option<ProviderHeaders> = session_id
        .filter(|_| compat.send_session_affinity_headers)
        .map(|session_id| {
            let mut headers = ProviderHeaders::new();
            match compat.session_affinity_format {
                SessionAffinityFormat::Openrouter => {
                    headers.insert("x-session-id".to_owned(), Some(session_id.to_owned()));
                }
                SessionAffinityFormat::Openai => {
                    headers.insert("session_id".to_owned(), Some(session_id.to_owned()));
                    headers.insert(
                        "x-client-request-id".to_owned(),
                        Some(session_id.to_owned()),
                    );
                    headers.insert("x-session-affinity".to_owned(), Some(session_id.to_owned()));
                }
                SessionAffinityFormat::OpenaiNosession => {
                    headers.insert(
                        "x-client-request-id".to_owned(),
                        Some(session_id.to_owned()),
                    );
                    headers.insert("x-session-affinity".to_owned(), Some(session_id.to_owned()));
                }
            }
            headers
        });

    merge_headers_chain(&[
        rpi_user_agent_headers(),
        Some(base),
        model_headers(model),
        copilot_headers,
        session_affinity_headers,
        options_headers.cloned(),
    ])
}

// ---------------------------------------------------------------------------
// Message conversion
// ---------------------------------------------------------------------------

/// `convertMessages`. Tool-call ids are normalized through
/// [`transform_messages`] with the completions-specific rules: pipe-separated
/// Responses-API ids collapse to `callId_itemId` (sanitized, 40-char cap with
/// an 8-char `shortHash` back-fill), and plain ids truncate to 40 chars for
/// provider `openai` only.
pub fn convert_messages(
    model: &Model,
    context: &TranscriptContext,
    compat: &ResolvedOpenAICompletionsCompat,
    grammar_tool_input_properties: &HashMap<String, String>,
) -> Result<Vec<Value>, String> {
    // #9548 (openai-completions.ts:1191): resolve the mid-conversation
    // policy here as well — callers may pass an already-resolved transcript
    // (idempotent for true, collapse-in-place for false).
    let normalized_context = crate::utils::transcript::resolve_transcript(
        context,
        compat.supports_mid_convo_system_messages,
    );
    let context: &TranscriptContext = &normalized_context;
    let mut params: Vec<Value> = Vec::new();

    let provider = model.provider.clone();
    let mut normalize_tool_call_id = move |id: &str, _model: &Model, _msg: &AssistantMessage| {
        // Pipe-separated IDs from the OpenAI Responses API:
        // `{call_id}|{item_id}` where item_id can be 400+ chars with special
        // chars. Multiple tool calls in the same turn can share call_id but
        // differ by item_id; preserve item-level uniqueness.
        if let Some(separator_index) = id.find('|') {
            // Sanitize to allowed chars and truncate to 40 chars (OpenAI limit).
            // Post-sanitize text is ASCII, so char-based slicing is exact.
            let sanitize = |text: &str| -> String {
                text.chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect()
            };
            let call_id = sanitize(&id[..separator_index]);
            let item_id = sanitize(&id[separator_index + 1..]);
            let combined_id = if item_id.is_empty() {
                call_id.clone()
            } else {
                format!("{call_id}_{item_id}")
            };
            if combined_id.chars().count() <= 40 {
                return combined_id;
            }
            let hash: String = short_hash(id).chars().take(8).collect();
            let prefix: String = call_id.chars().take(40 - hash.len() - 1).collect();
            let prefix: String = if prefix.is_empty() {
                // JS `callId.slice(0, Math.max(1, 31))` keeps at least one char.
                call_id.chars().take(1).collect()
            } else {
                prefix
            };
            return format!("{prefix}_{hash}");
        }

        if provider == "openai" {
            return id.chars().take(40).collect();
        }
        id.to_owned()
    };
    let transformed_messages =
        transform_messages(&context.messages, model, Some(&mut normalize_tool_call_id));

    // #9548: tools split between the request field and in-place additions
    // (Kimi-style `tools` field on a system message).
    let transcript_tools = crate::utils::transcript::resolve_transcript_tools(
        &context.messages,
        compat.supports_mid_convo_system_messages && compat.supports_mid_convo_tool_additions,
    );
    let instruction_role = if model.reasoning && compat.supports_developer_role {
        "developer"
    } else {
        "system"
    };

    let mut last_role: Option<Role> = None;
    let mut i = 0;
    while i < transformed_messages.len() {
        let msg = &transformed_messages[i];
        // Some providers don't allow user messages directly after tool results;
        // insert a synthetic assistant message to bridge the gap.
        if compat.requires_assistant_after_tool_result
            && last_role == Some(Role::ToolResult)
            && matches!(msg, Message::User(_))
        {
            params.push(json!({
                "role": "assistant",
                "content": "I have processed the tool results.",
            }));
        }

        match msg {
            Message::System(system) => {
                // #9548 (openai-completions.ts:1240-1255): later system
                // messages carry their in-place tool additions (Kimi), the
                // leading one renders the full prompt text.
                if i > 0 && transcript_tools.anchors_additions {
                    let added: Vec<Tool> = system.tools_added.clone().unwrap_or_default();
                    if !added.is_empty() {
                        // Kimi accepts a system message with tools but omits
                        // the standard content field.
                        params.push(json!({
                            "role": "system",
                            "tools": convert_tools(&added, compat)?,
                        }));
                    }
                }
                let text = if i == 0 {
                    crate::utils::text::get_system_message_text(system)
                } else {
                    crate::utils::text::render_system_message_update(system)
                };
                if !text.is_empty() {
                    params.push(json!({
                        "role": instruction_role,
                        "content": sanitize_surrogates(&text),
                    }));
                }
            }
            Message::User(user) => match &user.content {
                UserContent::Text(text) => {
                    params.push(json!({
                        "role": "user",
                        "content": sanitize_surrogates(text),
                    }));
                }
                UserContent::Blocks(blocks) => {
                    // #9797 (1b6ddca87, openai-completions.ts:1261): omit
                    // empty text parts from multimodal user messages — some
                    // OpenAI-compatible providers reject image-only prompts
                    // that carry an empty text segment. Whitespace-only
                    // parts are kept (upstream filters `length > 0` only).
                    let content: Vec<Value> = blocks
                        .iter()
                        .filter(|item| match item {
                            UserContentBlock::Text(text) => !text.text.is_empty(),
                            UserContentBlock::Image(_) => true,
                        })
                        .map(|item| match item {
                            UserContentBlock::Text(text) => json!({
                                "type": "text",
                                "text": sanitize_surrogates(&text.text),
                            }),
                            UserContentBlock::Image(image) => json!({
                                "type": "image_url",
                                "image_url": {
                                    "url": format!("data:{};base64,{}", image.mime_type, image.data),
                                },
                            }),
                        })
                        .collect();
                    if content.is_empty() {
                        i += 1;
                        continue;
                    }
                    params.push(json!({"role": "user", "content": content}));
                }
            },
            Message::Assistant(assistant) => {
                let mut assistant_msg = Map::new();
                assistant_msg.insert("role".to_owned(), json!("assistant"));
                // Some providers don't accept null content, use empty string instead.
                assistant_msg.insert(
                    "content".to_owned(),
                    if compat.requires_assistant_after_tool_result {
                        json!("")
                    } else {
                        Value::Null
                    },
                );

                let text_parts: Vec<Value> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContent::Text(text) if !text.text.trim().is_empty() => {
                            Some(json!({
                                "type": "text",
                                "text": sanitize_surrogates(&text.text),
                            }))
                        }
                        _ => None,
                    })
                    .collect();
                let assistant_text = text_parts
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("");

                let thinking_blocks: Vec<&ThinkingContent> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContent::Thinking(thinking)
                            if !thinking.thinking.trim().is_empty() =>
                        {
                            Some(thinking)
                        }
                        _ => None,
                    })
                    .collect();

                // b7bb00b93 (#7994): replay reasoning details verbatim and
                // in order — the signed thinking-signature array wins; the
                // pre-b7bb00b93 storage (encrypted details on tool-call
                // `thought_signature`s) is the fallback for old sessions.
                let all_thinking_blocks: Vec<&ThinkingContent> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContent::Thinking(thinking) => Some(thinking),
                        _ => None,
                    })
                    .collect();
                let tool_call_blocks: Vec<&ToolCall> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContent::ToolCall(call) => Some(call),
                        _ => None,
                    })
                    .collect();
                let signed_reasoning_details = all_thinking_blocks.iter().find_map(|block| {
                    parse_openai_reasoning_details(block.thinking_signature.as_deref())
                });
                let legacy_reasoning_details: Vec<Value> = tool_call_blocks
                    .iter()
                    .filter_map(|call| {
                        parse_legacy_encrypted_reasoning_detail(call.thought_signature.as_deref())
                    })
                    .collect();
                let preserved_reasoning_details = signed_reasoning_details.or_else(|| {
                    (!legacy_reasoning_details.is_empty()).then_some(legacy_reasoning_details)
                });

                if !thinking_blocks.is_empty() {
                    if compat.requires_thinking_as_text {
                        // Convert thinking blocks to plain text (no tags to avoid
                        // model mimicking them).
                        let thinking_text = thinking_blocks
                            .iter()
                            .map(|block| sanitize_surrogates(&block.thinking))
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        let mut content = vec![json!({"type": "text", "text": thinking_text})];
                        content.extend(text_parts);
                        assistant_msg.insert("content".to_owned(), Value::Array(content));
                    } else {
                        // Always send assistant content as a plain string (OpenAI
                        // Chat Completions API standard format); an array of text
                        // parts makes some models mirror the block structure in
                        // their output.
                        if !assistant_text.is_empty() {
                            assistant_msg.insert("content".to_owned(), json!(assistant_text));
                        }

                        // Use the signature from the first thinking block if
                        // available (for llama.cpp server + gpt-oss) — but
                        // never when the signature holds preserved reasoning
                        // details (:1310).
                        if preserved_reasoning_details.is_none() {
                            let mut signature = thinking_blocks[0].thinking_signature.clone();
                            if model.provider == "opencode-go"
                                && signature.as_deref() == Some("reasoning")
                            {
                                signature = Some("reasoning_content".to_owned());
                            }
                            if let Some(signature) = signature.filter(|s| !s.is_empty()) {
                                // Raw thinking text (not sanitized), joined with "\n".
                                let joined = thinking_blocks
                                    .iter()
                                    .map(|block| block.thinking.as_str())
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                assistant_msg.insert(signature, json!(joined));
                            }
                        }
                    }
                } else if !assistant_text.is_empty() {
                    assistant_msg.insert("content".to_owned(), json!(assistant_text));
                }

                let tool_calls: Vec<&ToolCall> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContent::ToolCall(call) => Some(call),
                        _ => None,
                    })
                    .collect();
                if !tool_calls.is_empty() {
                    let mut converted: Vec<Value> = Vec::with_capacity(tool_calls.len());
                    for tool_call in &tool_calls {
                        if let Some(property) = grammar_tool_input_properties.get(&tool_call.name) {
                            let input = get_grammar_tool_input(
                                &tool_call.name,
                                &tool_call.arguments,
                                property,
                            )?;
                            converted.push(json!({
                                "id": tool_call.id,
                                "type": "custom",
                                "custom": {
                                    "name": tool_call.name,
                                    "input": sanitize_surrogates(&input),
                                },
                            }));
                        } else {
                            converted.push(json!({
                                "id": tool_call.id,
                                "type": "function",
                                "function": {
                                    "name": tool_call.name,
                                    "arguments": serde_json::to_string(&tool_call.arguments)
                                        .unwrap_or_else(|_| "{}".to_owned()),
                                },
                            }));
                        }
                    }
                    assistant_msg.insert("tool_calls".to_owned(), Value::Array(converted));
                }
                // `assistantMsg.reasoning_details = preservedReasoningDetails`
                // (:1353-1354) — verbatim, in order.
                if let Some(reasoning_details) = preserved_reasoning_details {
                    assistant_msg.insert(
                        "reasoning_details".to_owned(),
                        Value::Array(reasoning_details),
                    );
                }

                if compat.requires_reasoning_content_on_assistant_messages
                    && model.reasoning
                    && !assistant_msg.contains_key("reasoning_content")
                {
                    assistant_msg.insert("reasoning_content".to_owned(), json!(""));
                }

                // Skip assistant messages that have no content and no tool calls.
                // Some providers require "either content or tool_calls, but not
                // none"; others don't accept empty assistant messages. This
                // handles aborted assistant responses that got no content.
                let has_content = match assistant_msg.get("content") {
                    Some(Value::String(text)) => !text.is_empty(),
                    Some(Value::Array(parts)) => !parts.is_empty(),
                    _ => false,
                };
                if !has_content && !assistant_msg.contains_key("tool_calls") {
                    i += 1;
                    continue;
                }
                params.push(Value::Object(assistant_msg));
            }
            Message::ToolResult(_) => {
                let mut image_blocks: Vec<Value> = Vec::new();
                let mut j = i;

                while j < transformed_messages.len() {
                    let Message::ToolResult(tool_msg) = &transformed_messages[j] else {
                        break;
                    };

                    let text_result = tool_msg
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            ToolResultContent::Text(text) => Some(text.text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    let has_images = tool_msg
                        .content
                        .iter()
                        .any(|block| matches!(block, ToolResultContent::Image(_)));

                    // Always send tool result with text (or placeholder if only images).
                    let tool_result_text = if !text_result.is_empty() {
                        text_result
                    } else if has_images {
                        "(see attached image)".to_owned()
                    } else {
                        "(no tool output)".to_owned()
                    };
                    let mut tool_result_msg = json!({
                        "role": "tool",
                        "content": sanitize_surrogates(&tool_result_text),
                        "tool_call_id": tool_msg.tool_call_id,
                    });
                    // Some providers require the 'name' field in tool results.
                    if compat.requires_tool_result_name && !tool_msg.tool_name.is_empty() {
                        tool_result_msg["name"] = json!(tool_msg.tool_name);
                    }
                    params.push(tool_result_msg);

                    if has_images && model.input.contains(&InputModality::Image) {
                        for block in &tool_msg.content {
                            if let ToolResultContent::Image(image) = block {
                                image_blocks.push(json!({
                                    "type": "image_url",
                                    "image_url": {
                                        "url": format!("data:{};base64,{}", image.mime_type, image.data),
                                    },
                                }));
                            }
                        }
                    }
                    j += 1;
                }

                if !image_blocks.is_empty() {
                    if compat.requires_assistant_after_tool_result {
                        params.push(json!({
                            "role": "assistant",
                            "content": "I have processed the tool results.",
                        }));
                    }
                    let mut content = vec![
                        json!({"type": "text", "text": "Attached image(s) from tool result:"}),
                    ];
                    content.extend(image_blocks);
                    params.push(json!({"role": "user", "content": content}));
                    last_role = Some(Role::User);
                } else {
                    last_role = Some(Role::ToolResult);
                }

                i = j;
                continue;
            }
        }

        last_role = Some(msg.role());
        i += 1;
    }

    Ok(params)
}

/// `convertTools`: grammar-constrained tools become OpenAI `custom` tools;
/// everything else becomes a `function` tool (`strict` included only when the
/// provider supports it — some reject unknown fields).
pub fn convert_tools(
    tools: &[Tool],
    compat: &ResolvedOpenAICompletionsCompat,
) -> Result<Vec<Value>, String> {
    tools
        .iter()
        .map(|tool| {
            if let Some(grammar) =
                resolve_grammar_constrained_sampling(tool, compat.supports_open_ai_grammar_tools)?
            {
                return Ok(json!({
                    "type": "custom",
                    "custom": {
                        "name": tool.name,
                        "description": tool.description,
                        "format": {
                            "type": "grammar",
                            "grammar": {
                                "syntax": grammar.format.as_str(),
                                "definition": grammar.definition,
                            },
                        },
                    },
                }));
            }

            let strict = resolve_json_schema_strict_sampling(tool, compat.supports_strict_mode)?;
            let mut function = Map::new();
            function.insert("name".to_owned(), json!(tool.name));
            function.insert("description".to_owned(), json!(tool.description));
            // TypeBox already generates JSON Schema upstream; `parameters` is a
            // schema value here — strict tools send the converted subset.
            function.insert(
                "parameters".to_owned(),
                get_json_schema_tool_parameters(tool, strict)?,
            );
            if compat.supports_strict_mode {
                function.insert("strict".to_owned(), json!(strict.unwrap_or(false)));
            }
            Ok(json!({"type": "function", "function": Value::Object(function)}))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Chat template kwargs (thinkingFormat "chat-template")
// ---------------------------------------------------------------------------

/// `buildChatTemplateValues` (upstream `openai-completions.ts:892`): resolves
/// each configured kwarg/arg; `None` when no entry survives resolution.
/// `resolveThinkingTokenBudgetField` (b23741269/#8275): explicit field
/// wins; the legacy bool compat is the `thinking_token_budget` alias.
fn thinking_token_budget_field(
    compat: &ResolvedOpenAICompletionsCompat,
) -> Option<crate::types::ThinkingTokenBudgetField> {
    use crate::types::ThinkingTokenBudgetField;
    compat.thinking_token_budget_field.or({
        compat
            .supports_thinking_token_budget
            .then_some(ThinkingTokenBudgetField::ThinkingTokenBudget)
    })
}

/// `resolveClampedThinkingBudget` (openai-completions.ts:1004-1017):
/// `thinkingBudgetForLevel` clamped to leave `MIN_ANSWER_TOKENS` of answer
/// room under the response ceiling; `None` when off / not reasoning /
/// clamped to zero.
fn resolve_clamped_thinking_budget(
    model: &Model,
    options: &OpenAICompletionsOptions,
    params: &Value,
    _field: Option<crate::types::ThinkingTokenBudgetField>,
) -> Option<u64> {
    let effort = options.reasoning_effort?;
    if !model.reasoning {
        return None;
    }
    // `clampReasoning`: xhigh/max use the high budget.
    let level = match effort {
        ModelThinkingLevel::Xhigh | ModelThinkingLevel::Max => ModelThinkingLevel::High,
        other => other,
    };
    let defaults = ThinkingBudgets {
        minimal: Some(1024),
        low: Some(2048),
        medium: Some(8192),
        high: Some(16384),
    };
    let budget_for = |budgets: &ThinkingBudgets| match level {
        ModelThinkingLevel::Minimal => budgets.minimal,
        ModelThinkingLevel::Low => budgets.low,
        ModelThinkingLevel::Medium => budgets.medium,
        ModelThinkingLevel::High => budgets.high,
        // Off is not a valid upstream `reasoningEffort`; xhigh/max were
        // clamped above.
        _ => None,
    };
    let level_budget = options
        .thinking_budgets
        .as_ref()
        .and_then(budget_for)
        .or_else(|| budget_for(&defaults))?;
    let ceiling = params
        .get("max_tokens")
        .or_else(|| params.get("max_completion_tokens"))
        .and_then(json_u64)
        .unwrap_or(u64::from(model.max_tokens));
    // Always leave room for the answer, otherwise the budget recreates the
    // bug it prevents (`clampThinkingBudgetToAnswerRoom`).
    let budget = u64::from(level_budget).min(ceiling.saturating_sub(u64::from(MIN_ANSWER_TOKENS)));
    (budget > 0).then_some(budget)
}

/// Parameterized over the source map so both `chat_template_kwargs` and
/// `chat_template_args` share one implementation (Baseten uses the latter,
/// `openai-completions.ts:784` @ `c1019d920`).
fn build_chat_template_values(
    model: &Model,
    reasoning_effort: Option<ModelThinkingLevel>,
    values: &std::collections::BTreeMap<String, ChatTemplateKwargValue>,
    thinking_budget: Option<u64>,
) -> Option<Value> {
    let mut kwargs = Map::new();
    for (key, value) in values {
        if let Some(resolved) =
            resolve_chat_template_kwarg_value(model, reasoning_effort, value, thinking_budget)
        {
            kwargs.insert(key.clone(), resolved);
        }
    }
    if kwargs.is_empty() {
        None
    } else {
        Some(Value::Object(kwargs))
    }
}

/// `chat_template_kwargs` resolution (chat-template thinkingFormat).
fn build_chat_template_kwargs(
    model: &Model,
    reasoning_effort: Option<ModelThinkingLevel>,
    compat: &ResolvedOpenAICompletionsCompat,
    thinking_budget: Option<u64>,
) -> Option<Value> {
    build_chat_template_values(
        model,
        reasoning_effort,
        &compat.chat_template_kwargs,
        thinking_budget,
    )
}

/// `chat_template_args` resolution (baseten thinkingFormat).
fn build_chat_template_args(
    model: &Model,
    reasoning_effort: Option<ModelThinkingLevel>,
    compat: &ResolvedOpenAICompletionsCompat,
    thinking_budget: Option<u64>,
) -> Option<Value> {
    build_chat_template_values(
        model,
        reasoning_effort,
        &compat.chat_template_args,
        thinking_budget,
    )
}

/// `resolveChatTemplateKwargValue`: scalars pass through; `$var` refs resolve
/// against the current reasoning effort and the model's thinking level map.
fn resolve_chat_template_kwarg_value(
    model: &Model,
    reasoning_effort: Option<ModelThinkingLevel>,
    value: &ChatTemplateKwargValue,
    thinking_budget: Option<u64>,
) -> Option<Value> {
    let scalar = match value {
        ChatTemplateKwargValue::Scalar(value) => return Some(value.clone()),
        ChatTemplateKwargValue::Var(var) => var,
    };

    if reasoning_effort.is_none() && scalar.omit_when_off == Some(true) {
        return None;
    }
    if scalar.var == ChatTemplateKwargVarKind::ThinkingEnabled {
        return Some(json!(reasoning_effort.is_some()));
    }
    // b23741269 (#8275): the clamped thinking budget — the same value the
    // top-level budget field carries.
    if scalar.var == ChatTemplateKwargVarKind::ThinkingBudget {
        return thinking_budget.map(|budget| json!(budget));
    }

    // thinking.effort: mapped value for the effort (or `off` when no effort).
    let mapped = match reasoning_effort {
        Some(effort) => model
            .thinking_level_map
            .as_ref()
            .and_then(|map| map.get(&effort))
            .cloned(),
        None => model
            .thinking_level_map
            .as_ref()
            .and_then(|map| map.get(&ModelThinkingLevel::Off))
            .cloned(),
    };
    // `mappedValue === undefined ? reasoningEffort : typeof mappedValue === "string" ? mappedValue : undefined`
    match mapped {
        None => reasoning_effort.map(|effort| json!(effort.as_str())),
        Some(Some(value)) => Some(json!(value)),
        Some(None) => None,
    }
}

// ---------------------------------------------------------------------------
// Anthropic-style cache control markers
// ---------------------------------------------------------------------------

/// `getCompatCacheControl`: `{"type":"ephemeral"}` (plus `"ttl":"1h"` for long
/// retention on supporting models) when the compat opts into Anthropic-style
/// cache control and retention is not "none".
fn get_compat_cache_control(
    compat: &ResolvedOpenAICompletionsCompat,
    cache_retention: CacheRetention,
) -> Option<Value> {
    if compat.cache_control_format != Some(CacheControlFormat::Anthropic)
        || cache_retention == CacheRetention::None
    {
        return None;
    }
    let long = cache_retention == CacheRetention::Long && compat.supports_long_cache_retention;
    Some(if long {
        json!({"type": "ephemeral", "ttl": "1h"})
    } else {
        json!({"type": "ephemeral"})
    })
}

/// `applyAnthropicCacheControl`: marks the first system/developer message,
/// the last tool definition, and the last user/assistant/tool text content.
fn apply_anthropic_cache_control(
    messages: &mut [Value],
    tools: Option<&mut Vec<Value>>,
    cache_control: &Value,
) {
    // `addCacheControlToSystemPrompt`: the first instruction message consumes
    // the attempt even when it cannot carry the marker (upstream `return`).
    for message in messages.iter_mut() {
        let role = message.get("role").and_then(Value::as_str);
        if role == Some("system") || role == Some("developer") {
            add_cache_control_to_text_content(message, cache_control);
            break;
        }
    }
    if let Some(tools) = tools
        && let Some(last_tool) = tools.last_mut()
    {
        last_tool["cache_control"] = cache_control.clone();
    }
    for message in messages.iter_mut().rev() {
        let role = message.get("role").and_then(Value::as_str);
        if matches!(role, Some("user") | Some("assistant") | Some("tool"))
            && add_cache_control_to_text_content(message, cache_control)
        {
            break;
        }
    }
}

/// `addCacheControlToTextContent`: string content converts to a marked text
/// part; array content marks its last text part. Returns false when nothing
/// could be marked (empty/missing content or no text part).
fn add_cache_control_to_text_content(message: &mut Value, cache_control: &Value) -> bool {
    let Some(content) = message.get_mut("content") else {
        return false;
    };
    match content {
        Value::String(text) => {
            if text.is_empty() {
                return false;
            }
            let text = std::mem::take(text);
            *content = json!([{
                "type": "text",
                "text": text,
                "cache_control": cache_control,
            }]);
            true
        }
        Value::Array(parts) => {
            for part in parts.iter_mut().rev() {
                if part.get("type").and_then(Value::as_str) == Some("text") {
                    part["cache_control"] = cache_control.clone();
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Thinking level map lookup semantics
// ---------------------------------------------------------------------------

/// `model.thinkingLevelMap?.[level] ?? level` — JS `??` semantics: key absent
/// *or* explicitly null falls back to the level name.
pub(crate) fn mapped_or_level_name(model: &Model, level: ModelThinkingLevel) -> String {
    model
        .thinking_level_map
        .as_ref()
        .and_then(|map| map.get(&level))
        .and_then(|mapped| mapped.clone())
        .unwrap_or_else(|| level.as_str().to_owned())
}

/// zai's `mapped === undefined ? level : mapped` — key absent falls back to
/// the level name; explicit null stays null (the param is omitted).
fn mapped_zai(model: &Model, level: ModelThinkingLevel) -> Option<String> {
    match model
        .thinking_level_map
        .as_ref()
        .and_then(|map| map.get(&level))
    {
        None => Some(level.as_str().to_owned()),
        Some(mapped) => mapped.clone(),
    }
}

/// `model.thinkingLevelMap?.off` — `None` = key absent, `Some(None)` = JSON
/// null, `Some(Some(v))` = mapped string.
pub(crate) fn off_value(model: &Model) -> Option<Option<String>> {
    model
        .thinking_level_map
        .as_ref()
        .and_then(|map| map.get(&ModelThinkingLevel::Off))
        .cloned()
}

/// `off !== null` guard: true when the key is absent or holds a string.
pub(crate) fn off_is_not_null(model: &Model) -> bool {
    off_value(model) != Some(None)
}

// ---------------------------------------------------------------------------
// buildParams
// ---------------------------------------------------------------------------

/// `buildParams`. Key insertion order mirrors the upstream object literal plus
/// conditional assignments (serde_json `preserve_order` keeps it), so payload
/// snapshots compare byte-for-byte with the JS payloads.
pub fn build_params(
    model: &Model,
    context: &TranscriptContext,
    options: &OpenAICompletionsOptions,
    compat: &ResolvedOpenAICompletionsCompat,
    cache_retention: CacheRetention,
    grammar_tool_input_properties: &HashMap<String, String>,
) -> Result<Value, String> {
    let mut messages = convert_messages(model, context, compat, grammar_tool_input_properties)?;
    let cache_control = get_compat_cache_control(compat, cache_retention);

    // #9548 (openai-completions.ts:807-811): request-level tools split from
    // in-place additions.
    let transcript_tools = crate::utils::transcript::resolve_transcript_tools(
        &context.messages,
        compat.supports_mid_convo_system_messages && compat.supports_mid_convo_tool_additions,
    );
    let request_tools = transcript_tools.request_tools;
    let mut tools: Option<Vec<Value>> = if !request_tools.is_empty() {
        Some(convert_tools(&request_tools, compat)?)
    } else if has_tool_history(&context.messages) {
        // Anthropic (via LiteLLM/proxy) requires tools param when the
        // conversation has tool_calls/tool_results.
        Some(Vec::new())
    } else {
        None
    };

    if let Some(cache_control) = &cache_control {
        apply_anthropic_cache_control(&mut messages, tools.as_mut(), cache_control);
    }

    let mut params = Map::new();
    params.insert("model".to_owned(), json!(model.id));
    params.insert("messages".to_owned(), Value::Array(messages));
    params.insert("stream".to_owned(), json!(true));

    let prompt_cache_key = if (model.base_url.contains("api.openai.com")
        && cache_retention != CacheRetention::None)
        || (cache_retention == CacheRetention::Long && compat.supports_long_cache_retention)
    {
        clamp_openai_prompt_cache_key(options.stream.session_id.as_deref())
    } else {
        None
    };
    if let Some(key) = prompt_cache_key {
        params.insert("prompt_cache_key".to_owned(), json!(key));
    }
    if cache_retention == CacheRetention::Long && compat.supports_long_cache_retention {
        params.insert("prompt_cache_retention".to_owned(), json!("24h"));
    }
    if compat.supports_usage_in_streaming {
        params.insert("stream_options".to_owned(), json!({"include_usage": true}));
    }
    if compat.supports_store {
        params.insert("store".to_owned(), json!(false));
    }
    if let Some(max_tokens) = options.stream.max_tokens {
        match compat.max_tokens_field {
            MaxTokensField::MaxTokens => {
                params.insert("max_tokens".to_owned(), json!(max_tokens));
            }
            MaxTokensField::MaxCompletionTokens => {
                params.insert("max_completion_tokens".to_owned(), json!(max_tokens));
            }
        }
    }
    if let Some(temperature) = options.stream.temperature {
        params.insert("temperature".to_owned(), json!(temperature));
    }
    if let Some(tools) = tools {
        let has_tools = !tools.is_empty();
        params.insert("tools".to_owned(), Value::Array(tools));
        if has_tools && compat.zai_tool_stream {
            params.insert("tool_stream".to_owned(), json!(true));
        }
    }
    if let Some(tool_choice) = &options.tool_choice {
        params.insert("tool_choice".to_owned(), tool_choice.clone());
    }

    // 256f63024 (#9004): vLLM scheduler priority.
    if let Some(priority) = compat.vllm_priority {
        params.insert("priority".to_owned(), json!(priority));
    }

    // d07889da0 (#7638) + b23741269 (#8275): cap reasoning with a top-level
    // budget field (`thinking_token_budget` / `thinking_budget` /
    // `thinking_budget_tokens`; the bool compat is the first one's alias).
    // Independent of thinkingFormat: the same server can serve zai, qwen or
    // chat-template models. Reasoning and the answer share max_tokens here,
    // so an uncapped reasoning phase can consume the whole response and
    // leave no answer and no tool call. The same clamped value feeds the
    // `{ "$var": "thinking.budget" }` chat-template kwarg.
    let clamped_thinking_budget = resolve_clamped_thinking_budget(
        model,
        options,
        &Value::Object(params.clone()),
        thinking_token_budget_field(compat),
    );
    if let (Some(field), Some(budget)) =
        (thinking_token_budget_field(compat), clamped_thinking_budget)
    {
        params.insert(field.as_str().to_owned(), json!(budget));
    }

    apply_thinking_params(
        &mut params,
        model,
        options.reasoning_effort,
        compat,
        clamped_thinking_budget,
    );

    // OpenRouter provider routing preferences (read from model.compat
    // directly, like upstream — not from the resolved compat).
    if let Some(routing) = model
        .compat
        .as_ref()
        .and_then(|compat| compat.open_router_routing.as_ref())
    {
        params.insert("provider".to_owned(), json!(routing));
    }

    // Vercel AI Gateway provider routing preferences.
    if let Some(routing) = model
        .compat
        .as_ref()
        .and_then(|compat| compat.vercel_gateway_routing.as_ref())
        && (routing.only.is_some() || routing.order.is_some())
    {
        let mut gateway = Map::new();
        if let Some(only) = &routing.only {
            gateway.insert("only".to_owned(), json!(only));
        }
        if let Some(order) = &routing.order {
            gateway.insert("order".to_owned(), json!(order));
        }
        params.insert(
            "providerOptions".to_owned(),
            json!({"gateway": Value::Object(gateway)}),
        );
    }

    // 25a2c8dcf (#7568): merged last so custom keys override the named
    // request fields.
    // Last so custom keys override the named request fields. The effective
    // thinking level's `samplingParamsByThinkingLevel` sits between the model
    // defaults and the per-request keys (#9506 `c01f687e5`, #9776).
    if let Some(sampling) = resolve_sampling_params(
        model,
        options.reasoning_effort.unwrap_or(ModelThinkingLevel::Off),
        options.stream.sampling_params.as_ref(),
    ) {
        for (key, value) in &sampling {
            params.insert(key.clone(), value.clone());
        }
    }

    Ok(Value::Object(params))
}

/// The ten `thinkingFormat` branches plus the OpenAI-style fallbacks, ported
/// as a single else-if chain (order matters).
fn apply_thinking_params(
    params: &mut Map<String, Value>,
    model: &Model,
    reasoning_effort: Option<ModelThinkingLevel>,
    compat: &ResolvedOpenAICompletionsCompat,
    thinking_budget: Option<u64>,
) {
    if compat.thinking_format == ThinkingFormat::Zai && model.reasoning {
        params.insert(
            "thinking".to_owned(),
            if reasoning_effort.is_some() {
                json!({"type": "enabled", "clear_thinking": false})
            } else {
                json!({"type": "disabled"})
            },
        );
        if let Some(effort) = reasoning_effort
            && compat.supports_reasoning_effort
            && let Some(value) = mapped_zai(model, effort)
        {
            params.insert("reasoning_effort".to_owned(), json!(value));
        }
    } else if compat.thinking_format == ThinkingFormat::Qwen && model.reasoning {
        params.insert(
            "enable_thinking".to_owned(),
            json!(reasoning_effort.is_some()),
        );
        // 4c1a0b92e (#6998/#6951): qwen thinking models also take
        // `reasoning_effort` (mapped through thinkingLevelMap, JS `??`
        // semantics) when the provider supports it.
        if let Some(effort) = reasoning_effort
            && compat.supports_reasoning_effort
        {
            params.insert(
                "reasoning_effort".to_owned(),
                json!(mapped_or_level_name(model, effort)),
            );
        }
    } else if compat.thinking_format == ThinkingFormat::QwenChatTemplate && model.reasoning {
        params.insert(
            "chat_template_kwargs".to_owned(),
            json!({
                "enable_thinking": reasoning_effort.is_some(),
                "preserve_thinking": true,
            }),
        );
    } else if compat.thinking_format == ThinkingFormat::ChatTemplate && model.reasoning {
        if let Some(kwargs) =
            build_chat_template_kwargs(model, reasoning_effort, compat, thinking_budget)
        {
            params.insert("chat_template_kwargs".to_owned(), kwargs);
        }
    } else if compat.thinking_format == ThinkingFormat::Baseten && model.reasoning {
        // Port of `openai-completions.ts:779-796` @ c1019d920 / 4181f66.
        // Baseten sends `chat_template_args` (resolved from compat) plus
        // `reasoning_effort` when the model supports it.
        if let Some(args) =
            build_chat_template_args(model, reasoning_effort, compat, thinking_budget)
        {
            params.insert("chat_template_args".to_owned(), args);
        }
        if compat.supports_reasoning_effort {
            // mappedEffort === undefined ? requestedEffort : mappedEffort
            let effort: Option<Option<String>> = match reasoning_effort {
                Some(level) => Some(
                    match model
                        .thinking_level_map
                        .as_ref()
                        .and_then(|map| map.get(&level))
                    {
                        Some(mapped) => mapped.clone(),
                        None => Some(level.as_str().to_owned()),
                    },
                ),
                None => off_value(model),
            };
            if let Some(Some(effort_str)) = effort {
                params.insert("reasoning_effort".to_owned(), json!(effort_str));
            }
        }
    } else if compat.thinking_format == ThinkingFormat::Deepseek && model.reasoning {
        if reasoning_effort.is_some() {
            params.insert("thinking".to_owned(), json!({"type": "enabled"}));
        } else if off_is_not_null(model) {
            params.insert("thinking".to_owned(), json!({"type": "disabled"}));
        }
        if let Some(effort) = reasoning_effort
            && compat.supports_reasoning_effort
        {
            params.insert(
                "reasoning_effort".to_owned(),
                json!(mapped_or_level_name(model, effort)),
            );
        }
    } else if compat.thinking_format == ThinkingFormat::Openrouter && model.reasoning {
        // OpenRouter normalizes reasoning across providers via a nested
        // reasoning object.
        if let Some(effort) = reasoning_effort {
            params.insert(
                "reasoning".to_owned(),
                json!({"effort": mapped_or_level_name(model, effort)}),
            );
        } else if off_is_not_null(model) {
            let effort = off_value(model)
                .flatten()
                .unwrap_or_else(|| "none".to_owned());
            params.insert("reasoning".to_owned(), json!({"effort": effort}));
        }
    } else if compat.thinking_format == ThinkingFormat::AntLing
        && model.reasoning
        && reasoning_effort.is_some()
    {
        // Only a mapped (non-null) effort string is sent.
        if let Some(effort) = reasoning_effort
            && let Some(Some(value)) = model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.get(&effort))
                .cloned()
        {
            params.insert("reasoning".to_owned(), json!({"effort": value}));
        }
    } else if compat.thinking_format == ThinkingFormat::Together && model.reasoning {
        params.insert(
            "reasoning".to_owned(),
            json!({"enabled": reasoning_effort.is_some()}),
        );
        if let Some(effort) = reasoning_effort
            && compat.supports_reasoning_effort
        {
            params.insert(
                "reasoning_effort".to_owned(),
                json!(mapped_or_level_name(model, effort)),
            );
        }
    } else if compat.thinking_format == ThinkingFormat::StringThinking && model.reasoning {
        if let Some(effort) = reasoning_effort {
            params.insert(
                "thinking".to_owned(),
                json!(mapped_or_level_name(model, effort)),
            );
        } else if off_is_not_null(model) {
            let value = off_value(model)
                .flatten()
                .unwrap_or_else(|| "none".to_owned());
            params.insert("thinking".to_owned(), json!(value));
        }
    } else if reasoning_effort.is_some() && model.reasoning && compat.supports_reasoning_effort {
        // OpenAI-style reasoning_effort.
        if let Some(effort) = reasoning_effort {
            params.insert(
                "reasoning_effort".to_owned(),
                json!(mapped_or_level_name(model, effort)),
            );
        }
    } else if reasoning_effort.is_none()
        && model.reasoning
        && compat.supports_reasoning_effort
        && let Some(Some(off)) = off_value(model)
    {
        params.insert("reasoning_effort".to_owned(), json!(off));
    }
}

// ---------------------------------------------------------------------------
// Usage and stop reason mapping
// ---------------------------------------------------------------------------

fn json_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_f64().map(|float| float as u64))
}

/// JS truthiness for JSON values (used where upstream relies on `Boolean` /
/// truthy checks, e.g. the `reasoning_details` `.filter(Boolean)`).
fn is_js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `parseChunkUsage`: OpenAI/OpenRouter usage semantics — `cached_tokens` is
/// cache-read (hits), `cache_write_tokens` a separate write count (do not
/// subtract writes from cached), `completion_tokens` already includes
/// reasoning tokens.
pub fn parse_chunk_usage(raw_usage: &Value, model: &Model) -> Usage {
    let prompt_tokens = json_u64(&raw_usage["prompt_tokens"]).unwrap_or(0);
    let cache_read_tokens = json_u64(&raw_usage["prompt_tokens_details"]["cached_tokens"])
        .or_else(|| json_u64(&raw_usage["prompt_cache_hit_tokens"]))
        // d3ab2af96 (#8119): Kimi reports cache hits at the top level.
        .or_else(|| json_u64(&raw_usage["cached_tokens"]))
        .unwrap_or(0);
    let cache_write_tokens =
        json_u64(&raw_usage["prompt_tokens_details"]["cache_write_tokens"]).unwrap_or(0);

    let input = prompt_tokens.saturating_sub(cache_read_tokens + cache_write_tokens);
    let output = json_u64(&raw_usage["completion_tokens"]).unwrap_or(0);
    let mut usage = Usage {
        input,
        output,
        cache_read: cache_read_tokens,
        cache_write: cache_write_tokens,
        cache_write1h: None,
        reasoning: Some(
            json_u64(&raw_usage["completion_tokens_details"]["reasoning_tokens"]).unwrap_or(0),
        ),
        total_tokens: input + output + cache_read_tokens + cache_write_tokens,
        cost: Default::default(),
    };
    calculate_cost(model, &mut usage);
    usage
}

/// `mapStopReason`; a null finish reason maps to "stop", unknown reasons are
/// an error carrying the raw reason.
pub fn map_stop_reason(reason: Option<&str>) -> (StopReason, Option<String>) {
    let Some(reason) = reason else {
        return (StopReason::Stop, None);
    };
    match reason {
        "stop" | "end" => (StopReason::Stop, None),
        "length" => (StopReason::Length, None),
        "function_call" | "tool_calls" => (StopReason::ToolUse, None),
        other => (
            StopReason::Error,
            Some(format!("Provider finish_reason: {other}")),
        ),
    }
}

// ---------------------------------------------------------------------------
// Stream processing
// ---------------------------------------------------------------------------

/// Custom (grammar) tool streaming state: the input property and the JSON
/// reconstruction buffer (upstream keeps these on the streaming block).
#[derive(Debug, Default)]
struct CustomInputState {
    property: String,
    json_buffer: GrammarToolInputJsonBuffer,
}

/// Per-tool-call streaming scratch state (upstream stores `partialArgs`,
/// `customInput` and `streamIndex` on the block; here they live alongside
/// `output.content`, keyed by content index).
#[derive(Debug, Default)]
struct ToolScratch {
    partial_args: Option<String>,
    custom_input: Option<CustomInputState>,
    stream_index: Option<usize>,
}

/// Outcome of handling one SSE event (the `[DONE]` sentinel ends the stream).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SseOutcome {
    Chunk,
    Done,
}

/// `getCustomToolCallInput`: the current raw grammar input text.
fn custom_tool_call_input(block: &ToolCall, scratch: &ToolScratch) -> String {
    let Some(custom_input) = &scratch.custom_input else {
        return String::new();
    };
    match block.arguments.get(&custom_input.property) {
        Some(Value::String(input)) => input.clone(),
        _ => String::new(),
    }
}

/// `appendCustomToolCallInput`: advance the grammar JSON buffer and replace
/// `block.arguments` with `{ property: nextInput }`. Returns the JSON delta to
/// emit (`None` for a no-op / idempotent close).
fn append_custom_tool_call_input(
    block: &mut ToolCall,
    scratch: &mut ToolScratch,
    next_input: &str,
    close: bool,
) -> Result<Option<String>, String> {
    let Some(custom_input) = &mut scratch.custom_input else {
        return Ok(None);
    };
    let delta = append_grammar_tool_input_json_delta(
        &mut custom_input.json_buffer,
        &custom_input.property,
        next_input,
        close,
    )?;
    let mut arguments = Map::new();
    arguments.insert(custom_input.property.clone(), json!(next_input));
    block.arguments = arguments;
    Ok(delta)
}

/// `isReasoningDetailObject` + `hasValidCommonReasoningDetailFields` +
/// `isOpenAIReasoningDetail` (openai-completions.ts:110-158): the three
/// retained `reasoning_details` shapes — `reasoning.text` (string `text`,
/// optional string `signature`), `reasoning.summary` (string `summary`),
/// `reasoning.encrypted` (string `data`); `id`/`format`/`index` optional
/// common fields.
fn is_openai_reasoning_detail(detail: &Value) -> bool {
    let Some(object) = detail.as_object() else {
        return false;
    };
    // `hasValidCommonReasoningDetailFields`: id/format strings when present,
    // index a number when present.
    for (field, is_string) in [("id", true), ("format", true), ("index", false)] {
        match object.get(field) {
            None | Some(Value::Null) => {}
            Some(value) => {
                if is_string {
                    if !value.is_string() {
                        return false;
                    }
                } else if !value.is_number() {
                    return false;
                }
            }
        }
    }
    match object.get("type").and_then(Value::as_str) {
        Some("reasoning.summary") => object.get("summary").is_some_and(Value::is_string),
        Some("reasoning.encrypted") => object.get("data").is_some_and(Value::is_string),
        Some("reasoning.text") => {
            object.get("text").is_some_and(Value::is_string)
                && object
                    .get("signature")
                    .is_none_or(|signature| signature.is_null() || signature.is_string())
        }
        _ => false,
    }
}

/// `fillMissingCommonReasoningDetailFields` (:255-262): `??=` for id/index,
/// `||=` for format — set only when the target's value is missing/null
/// (resp. falsy).
fn fill_missing_common_reasoning_detail_fields(target: &mut Value, source: &Value) {
    let Some(target) = target.as_object_mut() else {
        return;
    };
    for field in ["id", "index"] {
        let missing = target.get(field).is_none_or(|value| value.is_null());
        if missing && source.get(field).is_some_and(|value| !value.is_null()) {
            target.insert(
                field.to_owned(),
                source.get(field).cloned().unwrap_or(Value::Null),
            );
        }
    }
    let falsy = target
        .get("format")
        .is_none_or(|value| value.is_null() || value == &Value::String(String::new()));
    if falsy && let Some(format) = source.get("format").filter(|value| value.is_string()) {
        target.insert("format".to_owned(), format.clone());
    }
}

/// `appendOpenAIReasoningDetail` (:264-279): consecutive text/summary deltas
/// merge into one logical entry (`text`/`summary` concatenated, signature
/// `||=`); encrypted entries stay opaque and discrete. OpenRouter streams
/// `reasoning_details` as deltas (b7bb00b93, #7994 / c5ad7c1b0, #8605).
fn append_openai_reasoning_detail(details: &mut Vec<Value>, detail: &Value) {
    let detail_type = detail.get("type").and_then(Value::as_str).unwrap_or("");
    let merge_field = match detail_type {
        "reasoning.text" => "text",
        "reasoning.summary" => "summary",
        _ => "",
    };
    if !merge_field.is_empty()
        && let Some(last) = details.last_mut()
    {
        let last_type = last.get("type").and_then(Value::as_str).unwrap_or("");
        if last_type == detail_type {
            if let (Some(last_object), Some(detail_object)) =
                (last.as_object_mut(), detail.as_object())
            {
                let chunk = detail_object
                    .get(merge_field)
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let joined = format!(
                    "{}{chunk}",
                    last_object
                        .get(merge_field)
                        .and_then(Value::as_str)
                        .unwrap_or("")
                );
                last_object.insert(merge_field.to_owned(), json!(joined));
                if detail_type == "reasoning.text" {
                    // `lastDetail.signature ||= detail.signature`.
                    let falsy = last_object.get("signature").is_none_or(|value| {
                        value.is_null() || value == &Value::String(String::new())
                    });
                    if falsy
                        && let Some(signature) = detail_object
                            .get("signature")
                            .filter(|value| value.is_string())
                    {
                        last_object.insert("signature".to_owned(), signature.clone());
                    }
                }
            }
            fill_missing_common_reasoning_detail_fields(last, detail);
            return;
        }
    }
    details.push(detail.clone());
}

/// `parseOpenAIReasoningDetails` (:227-235): a thinking signature holding a
/// non-empty array of valid details.
fn parse_openai_reasoning_details(signature: Option<&str>) -> Option<Vec<Value>> {
    let signature = signature?;
    let parsed: Value = serde_json::from_str(signature).ok()?;
    parsed
        .as_array()
        .filter(|details| !details.is_empty() && details.iter().all(is_openai_reasoning_detail))
        .cloned()
}

/// `parseLegacyEncryptedReasoningDetail` (:237-253): the pre-b7bb00b93
/// storage — an encrypted detail (with non-empty string `id` and non-empty
/// `data`) stashed on a tool call's `thought_signature`.
fn parse_legacy_encrypted_reasoning_detail(signature: Option<&str>) -> Option<Value> {
    let signature = signature?;
    let parsed: Value = serde_json::from_str(signature).ok()?;
    if !is_openai_reasoning_detail(&parsed) {
        return None;
    }
    let id_is_nonempty_string = parsed
        .get("id")
        .is_some_and(|id| id.as_str().is_some_and(|id| !id.is_empty()));
    let data_is_nonempty = parsed
        .get("data")
        .and_then(Value::as_str)
        .is_some_and(|data| !data.is_empty());
    (parsed.get("type").and_then(Value::as_str) == Some("reasoning.encrypted")
        && id_is_nonempty_string
        && data_is_nonempty)
        .then_some(parsed)
}

/// Consumes OpenAI chat completion chunks and drives the [`StreamEvent`]
/// protocol, accumulating the final assistant message. Factored out of the
/// HTTP layer so tests can drive it with recorded streams.
struct CompletionsProcessor<'a> {
    output: &'a mut AssistantMessage,
    model: &'a Model,
    grammar_tool_input_properties: &'a HashMap<String, String>,
    /// Resolved `compat.supportsFinishReason` (2c3041242).
    supports_finish_reason: bool,
    has_finish_reason: bool,
    text_block: Option<usize>,
    thinking_block: Option<usize>,
    tool_call_by_index: HashMap<usize, usize>,
    tool_call_by_id: HashMap<String, usize>,
    scratch: HashMap<usize, ToolScratch>,
    /// `streamedReasoningDetails` (:339): replay metadata buffered in memory
    /// during streaming and serialized into each thinking block's signature
    /// once at finalize (b7bb00b93/#8671) — never a user-visible stream
    /// delta.
    streamed_reasoning_details: Option<Vec<Value>>,
}

impl<'a> CompletionsProcessor<'a> {
    fn new(
        output: &'a mut AssistantMessage,
        model: &'a Model,
        grammar_tool_input_properties: &'a HashMap<String, String>,
        supports_finish_reason: bool,
    ) -> Self {
        Self {
            output,
            model,
            grammar_tool_input_properties,
            supports_finish_reason,
            has_finish_reason: false,
            text_block: None,
            thinking_block: None,
            tool_call_by_index: HashMap::new(),
            tool_call_by_id: HashMap::new(),
            scratch: HashMap::new(),
            streamed_reasoning_details: None,
        }
    }

    /// Per-SSE-event body: `[DONE]` sentinel, strict chunk parse, dispatch.
    fn handle_sse(
        &mut self,
        sse: &ServerSentEvent,
        events: &AssistantMessageEventStream,
    ) -> Result<SseOutcome, String> {
        if sse.data.trim() == "[DONE]" {
            return Ok(SseOutcome::Done);
        }
        // P2-4: tolerate keep-alive/dispatch events with no `data` payload
        // (empty string can never decode to a valid chunk — skip instead of
        // hard-failing the stream).
        if sse.data.trim().is_empty() {
            return Ok(SseOutcome::Chunk);
        }
        let chunk: Value = serde_json::from_str(&sse.data).map_err(|error| {
            format!(
                "Could not parse OpenAI SSE chunk: {error}; data={}",
                sse.data
            )
        })?;
        self.handle_chunk(&chunk, events)?;
        Ok(SseOutcome::Chunk)
    }

    fn ensure_text_block(&mut self, events: &AssistantMessageEventStream) -> usize {
        if let Some(index) = self.text_block {
            return index;
        }
        self.output
            .content
            .push(AssistantContent::Text(TextContent {
                text: String::new(),
                text_signature: None,
            }));
        let index = self.output.content.len() - 1;
        self.text_block = Some(index);
        events.push(StreamEvent::TextStart {
            content_index: index,
            partial: Arc::new(self.output.clone()),
        });
        index
    }

    fn ensure_thinking_block(
        &mut self,
        thinking_signature: &str,
        events: &AssistantMessageEventStream,
    ) -> usize {
        if let Some(index) = self.thinking_block {
            return index;
        }
        self.output
            .content
            .push(AssistantContent::Thinking(ThinkingContent {
                thinking: String::new(),
                thinking_signature: Some(thinking_signature.to_owned()),
                redacted: None,
            }));
        let index = self.output.content.len() - 1;
        self.thinking_block = Some(index);
        events.push(StreamEvent::ThinkingStart {
            content_index: index,
            partial: Arc::new(self.output.clone()),
        });
        index
    }

    /// `ensureToolCallBlock`: find-or-create the streaming tool call block for
    /// a chunk delta, with id/index registration, name back-fill, custom-tool
    /// upgrade and pending reasoning-detail application.
    fn ensure_tool_call_block(
        &mut self,
        tool_call: &Value,
        events: &AssistantMessageEventStream,
    ) -> usize {
        let stream_index = tool_call
            .get("index")
            .and_then(json_u64)
            .map(|index| index as usize);
        let name = tool_call
            .get("function")
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
            .or_else(|| {
                tool_call
                    .get("custom")
                    .and_then(|custom| custom.get("name"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("");
        let id = tool_call.get("id").and_then(Value::as_str);
        // 34239180a: `toolCall.custom && !toolCall.function` — a delta with a
        // valid `function` plus an empty `custom` object stays a function call
        // (its arguments must not be dropped).
        let is_custom = tool_call.get("custom").is_some_and(Value::is_object)
            && !tool_call.get("function").is_some_and(is_js_truthy);

        let mut content_index =
            stream_index.and_then(|index| self.tool_call_by_index.get(&index).copied());
        if content_index.is_none()
            && let Some(id) = id
        {
            content_index = self.tool_call_by_id.get(id).copied();
        }

        if content_index.is_none() {
            // Note: the "input" fallback here should/must not be taken; in case
            // the LLM makes up a tool we don't know about, we at least have a
            // place to stash our stuff.
            let custom_property = if is_custom {
                Some(
                    self.grammar_tool_input_properties
                        .get(name)
                        .cloned()
                        .unwrap_or_else(|| "input".to_owned()),
                )
            } else {
                None
            };
            let mut arguments = Map::new();
            if let Some(property) = &custom_property {
                arguments.insert(property.clone(), json!(""));
            }
            self.output
                .content
                .push(AssistantContent::ToolCall(ToolCall {
                    id: id.unwrap_or("").to_owned(),
                    name: name.to_owned(),
                    arguments,
                    thought_signature: None,
                    namespace: None,
                }));
            let index = self.output.content.len() - 1;
            self.scratch.insert(
                index,
                ToolScratch {
                    partial_args: if custom_property.is_some() {
                        None
                    } else {
                        Some(String::new())
                    },
                    custom_input: custom_property.map(|property| CustomInputState {
                        property,
                        json_buffer: GrammarToolInputJsonBuffer::default(),
                    }),
                    stream_index,
                },
            );
            if let Some(stream_index) = stream_index {
                self.tool_call_by_index.insert(stream_index, index);
            }
            if let Some(id) = id.filter(|id| !id.is_empty()) {
                self.tool_call_by_id.insert(id.to_owned(), index);
            }
            events.push(StreamEvent::ToolCallStart {
                content_index: index,
                partial: Arc::new(self.output.clone()),
            });
            content_index = Some(index);
        }

        // invariant: content_index is always Some after the creation branch
        let index = content_index.unwrap_or(0);

        if let Some(stream_index) = stream_index {
            let register = match self.scratch.get_mut(&index) {
                Some(scratch) if scratch.stream_index.is_none() => {
                    scratch.stream_index = Some(stream_index);
                    true
                }
                _ => false,
            };
            if register {
                self.tool_call_by_index.insert(stream_index, index);
            }
        }
        if let Some(id) = id.filter(|id| !id.is_empty()) {
            self.tool_call_by_id.insert(id.to_owned(), index);
        }
        if let Some(AssistantContent::ToolCall(block)) = self.output.content.get_mut(index)
            && block.name.is_empty()
            && !name.is_empty()
        {
            block.name = name.to_owned();
        }

        // Custom-tool upgrade: a function-created block that later turns out
        // to be custom switches to grammar-input streaming.
        if is_custom
            && self
                .scratch
                .get(&index)
                .is_some_and(|scratch| scratch.custom_input.is_none())
        {
            let block_name = match self.output.content.get(index) {
                Some(AssistantContent::ToolCall(block)) => block.name.clone(),
                _ => String::new(),
            };
            let property = self
                .grammar_tool_input_properties
                .get(&block_name)
                .cloned()
                .unwrap_or_else(|| "input".to_owned());
            if let Some(AssistantContent::ToolCall(block)) = self.output.content.get_mut(index) {
                let mut arguments = Map::new();
                arguments.insert(property.clone(), json!(""));
                block.arguments = arguments;
            }
            if let Some(scratch) = self.scratch.get_mut(&index) {
                scratch.custom_input = Some(CustomInputState {
                    property,
                    json_buffer: GrammarToolInputJsonBuffer::default(),
                });
                scratch.partial_args = None;
            }
        }

        index
    }

    /// Per-chunk body: response metadata, usage, finish reason, and the delta
    /// (text / reasoning / tool calls / encrypted reasoning details).
    fn handle_chunk(
        &mut self,
        chunk: &Value,
        events: &AssistantMessageEventStream,
    ) -> Result<(), String> {
        if !chunk.is_object() {
            return Ok(());
        }

        // OpenAI documents ChatCompletionChunk.id as the unique chat completion
        // identifier; each chunk in a streamed completion carries the same id.
        if self.output.response_id.is_none()
            && let Some(id) = chunk.get("id").and_then(Value::as_str)
        {
            self.output.response_id = Some(id.to_owned());
        }
        if let Some(chunk_model) = chunk.get("model").and_then(Value::as_str)
            && !chunk_model.is_empty()
            && chunk_model != self.model.id
            && self.output.response_model.is_none()
        {
            self.output.response_model = Some(chunk_model.to_owned());
        }
        if chunk.get("usage").is_some_and(is_js_truthy) {
            self.output.usage = parse_chunk_usage(&chunk["usage"], self.model);
        }

        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        else {
            return Ok(());
        };

        // Fallback: some providers (e.g., Moonshot) return usage in
        // choice.usage instead of the standard chunk.usage.
        if !chunk.get("usage").is_some_and(is_js_truthy)
            && let Some(usage) = choice.get("usage").filter(|usage| is_js_truthy(usage))
        {
            self.output.usage = parse_chunk_usage(usage, self.model);
        }

        if let Some(finish_reason) = choice.get("finish_reason")
            && is_js_truthy(finish_reason)
        {
            // fe1c9b6d5: preserve the raw provider reason before mapping.
            self.output.raw_stop_reason = finish_reason.as_str().map(str::to_owned);
            let (stop_reason, error_message) = map_stop_reason(finish_reason.as_str());
            self.output.stop_reason = stop_reason;
            if let Some(error_message) = error_message {
                self.output.error_message = Some(error_message);
            }
            self.has_finish_reason = true;
        }

        let Some(delta) = choice.get("delta").filter(|delta| is_js_truthy(delta)) else {
            return Ok(());
        };

        if let Some(content) = delta.get("content").and_then(Value::as_str)
            && !content.is_empty()
        {
            let content_index = self.ensure_text_block(events);
            if let Some(AssistantContent::Text(block)) = self.output.content.get_mut(content_index)
            {
                block.text.push_str(content);
            }
            events.push(StreamEvent::TextDelta {
                content_index,
                delta: content.to_owned(),
                partial: Arc::new(self.output.clone()),
            });
        }

        // Some endpoints return reasoning in reasoning_content (llama.cpp), or
        // reasoning (other openai compatible endpoints). Use the first
        // non-empty reasoning field to avoid duplication (e.g., chutes.ai
        // returns both with the same content).
        let reasoning_field = ["reasoning_content", "reasoning", "reasoning_text"]
            .into_iter()
            .find(|field| {
                delta
                    .get(field)
                    .and_then(Value::as_str)
                    .is_some_and(|value| !value.is_empty())
            });
        if let Some(field) = reasoning_field {
            // invariant: `find` matched a non-empty string value
            let value = delta.get(field).and_then(Value::as_str).unwrap_or("");
            let thinking_signature = if self.model.provider == "opencode-go" && field == "reasoning"
            {
                "reasoning_content"
            } else {
                field
            };
            let content_index = self.ensure_thinking_block(thinking_signature, events);
            if let Some(AssistantContent::Thinking(block)) =
                self.output.content.get_mut(content_index)
            {
                block.thinking.push_str(value);
            }
            events.push(StreamEvent::ThinkingDelta {
                content_index,
                delta: value.to_owned(),
                partial: Arc::new(self.output.clone()),
            });
        }

        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for tool_call in tool_calls {
                let content_index = self.ensure_tool_call_block(tool_call, events);

                if let Some(id) = tool_call
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                {
                    let needs_id = matches!(
                        self.output.content.get(content_index),
                        Some(AssistantContent::ToolCall(block)) if block.id.is_empty()
                    );
                    if needs_id {
                        if let Some(AssistantContent::ToolCall(block)) =
                            self.output.content.get_mut(content_index)
                        {
                            block.id = id.to_owned();
                        }
                        self.tool_call_by_id.insert(id.to_owned(), content_index);
                    }
                }
                let name = tool_call
                    .get("function")
                    .and_then(|function| function.get("name"))
                    .and_then(Value::as_str)
                    .or_else(|| {
                        tool_call
                            .get("custom")
                            .and_then(|custom| custom.get("name"))
                            .and_then(Value::as_str)
                    });
                if let Some(AssistantContent::ToolCall(block)) =
                    self.output.content.get_mut(content_index)
                    && block.name.is_empty()
                    && let Some(name) = name
                {
                    block.name = name.to_owned();
                }

                let mut delta_text = String::new();
                let function_arguments = tool_call
                    .get("function")
                    .and_then(|function| function.get("arguments"))
                    .and_then(Value::as_str)
                    .filter(|arguments| !arguments.is_empty());
                let custom_input = tool_call
                    .get("custom")
                    .and_then(|custom| custom.get("input"))
                    .and_then(Value::as_str)
                    .filter(|input| !input.is_empty());
                if let Some(arguments) = function_arguments {
                    delta_text = arguments.to_owned();
                    let partial = {
                        let scratch = self.scratch.entry(content_index).or_default();
                        let partial = scratch.partial_args.get_or_insert_with(String::new);
                        partial.push_str(arguments);
                        partial.clone()
                    };
                    if let Some(AssistantContent::ToolCall(block)) =
                        self.output.content.get_mut(content_index)
                    {
                        block.arguments = parse_streaming_json(Some(&partial));
                    }
                } else if let Some(input) = custom_input {
                    let scratch = self.scratch.entry(content_index).or_default();
                    if let Some(AssistantContent::ToolCall(block)) =
                        self.output.content.get_mut(content_index)
                    {
                        let next_input =
                            format!("{}{input}", custom_tool_call_input(block, scratch));
                        delta_text =
                            append_custom_tool_call_input(block, scratch, &next_input, false)?
                                .unwrap_or_default();
                    }
                }
                events.push(StreamEvent::ToolCallDelta {
                    content_index,
                    delta: delta_text,
                    partial: Arc::new(self.output.clone()),
                });
            }
        }

        // b7bb00b93 (#7994): reasoning details are replay metadata, not
        // user-visible stream deltas — buffer them and serialize once at
        // block finalize. Consecutive text/summary deltas merge into logical
        // entries; encrypted entries remain opaque and discrete.
        if let Some(reasoning_details) = delta.get("reasoning_details").and_then(Value::as_array) {
            for detail in reasoning_details {
                if !is_openai_reasoning_detail(detail) {
                    continue;
                }
                self.ensure_thinking_block("", events);
                append_openai_reasoning_detail(
                    self.streamed_reasoning_details.get_or_insert_with(Vec::new),
                    detail,
                );
            }
        }

        Ok(())
    }

    /// `finishBlock`: emit the end event for one content block (closing the
    /// grammar JSON buffer for custom tools first).
    fn finish_block(
        &mut self,
        content_index: usize,
        events: &AssistantMessageEventStream,
    ) -> Result<(), String> {
        enum BlockEnd {
            Text(String),
            Thinking(String),
            ToolCall(ToolCall),
        }

        let mut close_delta: Option<String> = None;
        let streamed_signature = self
            .streamed_reasoning_details
            .as_ref()
            .map(|details| details.iter().map(|d| d.to_string()).collect::<Vec<_>>())
            .map(|parts| format!("[{}]", parts.join(",")));
        let end = match self.output.content.get_mut(content_index) {
            Some(AssistantContent::Text(block)) => Some(BlockEnd::Text(block.text.clone())),
            Some(AssistantContent::Thinking(block)) => {
                // #8671: serialize the buffered details exactly once, at
                // block finalize.
                if let Some(serialized) = &streamed_signature {
                    block.thinking_signature = Some(serialized.clone());
                }
                Some(BlockEnd::Thinking(block.thinking.clone()))
            }
            Some(AssistantContent::ToolCall(block)) => {
                let scratch = self.scratch.entry(content_index).or_default();
                if scratch.custom_input.is_some() {
                    let next_input = custom_tool_call_input(block, scratch);
                    close_delta = append_custom_tool_call_input(block, scratch, &next_input, true)?;
                } else {
                    block.arguments = parse_streaming_json(scratch.partial_args.as_deref());
                }
                Some(BlockEnd::ToolCall(block.clone()))
            }
            None => None,
        };

        if let Some(delta) = close_delta {
            events.push(StreamEvent::ToolCallDelta {
                content_index,
                delta,
                partial: Arc::new(self.output.clone()),
            });
        }
        match end {
            Some(BlockEnd::Text(content)) => events.push(StreamEvent::TextEnd {
                content_index,
                content,
                partial: Arc::new(self.output.clone()),
            }),
            Some(BlockEnd::Thinking(content)) => events.push(StreamEvent::ThinkingEnd {
                content_index,
                content,
                partial: Arc::new(self.output.clone()),
            }),
            Some(BlockEnd::ToolCall(tool_call)) => events.push(StreamEvent::ToolCallEnd {
                content_index,
                tool_call,
                partial: Arc::new(self.output.clone()),
            }),
            None => {}
        }
        Ok(())
    }

    /// The upstream `catch` arm (:699-701): apply buffered reasoning
    /// details to every thinking block on a failed stream.
    fn apply_streamed_reasoning_details_to_thinking_blocks(&mut self) {
        let Some(details) = &self.streamed_reasoning_details else {
            return;
        };
        let serialized = serde_json::to_string(details).unwrap_or_default();
        for block in &mut self.output.content {
            if let AssistantContent::Thinking(block) = block {
                block.thinking_signature = Some(serialized.clone());
            }
        }
    }

    /// Post-stream tail: finish all blocks, then the upstream final checks.
    fn finish(
        mut self,
        signal: Option<&CancellationToken>,
        events: &AssistantMessageEventStream,
    ) -> Result<DoneReason, String> {
        for index in 0..self.output.content.len() {
            self.finish_block(index, events)?;
        }
        if signal.is_some_and(|signal| signal.is_cancelled()) {
            return Err("Request was aborted".to_owned());
        }
        if self.output.stop_reason == StopReason::Aborted {
            return Err("Request was aborted".to_owned());
        }
        // 2c3041242: providers that never send `finish_reason` get their stop
        // reason inferred from content.
        if !self.has_finish_reason && !self.supports_finish_reason {
            self.output.stop_reason = if self
                .output
                .content
                .iter()
                .any(|block| matches!(block, AssistantContent::ToolCall(_)))
            {
                StopReason::ToolUse
            } else {
                StopReason::Stop
            };
        }
        if self.output.stop_reason == StopReason::Error {
            return Err(self
                .output
                .error_message
                .clone()
                .unwrap_or_else(|| "Provider returned an error stop reason".to_owned()));
        }
        if (self.supports_finish_reason && !self.has_finish_reason)
            || self.output.stop_reason == StopReason::Pending
        {
            return Err("Stream ended without finish_reason".to_owned());
        }
        Ok(match self.output.stop_reason {
            StopReason::Length => DoneReason::Length,
            StopReason::ToolUse => DoneReason::ToolUse,
            // Stop is the only remaining reachable reason (Pending/Error/
            // Aborted returned above).
            _ => DoneReason::Stop,
        })
    }
}

// ---------------------------------------------------------------------------
// Stream entry points
// ---------------------------------------------------------------------------

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
    options: &OpenAICompletionsOptions,
    output: &mut AssistantMessage,
    events: &AssistantMessageEventStream,
) -> Result<DoneReason, String> {
    let api_key = get_client_api_key(
        &model.provider,
        options.stream.api_key.as_deref(),
        options.stream.headers.as_ref(),
    )?;
    let compat = get_compat(model);
    let grammar_tool_input_properties = create_grammar_tool_input_properties(
        Some(crate::utils::transcript::get_declared_tools(&context.messages).as_slice()),
        compat.supports_open_ai_grammar_tools,
    )?;
    let cache_retention =
        resolve_cache_retention(options.stream.cache_retention, options.stream.env.as_ref());
    let cache_session_id = if cache_retention == CacheRetention::None {
        None
    } else {
        options.stream.session_id.as_deref()
    };
    let headers = build_client_headers(
        model,
        context,
        &api_key,
        options.stream.headers.as_ref(),
        cache_session_id,
        &compat,
    );
    let mut params = build_params(
        model,
        context,
        options,
        &compat,
        cache_retention,
        &grammar_tool_input_properties,
    )?;
    if let Some(on_payload) = &options.stream.on_payload
        && let Some(next_params) = on_payload(params.clone(), model).await
    {
        params = next_params;
    }

    let url = format!("{}/chat/completions", model.base_url.trim_end_matches('/'));
    let header_map = provider_headers_to_header_map(&headers)?;
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
            let request = client.post(&url).headers(header_map.clone()).json(&params);
            let signal = options.stream.signal.clone();
            let fetch = options.stream.fetch.clone();
            async move {
                // 027a58479 (R2.7.4): per-request custom fetch channel; `None`
                // keeps the reqwest default path unchanged.
                let result = send_provider_request(
                    request,
                    fetch.as_ref(),
                    signal.as_ref(),
                    options.stream.timeout_ms,
                )
                .await;
                match result {
                    Ok(response) => {
                        let status = response.status();
                        if status.is_success() {
                            Ok(response)
                        } else {
                            let status = status.as_u16();
                            let response_headers = headers_to_record(response.headers());
                            let body = response.text().await.unwrap_or_default();
                            // Some providers via OpenRouter give additional
                            // information in error.metadata.raw; append it when
                            // it is not already part of the message.
                            let raw_metadata = serde_json::from_str::<Value>(&body)
                                .ok()
                                .and_then(|value| {
                                    value
                                        .get("error")?
                                        .get("metadata")?
                                        .get("raw")?
                                        .as_str()
                                        .map(str::to_owned)
                                })
                                .filter(|raw| !raw.is_empty());
                            let normalized = NormalizedProviderError::new(
                                Some(status),
                                Some(body),
                                format!("Request failed with status {status}"),
                            );
                            let mut message = format_provider_error(&normalized, None);
                            if let Some(raw) = raw_metadata
                                && !message.contains(&raw)
                            {
                                message.push('\n');
                                message.push_str(&raw);
                            }
                            Err(ProviderErrorInfo {
                                status: Some(status),
                                headers: Some(response_headers),
                                message,
                            })
                        }
                    }
                    Err(error) => Err(error.into_provider_error_info()),
                }
            }
        },
        ProviderRetryOptions {
            max_retries: options.stream.max_retries,
            max_retry_delay_ms: options.stream.max_retry_delay_ms,
            ..Default::default()
        },
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

    let mut processor = CompletionsProcessor::new(
        output,
        model,
        &grammar_tool_input_properties,
        compat.supports_finish_reason,
    );
    let mut decoder = SseDecoder::new();
    // Inter-chunk idle timeout (upstream bodyTimeout; see
    // api::stream_timeouts).
    let mut byte_stream =
        crate::api::stream_timeouts::wrap(response.bytes_stream(), options.stream.timeout_ms);
    let mut saw_done = false;
    // The upstream `catch` (:699-701) serializes buffered reasoning details
    // into every thinking block even when the stream fails — the partial
    // message must still round-trip. Collected here so the `?`-style early
    // returns keep that guarantee.
    let mut result: Result<(), String> = Ok(());
    'body: loop {
        // P1-5: race the body read against the abort token — upstream hands
        // the signal to fetch (cancellation interrupts the body read
        // immediately); a check only after a chunk arrives would stall on an
        // idle body until the idle timeout (5 min default, unbounded when
        // disabled).
        let chunk =
            match next_chunk_or_cancelled(&mut byte_stream, options.stream.signal.as_ref()).await {
                StreamNext::Item(chunk) => chunk,
                StreamNext::Cancelled => {
                    result = Err("Request was aborted".to_owned());
                    break;
                }
            };
        let Some(chunk) = chunk else { break };
        let bytes = match chunk.map_err(|error| error.message()) {
            Ok(bytes) => bytes,
            Err(error) => {
                result = Err(error);
                break;
            }
        };
        for sse in decoder.feed(&bytes)? {
            match processor.handle_sse(&sse, events) {
                Ok(SseOutcome::Done) => {
                    saw_done = true;
                    break 'body;
                }
                Ok(SseOutcome::Chunk) => {}
                Err(error) => {
                    result = Err(error);
                    break 'body;
                }
            }
        }
    }
    if result.is_ok() && !saw_done {
        // P1 (rc.12 review): `finish()` fails when the unterminated tail
        // crosses the line cap after the U+FFFD flush — feed it into the
        // `result` channel instead of panicking the adapter task.
        match decoder.finish() {
            Ok(finish_events) => {
                for sse in finish_events {
                    match processor.handle_sse(&sse, events) {
                        Ok(SseOutcome::Done) => break,
                        Ok(SseOutcome::Chunk) => {}
                        Err(error) => {
                            result = Err(error);
                            break;
                        }
                    }
                }
            }
            Err(error) => result = Err(error),
        }
    }
    if let Err(error) = result {
        processor.apply_streamed_reasoning_details_to_thinking_blocks();
        return Err(error);
    }
    processor.finish(options.stream.signal.as_ref(), events)
}

/// `stream` (openai-completions).
pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: OpenAICompletionsOptions,
) -> AssistantMessageEventStream {
    let event_stream = AssistantMessageEventStream::new();
    let task_stream = event_stream.clone();
    let model = model.clone();
    let context = crate::utils::transcript::resolve_transcript(
        context,
        get_compat(&model).supports_mid_convo_system_messages,
    );
    tokio::spawn(async move {
        let signal = options.stream.signal.clone();
        let mut output = initial_output(&model);
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

/// `streamSimple` (openai-completions): reasoning maps through
/// `clampThinkingLevel`; a clamped "off" omits `reasoning_effort`.
pub fn stream_simple(
    model: &Model,
    context: &TranscriptContext,
    options: Option<SimpleStreamOptions>,
) -> Result<AssistantMessageEventStream, String> {
    // Auth check at the entry, before any stream is constructed
    // (8b5899dce: openai-completions.ts streamSimple runs `getClientApiKey`
    // first and throws synchronously — the Rust equivalent is `Err`).
    get_client_api_key(
        &model.provider,
        options.as_ref().and_then(|o| o.stream.api_key.as_deref()),
        options.as_ref().and_then(|o| o.stream.headers.as_ref()),
    )?;

    let api_key = options.as_ref().and_then(|o| o.stream.api_key.clone());
    let base = build_base_options(model, context, options.as_ref(), api_key);
    let reasoning_effort = options
        .as_ref()
        .and_then(|o| o.reasoning)
        .map(|reasoning| clamp_thinking_level(model, reasoning.to_model_level()))
        .filter(|level| *level != ModelThinkingLevel::Off);

    Ok(stream(
        model,
        context,
        OpenAICompletionsOptions {
            stream: base,
            reasoning_effort,
            tool_choice: options.as_ref().and_then(|o| o.tool_choice).map(|choice| {
                json!(match choice {
                    crate::types::SimpleToolChoice::Auto => "auto",
                    crate::types::SimpleToolChoice::None => "none",
                })
            }),
            thinking_budgets: options.and_then(|o| o.thinking_budgets),
        },
    ))
}

/// `ProviderStreams` implementation for `ApiKind::OPENAI_COMPLETIONS`.
///
/// The trait carries plain [`StreamOptions`]; completions-specific extras
/// ([`OpenAICompletionsOptions`]) reach [`stream`] only through direct calls
/// or via [`stream_simple`] reasoning mapping (design §3.3 collapses per-API
/// extras).
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenAiCompletions;

impl ProviderStreams for OpenAiCompletions {
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        stream(
            model,
            context,
            OpenAICompletionsOptions {
                stream: options.unwrap_or_default(),
                ..OpenAICompletionsOptions::default()
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests {
    use serde_json::{Value, json};

    use super::*;

    pub(crate) fn make_model(extra: Value) -> Model {
        let mut value = json!({
            "id": "gpt-4o", "name": "GPT-4o", "api": "openai-completions",
            "provider": "openai", "baseUrl": "https://api.openai.com",
            "reasoning": true, "input": ["text"],
            "cost": {"input": 2.5, "output": 10.0, "cacheRead": 1.25, "cacheWrite": 2.5},
            "contextWindow": 128000, "maxTokens": 16384
        });
        value
            .as_object_mut()
            .expect("object")
            .extend(extra.as_object().cloned().unwrap_or_default());
        serde_json::from_value(value).expect("model")
    }

    fn usage_json() -> Value {
        json!({
            "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
            "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0}
        })
    }

    /// Assistant message from the target model itself (same-model replay).
    pub(crate) fn same_model_assistant(content: Value) -> Message {
        serde_json::from_value(json!({
            "role": "assistant", "content": content,
            "api": "openai-completions", "provider": "openai", "model": "gpt-4o",
            "usage": usage_json(), "stopReason": "stop", "timestamp": 0
        }))
        .expect("assistant")
    }

    /// Assistant message from another API/model (cross-model handoff).
    pub(crate) fn foreign_assistant(content: Value) -> Message {
        serde_json::from_value(json!({
            "role": "assistant", "content": content,
            "api": "openai-responses", "provider": "github-copilot", "model": "gpt-5",
            "usage": usage_json(), "stopReason": "stop", "timestamp": 0
        }))
        .expect("assistant")
    }

    pub(crate) fn tool_result(tool_call_id: &str, content: Value, extra: Value) -> Message {
        let mut value = json!({
            "role": "toolResult", "toolCallId": tool_call_id, "toolName": "bash",
            "content": content, "isError": false, "timestamp": 0
        });
        value
            .as_object_mut()
            .expect("object")
            .extend(extra.as_object().cloned().unwrap_or_default());
        serde_json::from_value(value).expect("tool result")
    }

    pub(crate) fn user_text(text: &str) -> Message {
        serde_json::from_value(json!({"role": "user", "content": text, "timestamp": 0}))
            .expect("user")
    }

    pub(crate) fn context(
        messages: Vec<Message>,
        tools: Option<Vec<Tool>>,
    ) -> crate::types::TranscriptContext {
        crate::utils::transcript::normalize_context(&crate::types::Context {
            system_prompt: None,
            messages,
            tools,
        })
    }

    pub(crate) fn tool(name: &str) -> Tool {
        serde_json::from_value(json!({
            "name": name, "description": "d",
            "parameters": {"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]}
        }))
        .expect("tool")
    }

    fn no_grammar() -> HashMap<String, String> {
        HashMap::new()
    }

    fn convert(
        model: &Model,
        ctx: &crate::types::TranscriptContext,
        compat: &ResolvedOpenAICompletionsCompat,
    ) -> Vec<Value> {
        convert_messages(model, ctx, compat, &no_grammar()).expect("convert")
    }

    // -- compat detection ----------------------------------------------------

    #[test]
    fn test_detect_compat_default_openai() {
        let compat = detect_compat(&make_model(json!({})));
        assert!(compat.supports_store);
        assert!(compat.supports_developer_role);
        assert!(compat.supports_reasoning_effort);
        assert!(compat.supports_usage_in_streaming);
        assert_eq!(compat.max_tokens_field, MaxTokensField::MaxCompletionTokens);
        assert!(!compat.requires_tool_result_name);
        assert!(!compat.requires_assistant_after_tool_result);
        assert!(!compat.requires_thinking_as_text);
        assert!(!compat.requires_reasoning_content_on_assistant_messages);
        assert_eq!(compat.thinking_format, ThinkingFormat::Openai);
        // #9816 (890f92088): OpenAI compatibility alone does not imply strict
        // support; the runtime default is non-strict for every endpoint and
        // capable models carry explicit catalog metadata instead. G2: was
        // `assert!(compat.supports_strict_mode)` before the flip.
        assert!(!compat.supports_strict_mode);
        assert!(!compat.supports_open_ai_grammar_tools);
        assert_eq!(compat.cache_control_format, None);
        assert!(!compat.send_session_affinity_headers);
        // #9548: the Kimi face moved to supportsMidConvo* (default false).
        assert!(!compat.supports_mid_convo_system_messages);
        assert!(!compat.supports_mid_convo_tool_additions);
        assert_eq!(
            compat.session_affinity_format,
            SessionAffinityFormat::Openai
        );
        assert!(compat.supports_long_cache_retention);
    }

    #[test]
    fn test_detect_compat_zai() {
        let compat = detect_compat(&make_model(json!({"provider": "zai"})));
        assert_eq!(compat.thinking_format, ThinkingFormat::Zai);
        assert!(!compat.supports_store);
        assert!(!compat.supports_reasoning_effort);
        assert!(!compat.supports_developer_role);
        // 2fe21b407 (#7174): Z.AI ignores `max_completion_tokens`.
        assert_eq!(compat.max_tokens_field, MaxTokensField::MaxTokens);
        // baseUrl-only detection works too.
        let by_url = detect_compat(&make_model(
            json!({"baseUrl": "https://api.z.ai/api/paas/v4"}),
        ));
        assert_eq!(by_url.thinking_format, ThinkingFormat::Zai);
        assert_eq!(by_url.max_tokens_field, MaxTokensField::MaxTokens);
    }

    #[test]
    fn test_detect_compat_together_moonshot() {
        let together = detect_compat(&make_model(json!({"provider": "together"})));
        assert_eq!(together.thinking_format, ThinkingFormat::Together);
        assert_eq!(together.max_tokens_field, MaxTokensField::MaxTokens);
        assert!(!together.supports_strict_mode);
        assert!(!together.supports_long_cache_retention);

        let moonshot = detect_compat(&make_model(json!({"provider": "moonshotai"})));
        assert_eq!(moonshot.max_tokens_field, MaxTokensField::MaxTokens);
        assert!(!moonshot.supports_reasoning_effort);
        assert!(!moonshot.supports_strict_mode);
        assert_eq!(moonshot.thinking_format, ThinkingFormat::Openai);
    }

    #[test]
    fn test_detect_compat_openrouter() {
        let plain = detect_compat(&make_model(
            json!({"provider": "openrouter", "id": "meta/llama"}),
        ));
        assert_eq!(plain.thinking_format, ThinkingFormat::Openrouter);
        assert!(!plain.supports_developer_role);
        assert_eq!(
            plain.session_affinity_format,
            SessionAffinityFormat::Openrouter
        );
        assert_eq!(plain.cache_control_format, None);

        let anthropic_id = detect_compat(&make_model(
            json!({"provider": "openrouter", "id": "anthropic/claude"}),
        ));
        assert!(anthropic_id.supports_developer_role);
        assert_eq!(
            anthropic_id.cache_control_format,
            Some(CacheControlFormat::Anthropic)
        );
    }

    #[test]
    fn test_detect_compat_deepseek() {
        let compat = detect_compat(&make_model(json!({
            "provider": "deepseek", "baseUrl": "https://api.deepseek.com"
        })));
        assert_eq!(compat.thinking_format, ThinkingFormat::Deepseek);
        assert!(compat.requires_reasoning_content_on_assistant_messages);
        assert!(!compat.supports_store);
        // deepseek is not in the reasoning-effort exclusion list.
        assert!(compat.supports_reasoning_effort);
        // c185d4123: DeepSeek APIs take `max_tokens`.
        assert_eq!(compat.max_tokens_field, MaxTokensField::MaxTokens);
        let by_url = detect_compat(&make_model(json!({
            "provider": "custom", "baseUrl": "https://api.deepseek.com/v1"
        })));
        assert_eq!(by_url.max_tokens_field, MaxTokensField::MaxTokens);
    }

    #[test]
    fn test_detect_compat_nvidia_ant_ling() {
        let nvidia = detect_compat(&make_model(json!({"provider": "nvidia"})));
        assert_eq!(nvidia.max_tokens_field, MaxTokensField::MaxTokens);
        assert!(!nvidia.supports_reasoning_effort);
        assert!(!nvidia.supports_long_cache_retention);

        let ant_ling = detect_compat(&make_model(json!({"provider": "ant-ling"})));
        assert_eq!(ant_ling.thinking_format, ThinkingFormat::AntLing);
        assert!(!ant_ling.supports_reasoning_effort);
    }

    #[test]
    fn test_detect_compat_grok_and_chutes() {
        let grok = detect_compat(&make_model(json!({"provider": "xai"})));
        assert!(!grok.supports_reasoning_effort);
        assert_eq!(grok.max_tokens_field, MaxTokensField::MaxCompletionTokens);

        let chutes = detect_compat(&make_model(json!({"baseUrl": "https://llm.chutes.ai/v1"})));
        assert_eq!(chutes.max_tokens_field, MaxTokensField::MaxTokens);
        assert!(!chutes.supports_store);
    }

    #[test]
    fn test_get_compat_overrides() {
        let model = make_model(json!({
            "compat": {
                "supportsStore": false,
                "thinkingFormat": "string-thinking",
                "vercelGatewayRouting": {"only": ["bedrock"]}
            }
        }));
        let compat = get_compat(&model);
        assert!(!compat.supports_store);
        assert_eq!(compat.thinking_format, ThinkingFormat::StringThinking);
        assert_eq!(
            compat.vercel_gateway_routing.only,
            Some(vec!["bedrock".to_owned()])
        );
        // Untouched fields keep detected values; openRouterRouting defaults to
        // `{}` (not detected) when absent from model.compat.
        assert!(compat.supports_developer_role);
        assert_eq!(compat.open_router_routing, OpenRouterRouting::default());
    }

    // -- auth / tool history ---------------------------------------------------

    #[test]
    fn test_get_client_api_key() {
        assert_eq!(
            get_client_api_key("openai", Some("sk-key"), None),
            Ok("sk-key".to_owned())
        );
        let headers: ProviderHeaders =
            [("Authorization".to_owned(), Some("Bearer t".to_owned()))].into();
        assert_eq!(
            get_client_api_key("openai", None, Some(&headers)),
            Ok("unused".to_owned())
        );
        let cf: ProviderHeaders =
            [("cf-aig-authorization".to_owned(), Some("t".to_owned()))].into();
        assert_eq!(
            get_client_api_key("openai", None, Some(&cf)),
            Ok("unused".to_owned())
        );
        // Empty header values don't count.
        let empty: ProviderHeaders = [("authorization".to_owned(), Some("  ".to_owned()))].into();
        assert_eq!(
            get_client_api_key("openai", None, Some(&empty)),
            Err("No API key for provider: openai".to_owned())
        );
        assert_eq!(
            get_client_api_key("openai", None, None),
            Err("No API key for provider: openai".to_owned())
        );
    }

    #[test]
    fn test_has_tool_history() {
        assert!(!has_tool_history(&[user_text("hi")]));
        assert!(has_tool_history(&[same_model_assistant(json!([
            {"type": "toolCall", "id": "c1", "name": "bash", "arguments": {}}
        ]))]));
        assert!(has_tool_history(&[tool_result("c1", json!([]), json!({}))]));
    }

    // -- client headers --------------------------------------------------------

    #[test]
    fn test_build_client_headers_base_and_override() {
        let model = make_model(json!({}));
        let ctx = context(vec![user_text("hi")], None);
        let compat = get_compat(&model);
        let headers = build_client_headers(&model, &ctx, "sk-key", None, None, &compat);
        assert_eq!(
            headers.get("accept").and_then(|v| v.as_deref()),
            Some("application/json")
        );
        assert_eq!(
            headers.get("authorization").and_then(|v| v.as_deref()),
            Some("Bearer sk-key")
        );

        // SDK semantics: user headers land after auth headers and win
        // case-insensitively. `87af49dec`: the default pi User-Agent rides as
        // the first merge source and survives when nothing overrides it.
        let options_headers: ProviderHeaders =
            [("Authorization".to_owned(), Some("Bearer user".to_owned()))].into();
        let headers = build_client_headers(
            &model,
            &ctx,
            "sk-key",
            Some(&options_headers),
            None,
            &compat,
        );
        assert_eq!(headers.len(), 3, "accept + authorization + user-agent");
        assert_eq!(
            headers.get("Authorization").and_then(|v| v.as_deref()),
            Some("Bearer user")
        );
        assert_eq!(
            headers.get("User-Agent").and_then(|v| v.as_deref()),
            Some(crate::utils::rpi_user_agent::get_rpi_user_agent().as_str())
        );

        // A `User-Agent` in the request headers overrides the default.
        let options_headers: ProviderHeaders = [
            ("Authorization".to_owned(), Some("Bearer user".to_owned())),
            ("User-Agent".to_owned(), Some("custom-agent/1".to_owned())),
        ]
        .into();
        let headers = build_client_headers(
            &model,
            &ctx,
            "sk-key",
            Some(&options_headers),
            None,
            &compat,
        );
        assert_eq!(
            headers.get("User-Agent").and_then(|v| v.as_deref()),
            Some("custom-agent/1")
        );
    }

    #[test]
    fn test_build_client_headers_session_affinity() {
        let ctx = context(vec![user_text("hi")], None);
        let make = |format: SessionAffinityFormat| {
            let model = make_model(json!({
                "compat": {"sendSessionAffinityHeaders": true, "sessionAffinityFormat": format}
            }));
            let compat = get_compat(&model);
            build_client_headers(&model, &ctx, "k", None, Some("sess-1"), &compat)
        };
        let openrouter = make(SessionAffinityFormat::Openrouter);
        assert_eq!(
            openrouter.get("x-session-id").and_then(|v| v.as_deref()),
            Some("sess-1")
        );
        assert!(!openrouter.contains_key("x-client-request-id"));

        let openai = make(SessionAffinityFormat::Openai);
        for name in ["session_id", "x-client-request-id", "x-session-affinity"] {
            assert_eq!(openai.get(name).and_then(|v| v.as_deref()), Some("sess-1"));
        }

        let nosession = make(SessionAffinityFormat::OpenaiNosession);
        assert!(!nosession.contains_key("session_id"));
        assert!(nosession.contains_key("x-client-request-id"));
        assert!(nosession.contains_key("x-session-affinity"));

        // Disabled compat drops the headers entirely.
        let model = make_model(json!({}));
        let compat = get_compat(&model);
        let headers = build_client_headers(&model, &ctx, "k", None, Some("sess-1"), &compat);
        assert!(!headers.contains_key("x-session-id"));
        assert!(!headers.contains_key("x-session-affinity"));
    }

    // -- convert_messages --------------------------------------------------------

    #[test]
    fn test_convert_messages_system_prompt_role() {
        let model = make_model(json!({}));
        let ctx = crate::utils::transcript::normalize_context(&crate::types::Context {
            system_prompt: Some("be nice".to_owned()),
            messages: vec![user_text("hi")],
            tools: None,
        });
        // reasoning + developer role support → "developer".
        let compat = get_compat(&model);
        let params = convert(&model, &ctx, &compat);
        assert_eq!(
            params[0],
            json!({"role": "developer", "content": "be nice"})
        );

        let non_reasoning = make_model(json!({"reasoning": false}));
        let compat = get_compat(&non_reasoning);
        let params = convert(&non_reasoning, &ctx, &compat);
        assert_eq!(params[0], json!({"role": "system", "content": "be nice"}));
    }

    #[test]
    fn test_convert_messages_user_text_and_image() {
        let model = make_model(json!({"input": ["text", "image"]}));
        let ctx = context(
            vec![
                serde_json::from_value(json!({
                    "role": "user", "timestamp": 0,
                    "content": [
                        {"type": "text", "text": "look"},
                        {"type": "image", "data": "AAAA", "mimeType": "image/png"}
                    ]
                }))
                .expect("user"),
            ],
            None,
        );
        let compat = get_compat(&model);
        let params = convert(&model, &ctx, &compat);
        assert_eq!(
            params,
            vec![json!({
                "role": "user",
                "content": [
                    {"type": "text", "text": "look"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}
                ]
            })]
        );
    }

    /// #9797 (1b6ddca87) port of "omits empty text parts from user messages
    /// with images" (openai-completions-tool-result-images.test.ts:72-95):
    /// image-only user messages no longer carry an empty text part, an empty
    /// part between images is dropped, whitespace-only parts survive, and a
    /// fully empty message is skipped.
    #[test]
    fn test_convert_messages_omits_empty_text_parts() {
        let model = make_model(json!({"input": ["text", "image"]}));
        let compat = get_compat(&model);

        // Image-only: no empty text part (the #9797 regression).
        let ctx = context(
            vec![
                serde_json::from_value(json!({
                    "role": "user", "timestamp": 0,
                    "content": [
                        {"type": "text", "text": ""},
                        {"type": "image", "data": "ZmFrZQ==", "mimeType": "image/png"}
                    ]
                }))
                .expect("user"),
            ],
            None,
        );
        let params = convert(&model, &ctx, &compat);
        assert_eq!(
            params,
            vec![json!({
                "role": "user",
                "content": [
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,ZmFrZQ=="}}
                ]
            })]
        );

        // Empty text between images is dropped; whitespace-only text stays
        // (upstream filters `length > 0`, no trim).
        let ctx = context(
            vec![
                serde_json::from_value(json!({
                    "role": "user", "timestamp": 0,
                    "content": [
                        {"type": "image", "data": "AAAA", "mimeType": "image/png"},
                        {"type": "text", "text": ""},
                        {"type": "text", "text": "  "},
                        {"type": "image", "data": "BBBB", "mimeType": "image/png"}
                    ]
                }))
                .expect("user"),
            ],
            None,
        );
        let params = convert(&model, &ctx, &compat);
        assert_eq!(
            params,
            vec![json!({
                "role": "user",
                "content": [
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}},
                    {"type": "text", "text": "  "},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,BBBB"}}
                ]
            })]
        );

        // All-empty message is skipped entirely (upstream `continue`).
        let ctx = context(
            vec![
                serde_json::from_value(json!({
                    "role": "user", "timestamp": 0,
                    "content": [{"type": "text", "text": ""}]
                }))
                .expect("user"),
                serde_json::from_value(json!({
                    "role": "user", "timestamp": 1, "content": "keep"
                }))
                .expect("user2"),
            ],
            None,
        );
        let params = convert(&model, &ctx, &compat);
        assert_eq!(params, vec![json!({"role": "user", "content": "keep"})]);
    }

    #[test]
    fn test_convert_messages_normalizes_pipe_tool_call_id() {
        let model = make_model(json!({}));
        let ctx = context(
            vec![
                foreign_assistant(json!([
                    {"type": "toolCall", "id": "call_abc|item_xyz", "name": "bash", "arguments": {"cmd": "ls"}}
                ])),
                tool_result(
                    "call_abc|item_xyz",
                    json!([{"type": "text", "text": "ok"}]),
                    json!({}),
                ),
            ],
            None,
        );
        let compat = get_compat(&model);
        let params = convert(&model, &ctx, &compat);
        assert_eq!(params[0]["tool_calls"][0]["id"], json!("call_abc_item_xyz"));
        // The tool result's tool_call_id is back-filled with the normalized id.
        assert_eq!(params[1]["tool_call_id"], json!("call_abc_item_xyz"));
        assert_eq!(params[1]["role"], json!("tool"));
    }

    #[test]
    fn test_convert_messages_pipe_id_long_hash_backfill() {
        let model = make_model(json!({}));
        let long_id = format!("call_{}|{}", "a".repeat(50), "b".repeat(50));
        let ctx = context(
            vec![foreign_assistant(json!([
                {"type": "toolCall", "id": long_id, "name": "bash", "arguments": {}}
            ]))],
            None,
        );
        let compat = get_compat(&model);
        let params = convert(&model, &ctx, &compat);
        let id = params[0]["tool_calls"][0]["id"].as_str().expect("id");
        // 40-char cap: 31-char sanitized call_id prefix + "_" + 8-char hash.
        assert_eq!(id.chars().count(), 40);
        assert!(id.starts_with("call_"));
        let expected_hash: String = short_hash(&long_id).chars().take(8).collect();
        assert!(id.ends_with(&format!("_{expected_hash}")));
    }

    #[test]
    fn test_convert_messages_openai_truncates_plain_id() {
        let model = make_model(json!({}));
        let ctx = context(
            vec![foreign_assistant(json!([
                {"type": "toolCall", "id": "x".repeat(50), "name": "bash", "arguments": {}}
            ]))],
            None,
        );
        let compat = get_compat(&model);
        let params = convert(&model, &ctx, &compat);
        assert_eq!(params[0]["tool_calls"][0]["id"], json!("x".repeat(40)));

        // Non-openai providers keep long ids.
        let other = make_model(json!({"provider": "mistral"}));
        let compat = get_compat(&other);
        let params = convert(&other, &ctx, &compat);
        assert_eq!(params[0]["tool_calls"][0]["id"], json!("x".repeat(50)));
    }

    #[test]
    fn test_convert_messages_assistant_thinking_signature() {
        let model = make_model(json!({}));
        let ctx = context(
            vec![same_model_assistant(json!([
                {"type": "thinking", "thinking": "first", "thinkingSignature": "reasoning_content"},
                {"type": "thinking", "thinking": "second", "thinkingSignature": "reasoning_content"},
                {"type": "text", "text": "answer"}
            ]))],
            None,
        );
        let compat = get_compat(&model);
        let params = convert(&model, &ctx, &compat);
        // Content is the plain text string; thinking lands under the first
        // block's signature key, raw (unsanitized), joined with "\n".
        assert_eq!(
            params[0],
            json!({
                "role": "assistant",
                "content": "answer",
                "reasoning_content": "first\nsecond"
            })
        );
    }

    #[test]
    fn test_convert_messages_opencode_go_signature_mapping() {
        let model =
            make_model(json!({"provider": "opencode-go", "baseUrl": "https://opencode.ai"}));
        let assistant: Message = serde_json::from_value(json!({
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "t", "thinkingSignature": "reasoning"},
                {"type": "text", "text": "answer"}
            ],
            "api": "openai-completions", "provider": "opencode-go", "model": "gpt-4o",
            "usage": usage_json(), "stopReason": "stop", "timestamp": 0
        }))
        .expect("assistant");
        let ctx = context(vec![assistant], None);
        let compat = get_compat(&model);
        let params = convert(&model, &ctx, &compat);
        // opencode-go maps the "reasoning" signature to "reasoning_content".
        assert_eq!(params[0]["reasoning_content"], json!("t"));
        assert!(params[0].get("reasoning").is_none());
    }

    #[test]
    fn test_convert_messages_thinking_as_text() {
        let model = make_model(json!({"compat": {"requiresThinkingAsText": true}}));
        let ctx = context(
            vec![same_model_assistant(json!([
                {"type": "thinking", "thinking": "t1", "thinkingSignature": "reasoning_content"},
                {"type": "thinking", "thinking": "t2", "thinkingSignature": "reasoning_content"},
                {"type": "text", "text": "answer"}
            ]))],
            None,
        );
        let compat = get_compat(&model);
        let params = convert(&model, &ctx, &compat);
        assert_eq!(
            params[0]["content"],
            json!([
                {"type": "text", "text": "t1\n\nt2"},
                {"type": "text", "text": "answer"}
            ])
        );
    }

    #[test]
    fn test_convert_messages_custom_grammar_tool_call() {
        let model = make_model(json!({}));
        let ctx = context(
            vec![same_model_assistant(json!([
                {"type": "toolCall", "id": "c1", "name": "sql", "arguments": {"query": "SELECT 1"}}
            ]))],
            None,
        );
        let compat = get_compat(&model);
        let grammar = HashMap::from([("sql".to_owned(), "query".to_owned())]);
        let params = convert_messages(&model, &ctx, &compat, &grammar).expect("convert");
        assert_eq!(
            params[0]["tool_calls"][0],
            json!({
                "id": "c1",
                "type": "custom",
                "custom": {"name": "sql", "input": "SELECT 1"}
            })
        );

        // A non-string grammar input property is an error.
        let ctx = context(
            vec![same_model_assistant(json!([
                {"type": "toolCall", "id": "c1", "name": "sql", "arguments": {"query": 1}}
            ]))],
            None,
        );
        let error = convert_messages(&model, &ctx, &compat, &grammar).unwrap_err();
        assert_eq!(
            error,
            "Grammar tool call \"sql\" requires argument \"query\" to be a string."
        );
    }

    #[test]
    fn test_convert_messages_reasoning_details_falsy_filtered() {
        let model = make_model(json!({}));
        let ctx = context(
            vec![same_model_assistant(json!([
                {"type": "toolCall", "id": "c1", "name": "bash", "arguments": {},
                 "thoughtSignature": "{\"type\":\"reasoning.encrypted\",\"id\":\"c1\",\"data\":\"sig\"}"},
                {"type": "toolCall", "id": "c2", "name": "bash", "arguments": {},
                 "thoughtSignature": "null"},
                {"type": "toolCall", "id": "c3", "name": "bash", "arguments": {},
                 "thoughtSignature": "0"},
                {"type": "toolCall", "id": "c4", "name": "bash", "arguments": {},
                 "thoughtSignature": "\"\""},
                {"type": "toolCall", "id": "c5", "name": "bash", "arguments": {},
                 "thoughtSignature": "not json"}
            ]))],
            None,
        );
        let compat = get_compat(&model);
        let params = convert(&model, &ctx, &compat);
        // JS `JSON.parse` + `.filter(Boolean)`: null / 0 / "" / parse failures
        // are all dropped.
        assert_eq!(
            params[0]["reasoning_details"],
            json!([{"type": "reasoning.encrypted", "id": "c1", "data": "sig"}])
        );
    }

    #[test]
    fn test_convert_messages_requires_reasoning_content() {
        let model = make_model(json!({
            "compat": {"requiresReasoningContentOnAssistantMessages": true}
        }));
        let ctx = context(
            vec![same_model_assistant(
                json!([{"type": "text", "text": "hi"}]),
            )],
            None,
        );
        let compat = get_compat(&model);
        let params = convert(&model, &ctx, &compat);
        assert_eq!(params[0]["reasoning_content"], json!(""));

        // An existing reasoning_content is not overwritten.
        let ctx = context(
            vec![same_model_assistant(json!([
                {"type": "thinking", "thinking": "t", "thinkingSignature": "reasoning_content"},
                {"type": "text", "text": "hi"}
            ]))],
            None,
        );
        let params = convert(&model, &ctx, &compat);
        assert_eq!(params[0]["reasoning_content"], json!("t"));
    }

    #[test]
    fn test_convert_messages_skips_empty_assistant() {
        let model = make_model(json!({}));
        let ctx = context(
            vec![
                same_model_assistant(json!([])),
                same_model_assistant(json!([{"type": "text", "text": "  "}])),
                user_text("next"),
            ],
            None,
        );
        let compat = get_compat(&model);
        let params = convert(&model, &ctx, &compat);
        assert_eq!(params, vec![json!({"role": "user", "content": "next"})]);
    }

    #[test]
    fn test_convert_messages_tool_result_grouping_and_images() {
        let model = make_model(json!({"input": ["text", "image"]}));
        let ctx = context(
            vec![
                tool_result("c1", json!([{"type": "text", "text": "out1"}]), json!({})),
                tool_result(
                    "c2",
                    json!([{"type": "image", "data": "BBBB", "mimeType": "image/png"}]),
                    json!({}),
                ),
                user_text("go on"),
            ],
            None,
        );
        let compat = get_compat(&model);
        let params = convert(&model, &ctx, &compat);
        assert_eq!(
            params,
            vec![
                json!({"role": "tool", "content": "out1", "tool_call_id": "c1"}),
                json!({"role": "tool", "content": "(see attached image)", "tool_call_id": "c2"}),
                json!({
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "Attached image(s) from tool result:"},
                        {"type": "image_url", "image_url": {"url": "data:image/png;base64,BBBB"}}
                    ]
                }),
                json!({"role": "user", "content": "go on"}),
            ]
        );
    }

    #[test]
    fn test_convert_messages_requires_tool_result_name() {
        let model = make_model(json!({"compat": {"requiresToolResultName": true}}));
        let ctx = context(
            vec![tool_result(
                "c1",
                json!([{"type": "text", "text": "out"}]),
                json!({}),
            )],
            None,
        );
        let compat = get_compat(&model);
        let params = convert(&model, &ctx, &compat);
        assert_eq!(params[0]["name"], json!("bash"));
    }

    /// #9548 (openai-completions.ts:1240-1248): a later system message with
    /// `toolsAdded` becomes a Kimi-style system message carrying the tool
    /// definitions (`tools` field, no content), anchored in place.
    #[test]
    fn test_convert_messages_kimi_tool_additions() {
        let model = make_model(
            json!({"compat": {"supportsMidConvoSystemMessages": true, "supportsMidConvoToolAdditions": true}}),
        );
        // Leading system message declares bash; a later one adds sql
        // (upstream additionContext shape — no standalone Context.tools).
        let ctx = context(
            vec![
                Message::System(crate::types::SystemMessage {
                    role: Default::default(),
                    content: crate::types::SystemContent::Text("base".to_owned()),
                    sections: None,
                    tools_added: Some(vec![tool("bash")]),
                    tools_removed: None,
                    timestamp: 0,
                }),
                user_text("Hello"),
                Message::System(crate::types::SystemMessage {
                    role: Default::default(),
                    content: Default::default(),
                    sections: None,
                    tools_added: Some(vec![tool("sql")]),
                    tools_removed: None,
                    timestamp: 1,
                }),
            ],
            None,
        );
        let compat = get_compat(&model);
        let params = convert(&model, &ctx, &compat);
        // Leading prompt, user message, then the in-place addition.
        assert_eq!(params.len(), 3);
        assert_eq!(params[0]["role"], json!("developer"));
        assert_eq!(params[0]["content"], json!("base"));
        assert_eq!(params[1]["role"], json!("user"));
        assert_eq!(params[2]["role"], json!("system"));
        assert!(params[2].get("content").is_none());
        assert_eq!(params[2]["tools"][0]["function"]["name"], json!("sql"));
    }

    /// #9548: without mid-conversation support the transcript collapses and
    /// no `tools`-bearing system messages are emitted — every tool rides the
    /// request-level list.
    #[test]
    fn test_convert_messages_kimi_additions_collapse_without_support() {
        let model = make_model(json!({"compat": {"supportsMidConvoToolAdditions": true}}));
        let ctx = context(
            vec![
                Message::System(crate::types::SystemMessage {
                    role: Default::default(),
                    content: crate::types::SystemContent::Text("base".to_owned()),
                    sections: None,
                    tools_added: Some(vec![tool("bash")]),
                    tools_removed: None,
                    timestamp: 0,
                }),
                user_text("Hello"),
                Message::System(crate::types::SystemMessage {
                    role: Default::default(),
                    content: Default::default(),
                    sections: None,
                    tools_added: Some(vec![tool("sql")]),
                    tools_removed: None,
                    timestamp: 1,
                }),
            ],
            None,
        );
        let compat = get_compat(&model);
        let params = convert(&model, &ctx, &compat);
        let roles: Vec<&str> = params
            .iter()
            .map(|msg| msg["role"].as_str().unwrap_or(""))
            .collect();
        // Collapsed: one leading developer message (base prompt), then user.
        assert_eq!(roles, ["developer", "user"]);
        assert_eq!(params[0]["content"], json!("base"));
        assert!(params.iter().all(|msg| msg.get("tools").is_none()));
    }

    #[test]
    fn test_get_tools_by_name_duplicate_last_wins() {
        // Upstream `getToolsByName` builds a Map (later duplicates overwrite).
        let mut canonical = tool("late_tool");
        canonical.description = "Canonical definition".to_owned();
        let tools = vec![tool("late_tool"), canonical];
        let found = get_tools_by_name(Some(&tools), &["late_tool".to_owned()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].description, "Canonical definition");
        assert!(get_tools_by_name(None, &["late_tool".to_owned()]).is_empty());
    }

    #[test]
    fn test_convert_messages_bridge_after_tool_result() {
        let model = make_model(json!({
            "compat": {"requiresAssistantAfterToolResult": true}
        }));
        let ctx = context(
            vec![
                tool_result("c1", json!([{"type": "text", "text": "out"}]), json!({})),
                user_text("next"),
            ],
            None,
        );
        let compat = get_compat(&model);
        let params = convert(&model, &ctx, &compat);
        assert_eq!(
            params,
            vec![
                json!({"role": "tool", "content": "out", "tool_call_id": "c1"}),
                json!({"role": "assistant", "content": "I have processed the tool results."}),
                json!({"role": "user", "content": "next"}),
            ]
        );
    }
}

#[cfg(test)]
mod build_and_stream_tests {
    use futures::StreamExt;
    use serde_json::{Value, json};

    use super::tests::{context, make_model, same_model_assistant, tool, tool_result, user_text};
    use super::*;
    use crate::types::{ChatTemplateKwargVar, ThinkingBudgets, ThinkingLevel};

    fn no_grammar() -> HashMap<String, String> {
        HashMap::new()
    }

    fn options(stream: StreamOptions) -> OpenAICompletionsOptions {
        OpenAICompletionsOptions {
            stream,
            ..OpenAICompletionsOptions::default()
        }
    }

    fn params_for(
        model: &Model,
        ctx: &crate::types::TranscriptContext,
        opts: &OpenAICompletionsOptions,
        retention: CacheRetention,
    ) -> Value {
        let compat = get_compat(model);
        build_params(model, ctx, opts, &compat, retention, &no_grammar()).expect("params")
    }

    // -- transport preference ----------------------------------------------------

    #[test]
    fn test_build_params_transport_preference_ignored() {
        // Transport is a codex-only preference; every other provider silently
        // ignores it (upstream: only openai-codex-responses.ts reads it).
        let model = make_model(json!({}));
        let ctx = context(vec![user_text("hi")], None);
        let plain = params_for(
            &model,
            &ctx,
            &options(StreamOptions::default()),
            CacheRetention::Short,
        );
        let with_transport = params_for(
            &model,
            &ctx,
            &options(StreamOptions {
                transport: Some(crate::types::Transport::Websocket),
                ..StreamOptions::default()
            }),
            CacheRetention::Short,
        );
        assert_eq!(
            serde_json::to_string(&plain).expect("serialize"),
            serde_json::to_string(&with_transport).expect("serialize")
        );
    }

    /// "keeps Kimi K2 system text inline without dynamic tool messages"
    /// (transcript-tool-changes.test.ts:304): mid-conversation system
    /// messages without tool-addition support — updates render inline as
    /// plain system text and the request keeps the complete current tool set.
    #[test]
    fn test_convert_messages_kimi_k2_inline_without_tool_additions() {
        let model = make_model(json!({
            "id": "kimi-k2.7-code", "provider": "moonshotai",
            "compat": {"supportsMidConvoSystemMessages": true}
        }));
        let ctx = context(
            vec![
                Message::System(crate::types::SystemMessage {
                    role: Default::default(),
                    content: crate::types::SystemContent::Text("base prompt".to_owned()),
                    sections: None,
                    tools_added: Some(vec![tool("base_tool")]),
                    tools_removed: None,
                    timestamp: 0,
                }),
                user_text("before"),
                Message::System(crate::types::SystemMessage {
                    role: Default::default(),
                    content: crate::types::SystemContent::Text("updated guidance".to_owned()),
                    sections: None,
                    tools_added: Some(vec![tool("late_tool")]),
                    tools_removed: None,
                    timestamp: 2,
                }),
            ],
            None,
        );
        let compat = get_compat(&model);
        let params = convert_messages(&model, &ctx, &compat, &no_grammar()).expect("messages");
        // No message carries a `tools` field (additions not anchored).
        assert!(params.iter().all(|msg| msg.get("tools").is_none()));
        // System text inline: leading prompt + the rendered update
        // (moonshotai is non-standard → instruction role stays "system").
        let system_texts: Vec<&str> = params
            .iter()
            .filter(|msg| msg["role"] == json!("system"))
            .filter_map(|msg| msg["content"].as_str())
            .collect();
        assert_eq!(system_texts, vec!["base prompt", "updated guidance"]);
        // The request-level tool list is the complete current set.
        let opts = options(StreamOptions::default());
        let built = params_for(&model, &ctx, &opts, CacheRetention::Short);
        let names: Vec<&str> = built["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(names, ["base_tool", "late_tool"]);
    }

    #[test]
    fn test_convert_messages_without_kimi_mode_leaves_tools_unchanged() {
        // Upstream "leaves OpenAI Completions tools unchanged without Kimi
        // mode": no system tool message, and build_params keeps every tool.
        let model = make_model(json!({}));
        let ctx = context(
            vec![
                same_model_assistant(json!([
                    {"type": "toolCall", "id": "call_1", "name": "base_tool", "arguments": {}}
                ])),
                tool_result(
                    "call_1",
                    json!([{"type": "text", "text": "done"}]),
                    json!({}),
                ),
            ],
            Some(vec![tool("base_tool"), tool("late_tool")]),
        );
        let compat = get_compat(&model);
        let params = convert_messages(&model, &ctx, &compat, &no_grammar()).expect("messages");
        assert!(
            !params
                .iter()
                .any(|msg| msg["role"] == json!("system") && msg.get("tools").is_some())
        );
        let opts = options(StreamOptions::default());
        let built = params_for(&model, &ctx, &opts, CacheRetention::Short);
        let names: Vec<&str> = built["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(names, ["base_tool", "late_tool"]);
    }

    // -- convert_tools -----------------------------------------------------------

    #[test]
    fn test_convert_tools_function_strict_and_grammar() {
        // #9816 (890f92088): strict comes from explicit metadata (the way
        // the generator pins capable built-ins), not from the runtime
        // default — `make_model(json!({}))` no longer declares strict.
        let model = make_model(json!({"compat": {"supportsStrictMode": true}}));
        let compat = get_compat(&model);
        assert!(compat.supports_strict_mode);

        let function_tool = tool("bash");
        let converted = convert_tools(&[function_tool], &compat).expect("tools");
        assert_eq!(
            converted[0],
            json!({
                "type": "function",
                "function": {
                    "name": "bash",
                    "description": "d",
                    "parameters": {"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]},
                    "strict": false
                }
            })
        );

        // strict omitted when the provider doesn't support it (#9816: the
        // runtime default for unknown endpoints).
        let no_strict = get_compat(&make_model(json!({})));
        assert!(!no_strict.supports_strict_mode);
        let converted = convert_tools(&[tool("bash")], &no_strict).expect("tools");
        assert!(converted[0]["function"].get("strict").is_none());

        // Grammar-constrained tool → custom tool.
        let grammar_model = make_model(json!({"compat": {"supportsOpenAIGrammarTools": true}}));
        let compat = get_compat(&grammar_model);
        let grammar_tool: Tool = serde_json::from_value(json!({
            "name": "sql", "description": "d",
            "parameters": {"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]},
            "constrainedSampling": {"type": "grammar", "variants": {"openai_lark": "start: query"}}
        }))
        .expect("tool");
        let converted = convert_tools(&[grammar_tool], &compat).expect("tools");
        assert_eq!(
            converted[0],
            json!({
                "type": "custom",
                "custom": {
                    "name": "sql",
                    "description": "d",
                    "format": {"type": "grammar", "grammar": {"syntax": "lark", "definition": "start: query"}}
                }
            })
        );
    }

    // -- chat template kwargs ------------------------------------------------------

    /// #9816 (890f92088) port of "defaults unknown OpenAI-compatible
    /// endpoints to non-strict tools": an endpoint with no catalog metadata
    /// sends neither the `strict` field nor the all-args `required` massage.
    #[test]
    fn test_convert_tools_unknown_endpoint_defaults_non_strict() {
        let model = make_model(json!({
            "id": "local-model", "name": "Local Model",
            "provider": "local", "baseUrl": "https://localhost:8000/v1"
        }));
        let compat = get_compat(&model);
        assert!(!compat.supports_strict_mode);

        let strict_tool: Tool = serde_json::from_value(json!({
            "name": "ping", "description": "Ping tool",
            "parameters": {
                "type": "object",
                "properties": {"required": {"type": "string"}, "optional": {"type": "string"}},
                "required": ["required"]
            },
            "constrainedSampling": {"type": "json_schema", "strict": "prefer"}
        }))
        .expect("tool");
        let converted = convert_tools(&[strict_tool], &compat).expect("tools");
        let function_tool = &converted[0]["function"];
        assert!(function_tool.get("strict").is_none());
        // Without strict the schema stays as authored (optional stays
        // optional); upstream asserts `required` equals `["required"]`.
        assert_eq!(function_tool["parameters"]["required"], json!(["required"]));
    }

    /// #9816 (890f92088) port of "preserves strict tools for capable
    /// built-in Chat Completions models": the generator pins explicit
    /// `supportsStrictMode` metadata for known-capable providers, which
    /// overrides the non-strict runtime default via [`get_compat`]. Upstream
    /// anchors on groq `openai/gpt-oss-20b`; its metadata lands in rpi with
    /// the V16-02 catalog regen, so the port uses baseten — a provider whose
    /// vendored snapshot already pins the flag — proving the same mechanism.
    #[test]
    fn test_convert_tools_builtin_metadata_preserves_strict() {
        // Data-driven anchor: the first baseten catalog model whose vendored
        // metadata pins `supportsStrictMode: true` (all 21 entries do today).
        let model = crate::generated::get_builtin_models("baseten")
            .iter()
            .find(|m| {
                m.compat
                    .as_ref()
                    .is_some_and(|c| c.supports_strict_mode == Some(true))
            })
            .expect("baseten catalog model with strict metadata");
        let compat = get_compat(model);
        assert_eq!(
            model.compat.as_ref().expect("compat").supports_strict_mode,
            Some(true)
        );
        assert!(compat.supports_strict_mode);

        let strict_tool: Tool = serde_json::from_value(json!({
            "name": "ping", "description": "Ping tool",
            "parameters": {
                "type": "object",
                "properties": {"required": {"type": "string"}, "optional": {"type": "string"}},
                "required": ["required"]
            },
            "constrainedSampling": {"type": "json_schema", "strict": "prefer"}
        }))
        .expect("tool");
        let converted = convert_tools(&[strict_tool], &compat).expect("tools");
        let function_tool = &converted[0]["function"];
        assert_eq!(function_tool["strict"], json!(true));
        // Strict massaging marks every property required, upstream asserts
        // `required` equals `["required", "optional"]`.
        assert_eq!(
            function_tool["parameters"]["required"],
            json!(["required", "optional"])
        );
    }

    /// #9804 (af7359b90, FR-J): mixed strict/non-strict tool usage 400s on
    /// Cerebras — it pins explicit `supportsStrictMode: false` catalog
    /// metadata (and the #9816 runtime default is non-strict anyway), so
    /// `strict` is omitted from every tool (upstream
    /// `cache-retention.test.ts` "should omit strict field on tools for
    /// cerebras/$id").
    #[test]
    fn test_convert_tools_cerebras_omits_strict_field() {
        for model in [
            make_model(json!({"provider": "cerebras", "id": "gpt-oss-120b"})),
            make_model(
                json!({"provider": "custom", "baseUrl": "https://api.cerebras.ai/v1", "id": "qwen-3.8-27b"}),
            ),
        ] {
            let compat = get_compat(&model);
            assert!(!compat.supports_strict_mode, "{}", model.id);

            // Mixed constrained-sampling (strict) and plain tools: neither
            // may carry `strict`.
            let strict_tool: Tool = serde_json::from_value(json!({
                "name": "t1", "description": "strict tool",
                "parameters": {"type": "object", "properties": {"x": {"type": "string"}}, "required": ["x"]},
                "constrainedSampling": {"type": "json_schema", "strict": "prefer"}
            }))
            .expect("tool");
            let plain_tool = tool("t2");
            let converted = convert_tools(&[strict_tool, plain_tool], &compat).expect("tools");
            assert_eq!(converted.len(), 2);
            for converted_tool in &converted {
                assert!(
                    converted_tool["function"].get("strict").is_none(),
                    "{}: strict must be omitted for {}",
                    model.id,
                    converted_tool["function"]["name"]
                );
            }
        }
    }

    #[test]
    fn test_resolve_chat_template_kwarg_value() {
        let model = make_model(json!({"thinkingLevelMap": {"high": "HIGH", "off": "NONE"}}));

        // Scalar passes through.
        assert_eq!(
            resolve_chat_template_kwarg_value(
                &model,
                None,
                &ChatTemplateKwargValue::Scalar(json!("x")),
                None,
            ),
            Some(json!("x"))
        );

        // thinking.enabled reflects the effort presence.
        let enabled = ChatTemplateKwargValue::Var(ChatTemplateKwargVar {
            var: ChatTemplateKwargVarKind::ThinkingEnabled,
            omit_when_off: None,
        });
        assert_eq!(
            resolve_chat_template_kwarg_value(&model, None, &enabled, None),
            Some(json!(false))
        );
        assert_eq!(
            resolve_chat_template_kwarg_value(
                &model,
                Some(ModelThinkingLevel::High),
                &enabled,
                None
            ),
            Some(json!(true))
        );

        // thinking.effort resolves through the map; omitWhenOff drops it.
        let effort = ChatTemplateKwargValue::Var(ChatTemplateKwargVar {
            var: ChatTemplateKwargVarKind::ThinkingEffort,
            omit_when_off: Some(true),
        });
        assert_eq!(
            resolve_chat_template_kwarg_value(&model, None, &effort, None),
            None
        );
        assert_eq!(
            resolve_chat_template_kwarg_value(
                &model,
                Some(ModelThinkingLevel::High),
                &effort,
                None
            ),
            Some(json!("HIGH"))
        );
        // Unmapped level → the level name itself.
        assert_eq!(
            resolve_chat_template_kwarg_value(&model, Some(ModelThinkingLevel::Low), &effort, None),
            Some(json!("low"))
        );

        // Without omitWhenOff, `off` resolves through the map.
        let keep = ChatTemplateKwargValue::Var(ChatTemplateKwargVar {
            var: ChatTemplateKwargVarKind::ThinkingEffort,
            omit_when_off: None,
        });
        assert_eq!(
            resolve_chat_template_kwarg_value(&model, None, &keep, None),
            Some(json!("NONE"))
        );
    }

    #[test]
    fn test_build_chat_template_kwargs_empty_is_none() {
        let model = make_model(json!({}));
        let compat = get_compat(&model);
        assert_eq!(
            build_chat_template_kwargs(&model, None, &compat, None),
            None
        );
    }

    // -----------------------------------------------------------------------
    // V14-06: reasoning details retention (#7994/#8671), toolChoice (#8607),
    // Kimi cached_tokens (#8119), budget field (#8275), vllmPriority (#9004),
    // DeepSeek case (#7933).
    // -----------------------------------------------------------------------

    fn v14_06_assistant(content: Vec<AssistantContent>) -> Message {
        Message::Assistant(AssistantMessage {
            role: crate::types::AssistantRole::Assistant,
            content,
            api: crate::types::ApiKind("openai-completions".into()),
            // Same model as `make_model` so thinking blocks survive
            // `transform_messages` (cross-model thinking becomes text).
            provider: "openai".to_owned(),
            model: "gpt-4o".to_owned(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            thinking_level: None,
            diagnostics: None,
            usage: Default::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 1,
            deferred: None,
            end_turn: None,
            raw_stop_reason: None,
            duration_ms: None,
        })
    }

    fn reasoning_chunk(details: Value) -> String {
        format!(
            "data: {{\"id\":\"c\",\"model\":\"m\",\"choices\":[{{\"index\":0,\"delta\":{{\"reasoning_details\":{details}}},\"finish_reason\":null}}]}}\n\n"
        )
    }

    fn final_chunk() -> &'static str {
        "data: {\"id\":\"c\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
    }

    /// Streaming merge matrix (b7bb00b93/c5ad7c1b0): consecutive text and
    /// summary deltas each merge into one logical entry; encrypted entries
    /// stay discrete; the serialized signature is written exactly once at
    /// finalize.
    #[test]
    fn test_reasoning_details_merge_and_single_serialization() {
        let model = make_model(json!({}));
        let stream = format!(
            "{}{}{}{}{}",
            reasoning_chunk(json!([{"type": "reasoning.text", "text": "Think "} ])),
            reasoning_chunk(
                json!([{"type": "reasoning.text", "text": "hard", "signature": "sig"}])
            ),
            reasoning_chunk(json!([{"type": "reasoning.summary", "summary": "S1"}])),
            reasoning_chunk(json!([{"type": "reasoning.encrypted", "data": "enc-1"}])),
            final_chunk(),
        );
        let (_events, reason, output) = replay(&model, &no_grammar(), stream.as_bytes());
        assert_eq!(reason, Ok(DoneReason::Stop));
        assert_eq!(output.content.len(), 1, "one thinking block");
        let AssistantContent::Thinking(block) = &output.content[0] else {
            panic!("thinking block");
        };
        // text deltas merged (second carries the signature → `||=` fills);
        // summary is its own entry; encrypted discrete.
        let signature: Value =
            serde_json::from_str(block.thinking_signature.as_deref().expect("signature"))
                .expect("json");
        assert_eq!(
            signature,
            json!([
                {"type": "reasoning.text", "text": "Think hard", "signature": "sig"},
                {"type": "reasoning.summary", "summary": "S1"},
                {"type": "reasoning.encrypted", "data": "enc-1"},
            ])
        );
    }

    /// Replay is verbatim + in order (FR-A red line): a history message whose
    /// thinking signature holds a details array replays it as
    /// `reasoning_details` on the wire, and the legacy encrypted-on-tool-call
    /// storage is the fallback; the reasoning-field-as-signature path is
    /// suppressed when details are preserved.
    #[test]
    fn test_reasoning_details_replay_verbatim_in_order() {
        let model = make_model(json!({}));

        // Signed details win; signature-as-field suppressed.
        let mut signed = v14_06_assistant(vec![
            AssistantContent::Thinking(ThinkingContent {
                thinking: "think".to_owned(),
                thinking_signature: Some(
                    r#"[{"type":"reasoning.text","text":"a b"},{"type":"reasoning.encrypted","data":"enc"}]"#
                        .to_owned(),
                ),
                redacted: None,
            }),
            AssistantContent::Text(TextContent {
                text: "answer".to_owned(),
                text_signature: None,
            }),
        ]);
        let mut ctx = context(vec![signed.clone(), user_text("next")], None);
        let params = build_params(
            &model,
            &ctx,
            &OpenAICompletionsOptions::default(),
            &get_compat(&model),
            CacheRetention::Short,
            &no_grammar(),
        )
        .expect("params");
        let messages = params["messages"].as_array().expect("messages");
        assert_eq!(
            messages[0].get("reasoning_details"),
            Some(&json!([
                {"type": "reasoning.text", "text": "a b"},
                {"type": "reasoning.encrypted", "data": "enc"}
            ])),
            "verbatim, in order"
        );
        // The raw thinking is NOT replayed under a reasoning field.
        assert!(messages[0].get("reasoning").is_none());
        assert!(messages[0].get("reasoning_content").is_none());

        // Legacy fallback: encrypted detail on the tool call (old sessions).
        let Message::Assistant(signed_message) = &mut signed else {
            unreachable!()
        };
        signed_message.content = vec![AssistantContent::ToolCall(ToolCall {
            id: "call_1".to_owned(),
            name: "bash".to_owned(),
            arguments: serde_json::Map::new(),
            thought_signature: Some(
                r#"{"type":"reasoning.encrypted","id":"call_1","data":"enc-old"}"#.to_owned(),
            ),
            namespace: None,
        })];
        ctx = context(vec![signed, user_text("next")], None);
        let params = build_params(
            &model,
            &ctx,
            &OpenAICompletionsOptions::default(),
            &get_compat(&model),
            CacheRetention::Short,
            &no_grammar(),
        )
        .expect("params");
        let messages = params["messages"].as_array().expect("messages");
        assert_eq!(
            messages[0].get("reasoning_details"),
            Some(&json!([
                {"type": "reasoning.encrypted", "id": "call_1", "data": "enc-old"}
            ]))
        );
    }

    /// toolChoice (#8607): an explicitly requested choice is sent even with
    /// no tools in context — and the params carry no `tools` key (upstream
    /// openai-completions-tool-choice.test.ts:153 intent).
    #[test]
    fn test_tool_choice_sent_without_tools() {
        let model = make_model(json!({}));
        let ctx = context(vec![user_text("hi")], None);
        let options = OpenAICompletionsOptions {
            tool_choice: Some(json!("none")),
            ..Default::default()
        };
        let params = build_params(
            &model,
            &ctx,
            &options,
            &get_compat(&model),
            CacheRetention::Short,
            &no_grammar(),
        )
        .expect("params");
        assert_eq!(params["tool_choice"], json!("none"));
        assert!(
            params.get("tools").is_none(),
            "no tools key without tools: {params}"
        );

        // tools=[] with tool history: `params.tools = []` is still sent and
        // the explicit choice stays (upstream :846-849/:855-857).
        let history = v14_06_assistant(vec![AssistantContent::ToolCall(ToolCall {
            id: "call_1".to_owned(),
            name: "bash".to_owned(),
            arguments: serde_json::Map::new(),
            thought_signature: None,
            namespace: None,
        })]);
        let ctx = context(
            vec![
                history,
                tool_result("call_1", json!([]), json!({})),
                user_text("hi"),
            ],
            Some(vec![]),
        );
        let params = build_params(
            &model,
            &ctx,
            &options,
            &get_compat(&model),
            CacheRetention::Short,
            &no_grammar(),
        )
        .expect("params");
        assert_eq!(params["tools"], json!([]));
        assert_eq!(params["tool_choice"], json!("none"));
    }

    /// Kimi usage fallback (#8119): top-level `cached_tokens` is the third
    /// priority.
    #[test]
    fn test_usage_kimi_top_level_cached_tokens() {
        let model = make_model(json!({}));
        let stream = "data: {\"id\":\"c\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":5,\"cached_tokens\":40}}\n\ndata: [DONE]\n\n";
        let (_events, reason, output) = replay(&model, &no_grammar(), stream.as_bytes());
        assert_eq!(reason, Ok(DoneReason::Stop));
        assert_eq!(output.usage.cache_read, 40);

        // Priority: details > prompt_cache_hit_tokens > top level.
        let stream = "data: {\"id\":\"c\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":5,\"cached_tokens\":40,\"prompt_cache_hit_tokens\":7,\"prompt_tokens_details\":{\"cached_tokens\":3}}}\n\ndata: [DONE]\n\n";
        let (_events, _reason, output) = replay(&model, &no_grammar(), stream.as_bytes());
        assert_eq!(output.usage.cache_read, 3);
    }

    /// Budget field generalization (#8275): the three field names and the
    /// bool alias; the 1024 answer-room clamp; `{ "$var": "thinking.budget" }`.
    #[test]
    fn test_thinking_token_budget_field_matrix() {
        for (compat, expected_field) in [
            (
                json!({"thinkingTokenBudgetField": "thinking_budget"}),
                "thinking_budget",
            ),
            (
                json!({"thinkingTokenBudgetField": "thinking_budget_tokens"}),
                "thinking_budget_tokens",
            ),
            (
                json!({"thinkingTokenBudgetField": "thinking_token_budget"}),
                "thinking_token_budget",
            ),
            (
                json!({"supportsThinkingTokenBudget": true}),
                "thinking_token_budget",
            ),
        ] {
            let model = make_model(json!({
                "reasoning": true, "maxTokens": 20000, "compat": compat
            }));
            let ctx = context(vec![user_text("hi")], None);
            let options = OpenAICompletionsOptions {
                reasoning_effort: Some(ModelThinkingLevel::High),
                ..Default::default()
            };
            let params = build_params(
                &model,
                &ctx,
                &options,
                &get_compat(&model),
                CacheRetention::Short,
                &no_grammar(),
            )
            .expect("params");
            assert_eq!(
                params.get(expected_field),
                Some(&json!(16384)),
                "field {expected_field} (max_tokens 20000 - 1024 clamp on high 16384)"
            );
        }

        // Clamp boundary: ceiling - 1024 <= 0 → no budget field at all.
        let model = make_model(json!({
            "reasoning": true, "maxTokens": 1024, "compat": {"supportsThinkingTokenBudget": true}
        }));
        let ctx = context(vec![user_text("hi")], None);
        let options = OpenAICompletionsOptions {
            reasoning_effort: Some(ModelThinkingLevel::High),
            ..Default::default()
        };
        let params = build_params(
            &model,
            &ctx,
            &options,
            &get_compat(&model),
            CacheRetention::Short,
            &no_grammar(),
        )
        .expect("params");
        assert!(params.get("thinking_token_budget").is_none());

        // `$var: thinking.budget` carries the same clamped value.
        let model = make_model(json!({
            "reasoning": true, "maxTokens": 20000,
            "compat": {
                "thinkingFormat": "chat-template",
                "thinkingTokenBudgetField": "thinking_budget",
                "chatTemplateKwargs": {"budget": {"$var": "thinking.budget"}}
            }
        }));
        let ctx = context(vec![user_text("hi")], None);
        let options = OpenAICompletionsOptions {
            reasoning_effort: Some(ModelThinkingLevel::High),
            ..Default::default()
        };
        let params = build_params(
            &model,
            &ctx,
            &options,
            &get_compat(&model),
            CacheRetention::Short,
            &no_grammar(),
        )
        .expect("params");
        assert_eq!(
            params["chat_template_kwargs"]["budget"],
            json!(16384),
            "same clamped budget as the top-level field"
        );
        assert_eq!(params["thinking_budget"], json!(16384));
    }

    /// vllmPriority (#9004): set → top-level `priority`.
    #[test]
    fn test_vllm_priority_top_level_param() {
        let model = make_model(json!({"compat": {"vllmPriority": 7.0}}));
        let ctx = context(vec![user_text("hi")], None);
        let params = build_params(
            &model,
            &ctx,
            &OpenAICompletionsOptions::default(),
            &get_compat(&model),
            CacheRetention::Short,
            &no_grammar(),
        )
        .expect("params");
        assert_eq!(params["priority"], json!(7.0));

        let plain = make_model(json!({}));
        let params = build_params(
            &plain,
            &ctx,
            &OpenAICompletionsOptions::default(),
            &get_compat(&plain),
            CacheRetention::Short,
            &no_grammar(),
        )
        .expect("params");
        assert!(params.get("priority").is_none());
    }

    /// DeepSeek baseUrl detection is case-insensitive (#7933): the
    /// `max_tokens` (not `max_completion_tokens`) wire field follows the
    /// lowercase match.
    #[test]
    fn test_deepseek_base_url_case_insensitive() {
        for base_url in [
            "https://api.DEEPSEEK.com/v1",
            "https://api.deepseek.COM/v1",
            "https://DeepSeek.com/v1",
        ] {
            let model = make_model(json!({"baseUrl": base_url}));
            let ctx = context(vec![user_text("hi")], None);
            let mut options = OpenAICompletionsOptions::default();
            options.stream.max_tokens = Some(512);
            let params = build_params(
                &model,
                &ctx,
                &options,
                &get_compat(&model),
                CacheRetention::Short,
                &no_grammar(),
            )
            .expect("params");
            assert_eq!(
                params.get("max_tokens"),
                Some(&json!(512)),
                "deepseek endpoint takes max_tokens: {base_url}"
            );
            assert!(params.get("max_completion_tokens").is_none());
        }
    }

    // -- cache control ------------------------------------------------------------

    #[test]
    fn test_get_compat_cache_control() {
        let model = make_model(json!({
            "compat": {"cacheControlFormat": "anthropic"}
        }));
        let compat = get_compat(&model);
        assert_eq!(
            get_compat_cache_control(&compat, CacheRetention::Short),
            Some(json!({"type": "ephemeral"}))
        );
        assert_eq!(
            get_compat_cache_control(&compat, CacheRetention::Long),
            Some(json!({"type": "ephemeral", "ttl": "1h"}))
        );
        assert_eq!(
            get_compat_cache_control(&compat, CacheRetention::None),
            None
        );

        // No anthropic format → no marker.
        let compat = get_compat(&make_model(json!({})));
        assert_eq!(
            get_compat_cache_control(&compat, CacheRetention::Short),
            None
        );
    }

    #[test]
    fn test_apply_anthropic_cache_control() {
        let cache_control = json!({"type": "ephemeral"});
        let mut messages = vec![
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": "first"}),
            json!({"role": "assistant", "content": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]}),
            json!({"role": "user", "content": [{"type": "image_url", "image_url": {"url": "u"}}]}),
        ];
        let mut tools = vec![
            json!({"type": "function", "function": {"name": "t1"}}),
            json!({"type": "function", "function": {"name": "t2"}}),
        ];
        apply_anthropic_cache_control(&mut messages, Some(&mut tools), &cache_control);

        // First system message: string content becomes a marked text part.
        assert_eq!(
            messages[0]["content"],
            json!([{"type": "text", "text": "sys", "cache_control": {"type": "ephemeral"}}])
        );
        // Last tool definition is marked.
        assert!(tools[0].get("cache_control").is_none());
        assert_eq!(tools[1]["cache_control"], cache_control);
        // Last conversation message with a text part wins: the trailing user
        // message has no text part, so the assistant's LAST text part is marked.
        assert!(messages[3]["content"][0].get("cache_control").is_none());
        assert!(messages[2]["content"][0].get("cache_control").is_none());
        assert_eq!(messages[2]["content"][1]["cache_control"], cache_control);
        assert!(messages[1]["content"].get("cache_control").is_none());
    }

    // -- build_params ---------------------------------------------------------------

    #[test]
    fn test_build_params_default_openai_shape() {
        let model = make_model(json!({}));
        let ctx = context(vec![user_text("hi")], None);
        let opts = options(StreamOptions {
            max_tokens: Some(1024),
            temperature: Some(0.5),
            session_id: Some("sess".to_owned()),
            ..StreamOptions::default()
        });
        let params = params_for(&model, &ctx, &opts, CacheRetention::Short);
        // Byte-for-byte: key order mirrors the upstream params object.
        assert_eq!(
            serde_json::to_string(&params).expect("serialize"),
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}],"stream":true,"prompt_cache_key":"sess","stream_options":{"include_usage":true},"store":false,"max_completion_tokens":1024,"temperature":0.5}"#
        );
    }

    #[test]
    fn test_build_params_cache_key_and_retention() {
        let model = make_model(json!({}));
        let ctx = context(vec![user_text("hi")], None);
        let opts = options(StreamOptions {
            session_id: Some("s".to_owned()),
            ..StreamOptions::default()
        });

        // "none" retention suppresses the cache key entirely.
        let params = params_for(&model, &ctx, &opts, CacheRetention::None);
        assert!(params.get("prompt_cache_key").is_none());
        assert!(params.get("prompt_cache_retention").is_none());

        // Long retention adds the 24h marker (default openai compat supports it).
        let params = params_for(&model, &ctx, &opts, CacheRetention::Long);
        assert_eq!(params["prompt_cache_key"], json!("s"));
        assert_eq!(params["prompt_cache_retention"], json!("24h"));

        // Non-openai baseUrl: short retention → no key; long still qualifies.
        let other = make_model(json!({"baseUrl": "https://example.com/v1"}));
        let params = params_for(&other, &ctx, &opts, CacheRetention::Short);
        assert!(params.get("prompt_cache_key").is_none());
        let params = params_for(&other, &ctx, &opts, CacheRetention::Long);
        assert_eq!(params["prompt_cache_key"], json!("s"));
    }

    #[test]
    fn test_build_params_max_tokens_field() {
        let ctx = context(vec![user_text("hi")], None);
        let opts = options(StreamOptions {
            max_tokens: Some(100),
            ..StreamOptions::default()
        });
        let together = make_model(json!({"provider": "together"}));
        let params = params_for(&together, &ctx, &opts, CacheRetention::Short);
        assert_eq!(params["max_tokens"], json!(100));
        assert!(params.get("max_completion_tokens").is_none());
    }

    /// Upstream: "sends max_tokens for built-in and custom DeepSeek API
    /// models" (c185d4123 @ 4181f66; runtime detection half — the catalog
    /// `maxTokensField` entries are T26) and "sends max_tokens for Z.AI
    /// completions models" (2fe21b407, #7174). `maxTokens: 123` mirrors the
    /// upstream assertion value.
    #[test]
    fn test_build_params_max_tokens_field_deepseek_zai() {
        let ctx = context(vec![user_text("hi")], None);
        let opts = options(StreamOptions {
            max_tokens: Some(123),
            ..StreamOptions::default()
        });
        let cases = [
            // Native provider id.
            json!({"provider": "deepseek", "baseUrl": "https://api.deepseek.com"}),
            // Custom provider whose baseUrl points at DeepSeek.
            json!({"provider": "custom-deepseek", "baseUrl": "https://api.deepseek.com"}),
            json!({"provider": "zai"}),
            json!({"provider": "zai-coding-cn", "baseUrl": "https://open.bigmodel.cn/api/paas/v4"}),
            json!({"provider": "custom-zai", "baseUrl": "https://api.z.ai/api/paas/v4"}),
        ];
        for extra in cases {
            let model = make_model(extra);
            let params = params_for(&model, &ctx, &opts, CacheRetention::Short);
            assert_eq!(params["max_tokens"], json!(123), "model: {}", model.id);
            assert!(
                params.get("max_completion_tokens").is_none(),
                "model: {}",
                model.id
            );
        }
    }

    /// Upstream sampling-options.test.ts (25a2c8dcf @ 4181f66, #7568):
    /// "merges stream-option sampling params into the request body" +
    /// "overrides named request fields" (sampling params merge last, so their
    /// keys win over named parameters like `temperature`). The model-level →
    /// request-level merge half lives in simple_options.rs tests.
    #[test]
    fn test_build_params_sampling_params_merged_last() {
        let ctx = context(vec![user_text("hi")], None);
        let mut opts = options(StreamOptions {
            temperature: Some(0.0),
            sampling_params: Some(
                [
                    ("top_p".to_owned(), json!(0.95)),
                    ("top_k".to_owned(), json!(0)),
                    ("min_p".to_owned(), json!(0)),
                    ("temperature".to_owned(), json!(1.0)),
                ]
                .into_iter()
                .collect(),
            ),
            ..StreamOptions::default()
        });
        opts.tool_choice = None;
        let model = make_model(json!({}));
        let params = params_for(&model, &ctx, &opts, CacheRetention::Short);
        assert_eq!(params["top_p"], json!(0.95));
        assert_eq!(params["top_k"], json!(0));
        assert_eq!(params["min_p"], json!(0));
        // Custom keys override the named request fields.
        assert_eq!(params["temperature"], json!(1.0));

        // Without sampling params nothing extra appears (upstream "omits
        // sampling params when neither options nor model set them").
        let plain = params_for(
            &model,
            &ctx,
            &options(StreamOptions::default()),
            CacheRetention::Short,
        );
        assert!(plain.get("top_p").is_none());
        assert!(plain.get("temperature").is_none());

        // #9506 (`c01f687e5`): model-level defaults apply to direct
        // stream()/complete() calls; request keys override them.
        let model_with_defaults = make_model(json!({
            "samplingParams": {"top_p": 0.95, "min_p": 0.05}
        }));
        let override_opts = options(StreamOptions {
            sampling_params: Some([("top_p".to_owned(), json!(0.5))].into_iter().collect()),
            ..StreamOptions::default()
        });
        let merged = params_for(
            &model_with_defaults,
            &ctx,
            &override_opts,
            CacheRetention::Short,
        );
        assert_eq!(merged["top_p"], json!(0.5));
        assert_eq!(merged["min_p"], json!(0.05));

        // v1.0.2 (#9776, R2.3.1): the effective (clamped) thinking level's
        // params sit between the model defaults and the request keys.
        let level_model = make_model(json!({
            "reasoning": true,
            "thinkingLevelMap": {"low": null, "medium": null},
            "samplingParams": {"temperature": 1.0, "min_p": 0.05},
            "samplingParamsByThinkingLevel": {"high": {"temperature": 0.8, "top_k": 64}}
        }));
        let mut level_opts = options(StreamOptions {
            sampling_params: Some([("top_p".to_owned(), json!(0.5))].into_iter().collect()),
            ..StreamOptions::default()
        });
        level_opts.reasoning_effort = Some(ModelThinkingLevel::Low);
        level_opts.tool_choice = None;
        let level_params = params_for(&level_model, &ctx, &level_opts, CacheRetention::Short);
        assert_eq!(level_params["temperature"], json!(0.8));
        assert_eq!(level_params["top_p"], json!(0.5));
        assert_eq!(level_params["top_k"], json!(64));
        assert_eq!(level_params["min_p"], json!(0.05));
    }

    /// Upstream openai-completions-thinking-token-budget.test.ts (d07889da0
    /// @ 4181f66, #7638): vLLM `thinking_token_budget` levels, clamps, and
    /// the MIN_ANSWER_TOKENS = 1024 reserve.
    #[test]
    fn test_build_params_thinking_token_budget() {
        let ctx = context(vec![user_text("hi")], None);
        // vLLM-served reasoning model: reasoning and the answer share
        // max_tokens (upstream `vllmModel`).
        let vllm = make_model(json!({
            "id": "zai-org/glm-5.2",
            "provider": "local-vllm",
            "baseUrl": "http://localhost:8000/v1",
            "maxTokens": 16384,
            "compat": {"thinkingFormat": "zai", "supportsThinkingTokenBudget": true}
        }));
        let with_effort =
            |effort: ModelThinkingLevel, budgets: ThinkingBudgets, max_tokens: Option<u32>| {
                OpenAICompletionsOptions {
                    stream: StreamOptions {
                        max_tokens,
                        ..StreamOptions::default()
                    },
                    reasoning_effort: Some(effort),
                    tool_choice: None,
                    thinking_budgets: Some(budgets),
                }
            };
        let no_budgets = ThinkingBudgets::default();

        // "sends the configured budget for the requested level".
        let params = params_for(
            &vllm,
            &ctx,
            &with_effort(
                ModelThinkingLevel::Medium,
                ThinkingBudgets {
                    medium: Some(4096),
                    ..ThinkingBudgets::default()
                },
                None,
            ),
            CacheRetention::Short,
        );
        assert_eq!(params["thinking_token_budget"], json!(4096));

        // "omits the budget when the compat flag is not set".
        let no_flag = make_model(json!({
            "id": "zai-org/glm-5.2",
            "provider": "local-vllm",
            "baseUrl": "http://localhost:8000/v1",
            "maxTokens": 16384,
            "compat": {"thinkingFormat": "zai"}
        }));
        let params = params_for(
            &no_flag,
            &ctx,
            &with_effort(
                ModelThinkingLevel::Medium,
                ThinkingBudgets {
                    medium: Some(4096),
                    ..ThinkingBudgets::default()
                },
                None,
            ),
            CacheRetention::Short,
        );
        assert!(params.get("thinking_token_budget").is_none());

        // "omits the budget when thinking is off".
        let mut opts = options(StreamOptions::default());
        opts.thinking_budgets = Some(ThinkingBudgets {
            high: Some(8192),
            ..ThinkingBudgets::default()
        });
        let params = params_for(&vllm, &ctx, &opts, CacheRetention::Short);
        assert!(params.get("thinking_token_budget").is_none());

        // "clamps xhigh and max to the high budget".
        for level in [ModelThinkingLevel::Xhigh, ModelThinkingLevel::Max] {
            let params = params_for(
                &vllm,
                &ctx,
                &with_effort(
                    level,
                    ThinkingBudgets {
                        high: Some(8192),
                        ..ThinkingBudgets::default()
                    },
                    None,
                ),
                CacheRetention::Short,
            );
            assert_eq!(params["thinking_token_budget"], json!(8192), "{level:?}");
        }

        // "leaves room for the answer when the budget meets the response
        // ceiling": default high budget (16384) equals the model ceiling.
        let params = params_for(
            &vllm,
            &ctx,
            &with_effort(ModelThinkingLevel::High, no_budgets, None),
            CacheRetention::Short,
        );
        assert_eq!(params["thinking_token_budget"], json!(16384 - 1024));

        // "uses the caller max_tokens as the ceiling when it is lower than
        // the model cap".
        let params = params_for(
            &vllm,
            &ctx,
            &with_effort(
                ModelThinkingLevel::High,
                ThinkingBudgets {
                    high: Some(8192),
                    ..ThinkingBudgets::default()
                },
                Some(4096),
            ),
            CacheRetention::Short,
        );
        assert_eq!(params["thinking_token_budget"], json!(4096 - 1024));
    }

    #[test]
    fn test_build_params_tools_and_tool_stream() {
        let ctx = context(vec![user_text("hi")], Some(vec![tool("bash")]));
        let opts = options(StreamOptions::default());

        let model = make_model(json!({}));
        let params = params_for(&model, &ctx, &opts, CacheRetention::Short);
        assert_eq!(params["tools"][0]["function"]["name"], json!("bash"));
        assert!(params.get("tool_stream").is_none());

        // zai tool_stream flag.
        let zai = make_model(json!({"provider": "zai", "compat": {"zaiToolStream": true}}));
        let params = params_for(&zai, &ctx, &opts, CacheRetention::Short);
        assert_eq!(params["tool_stream"], json!(true));

        // Tool history without tools → empty tools array.
        let history_ctx = context(
            vec![same_model_assistant(json!([
                {"type": "toolCall", "id": "c1", "name": "bash", "arguments": {}}
            ]))],
            None,
        );
        let params = params_for(&model, &history_ctx, &opts, CacheRetention::Short);
        assert_eq!(params["tools"], json!([]));
    }

    #[test]
    fn test_build_params_tool_choice() {
        let model = make_model(json!({}));
        let ctx = context(vec![user_text("hi")], None);
        let mut opts = options(StreamOptions::default());
        opts.tool_choice = Some(json!("required"));
        let params = params_for(&model, &ctx, &opts, CacheRetention::Short);
        assert_eq!(params["tool_choice"], json!("required"));
    }

    /// Upstream qwen-token-plan-models.test.ts (4c1a0b92e @ 4181f66,
    /// #6998/#6951), runtime half (catalog assertions are T26): "sends Qwen
    /// thinking fields", "sends Qwen reasoning_effort", "sends qwen3.8 max
    /// reasoning_effort".
    #[test]
    fn test_build_params_qwen_reasoning_effort() {
        let ctx = context(vec![user_text("hi")], None);
        let with_effort = |effort: ModelThinkingLevel| OpenAICompletionsOptions {
            stream: StreamOptions::default(),
            reasoning_effort: Some(effort),
            tool_choice: None,
            thinking_budgets: None,
        };

        // Token-plan thinking models: high/max mapped, the rest null.
        let qwen = make_model(json!({
            "provider": "qwen-token-plan",
            "compat": {"thinkingFormat": "qwen"},
            "thinkingLevelMap": {"minimal": null, "low": null, "medium": null, "high": "high", "xhigh": null, "max": "max"}
        }));
        let params = params_for(
            &qwen,
            &ctx,
            &with_effort(ModelThinkingLevel::High),
            CacheRetention::Short,
        );
        assert_eq!(params["enable_thinking"], json!(true));
        assert_eq!(params["reasoning_effort"], json!("high"));
        assert!(params.get("thinking").is_none());

        // qwen3.8-style map: xhigh mapped, high null (null falls back to the
        // level name — JS `??` semantics).
        let qwen38 = make_model(json!({
            "provider": "qwen-token-plan",
            "compat": {"thinkingFormat": "qwen"},
            "thinkingLevelMap": {"minimal": null, "low": "low", "medium": "medium", "high": null, "xhigh": "xhigh", "max": null}
        }));
        let params = params_for(
            &qwen38,
            &ctx,
            &with_effort(ModelThinkingLevel::Xhigh),
            CacheRetention::Short,
        );
        assert_eq!(params["enable_thinking"], json!(true));
        assert_eq!(params["reasoning_effort"], json!("xhigh"));
        assert!(params.get("thinking").is_none());
        let params = params_for(
            &qwen38,
            &ctx,
            &with_effort(ModelThinkingLevel::High),
            CacheRetention::Short,
        );
        assert_eq!(params["reasoning_effort"], json!("high"));

        // Providers without reasoning-effort support only get enable_thinking.
        let qwen_no_effort = make_model(json!({
            "provider": "qwen-token-plan",
            "compat": {"thinkingFormat": "qwen", "supportsReasoningEffort": false}
        }));
        let params = params_for(
            &qwen_no_effort,
            &ctx,
            &with_effort(ModelThinkingLevel::High),
            CacheRetention::Short,
        );
        assert_eq!(params["enable_thinking"], json!(true));
        assert!(params.get("reasoning_effort").is_none());

        // Thinking off: enable_thinking false, no reasoning_effort.
        let params = params_for(
            &qwen,
            &ctx,
            &options(StreamOptions::default()),
            CacheRetention::Short,
        );
        assert_eq!(params["enable_thinking"], json!(false));
        assert!(params.get("reasoning_effort").is_none());
    }

    #[test]
    fn test_build_params_thinking_formats() {
        let ctx = context(vec![user_text("hi")], None);
        let with_effort = |effort: ModelThinkingLevel| OpenAICompletionsOptions {
            stream: StreamOptions::default(),
            reasoning_effort: Some(effort),
            tool_choice: None,
            thinking_budgets: None,
        };
        let no_effort = options(StreamOptions::default());

        // zai: enabled/disabled thinking object; no reasoning_effort by default.
        let zai = make_model(json!({"provider": "zai"}));
        let params = params_for(&zai, &ctx, &no_effort, CacheRetention::Short);
        assert_eq!(params["thinking"], json!({"type": "disabled"}));
        let params = params_for(
            &zai,
            &ctx,
            &with_effort(ModelThinkingLevel::High),
            CacheRetention::Short,
        );
        assert_eq!(
            params["thinking"],
            json!({"type": "enabled", "clear_thinking": false})
        );
        assert!(params.get("reasoning_effort").is_none());
        // zai with explicit effort support: absent mapping → level name; null
        // mapping → omitted.
        let zai_mapped = make_model(json!({
            "provider": "zai",
            "compat": {"supportsReasoningEffort": true},
            "thinkingLevelMap": {"high": "HIGH", "low": null}
        }));
        let params = params_for(
            &zai_mapped,
            &ctx,
            &with_effort(ModelThinkingLevel::High),
            CacheRetention::Short,
        );
        assert_eq!(params["reasoning_effort"], json!("HIGH"));
        let params = params_for(
            &zai_mapped,
            &ctx,
            &with_effort(ModelThinkingLevel::Medium),
            CacheRetention::Short,
        );
        assert_eq!(params["reasoning_effort"], json!("medium"));
        let params = params_for(
            &zai_mapped,
            &ctx,
            &with_effort(ModelThinkingLevel::Low),
            CacheRetention::Short,
        );
        assert!(params.get("reasoning_effort").is_none());

        // qwen / qwen-chat-template.
        let qwen = make_model(json!({"compat": {"thinkingFormat": "qwen"}}));
        let params = params_for(
            &qwen,
            &ctx,
            &with_effort(ModelThinkingLevel::Low),
            CacheRetention::Short,
        );
        assert_eq!(params["enable_thinking"], json!(true));
        let qwen_ct = make_model(json!({"compat": {"thinkingFormat": "qwen-chat-template"}}));
        let params = params_for(&qwen_ct, &ctx, &no_effort, CacheRetention::Short);
        assert_eq!(
            params["chat_template_kwargs"],
            json!({"enable_thinking": false, "preserve_thinking": true})
        );

        // chat-template with configured kwargs.
        let chat_template = make_model(json!({
            "compat": {
                "thinkingFormat": "chat-template",
                "chatTemplateKwargs": {
                    "on": {"$var": "thinking.enabled"},
                    "level": {"$var": "thinking.effort", "omitWhenOff": true},
                    "fixed": "v"
                }
            }
        }));
        let params = params_for(&chat_template, &ctx, &no_effort, CacheRetention::Short);
        assert_eq!(
            params["chat_template_kwargs"],
            json!({"on": false, "fixed": "v"})
        );
        let params = params_for(
            &chat_template,
            &ctx,
            &with_effort(ModelThinkingLevel::High),
            CacheRetention::Short,
        );
        assert_eq!(
            params["chat_template_kwargs"],
            json!({"on": true, "level": "high", "fixed": "v"})
        );

        // deepseek: disabled unless off is explicitly null; effort adds
        // reasoning_effort (deepseek supports it by detection).
        let deepseek = make_model(json!({"provider": "deepseek"}));
        let params = params_for(&deepseek, &ctx, &no_effort, CacheRetention::Short);
        assert_eq!(params["thinking"], json!({"type": "disabled"}));
        let params = params_for(
            &deepseek,
            &ctx,
            &with_effort(ModelThinkingLevel::High),
            CacheRetention::Short,
        );
        assert_eq!(params["thinking"], json!({"type": "enabled"}));
        assert_eq!(params["reasoning_effort"], json!("high"));
        let deepseek_null_off = make_model(json!({
            "provider": "deepseek", "thinkingLevelMap": {"off": null}
        }));
        let params = params_for(&deepseek_null_off, &ctx, &no_effort, CacheRetention::Short);
        assert!(params.get("thinking").is_none());

        // openrouter: nested reasoning object; off default "none".
        let openrouter = make_model(json!({"provider": "openrouter"}));
        let params = params_for(&openrouter, &ctx, &no_effort, CacheRetention::Short);
        assert_eq!(params["reasoning"], json!({"effort": "none"}));
        let params = params_for(
            &openrouter,
            &ctx,
            &with_effort(ModelThinkingLevel::High),
            CacheRetention::Short,
        );
        assert_eq!(params["reasoning"], json!({"effort": "high"}));

        // openrouter `thinkingLevelMap.off` three states (`off !== null` guard,
        // openai-completions.ts:931): mandatory models carry `off: null` —
        // the reasoning field is omitted entirely so no `effort: "none"`
        // leaks (#8454); an explicit string maps through; absent defaults to
        // "none" (asserted above).
        let mandatory = make_model(json!({
            "provider": "openrouter",
            "thinkingLevelMap": {"off": null, "high": "high"}
        }));
        let params = params_for(&mandatory, &ctx, &no_effort, CacheRetention::Short);
        assert!(
            params.get("reasoning").is_none(),
            "off: null must omit the reasoning field: {params}"
        );
        let mapped_off = make_model(json!({
            "provider": "openrouter",
            "thinkingLevelMap": {"off": "none"}
        }));
        let params = params_for(&mapped_off, &ctx, &no_effort, CacheRetention::Short);
        assert_eq!(params["reasoning"], json!({"effort": "none"}));

        // ant-ling: only a mapped (non-null) effort is sent.
        let ant_ling = make_model(json!({
            "provider": "ant-ling", "thinkingLevelMap": {"high": "HIGH"}
        }));
        let params = params_for(
            &ant_ling,
            &ctx,
            &with_effort(ModelThinkingLevel::High),
            CacheRetention::Short,
        );
        assert_eq!(params["reasoning"], json!({"effort": "HIGH"}));
        let params = params_for(
            &ant_ling,
            &ctx,
            &with_effort(ModelThinkingLevel::Low),
            CacheRetention::Short,
        );
        assert!(params.get("reasoning").is_none());

        // together: enabled flag; reasoning_effort only when supported.
        let together = make_model(json!({"provider": "together"}));
        let params = params_for(&together, &ctx, &no_effort, CacheRetention::Short);
        assert_eq!(params["reasoning"], json!({"enabled": false}));
        let together_effort = make_model(json!({
            "provider": "together", "compat": {"supportsReasoningEffort": true}
        }));
        let params = params_for(
            &together_effort,
            &ctx,
            &with_effort(ModelThinkingLevel::High),
            CacheRetention::Short,
        );
        assert_eq!(params["reasoning"], json!({"enabled": true}));
        assert_eq!(params["reasoning_effort"], json!("high"));

        // baseten (c1019d920): chat_template_args with enable_thinking +
        // reasoning_effort when supported.
        let baseten_glm52 = make_model(json!({
            "compat": {
                "thinkingFormat": "baseten",
                "supportsReasoningEffort": true,
                "chatTemplateArgs": {
                    "enable_thinking": {"$var": "thinking.enabled"}
                }
            },
            "thinkingLevelMap": {"off": "none", "high": "high", "max": "max"}
        }));
        // With reasoning=high: enable_thinking=true, reasoning_effort="high".
        let params = params_for(
            &baseten_glm52,
            &ctx,
            &with_effort(ModelThinkingLevel::High),
            CacheRetention::Short,
        );
        assert_eq!(
            params["chat_template_args"],
            json!({"enable_thinking": true})
        );
        assert_eq!(params["reasoning_effort"], json!("high"));
        // With thinking off: enable_thinking=false, reasoning_effort="none".
        let params = params_for(&baseten_glm52, &ctx, &no_effort, CacheRetention::Short);
        assert_eq!(
            params["chat_template_args"],
            json!({"enable_thinking": false})
        );
        assert_eq!(params["reasoning_effort"], json!("none"));

        // baseten toggle-only (e.g. Kimi K2.6): no reasoning_effort support.
        let baseten_toggle = make_model(json!({
            "compat": {
                "thinkingFormat": "baseten",
                "supportsReasoningEffort": false,
                "chatTemplateArgs": {
                    "enable_thinking": {"$var": "thinking.enabled"}
                }
            },
            "thinkingLevelMap": {"off": "off", "high": "high"}
        }));
        let params = params_for(
            &baseten_toggle,
            &ctx,
            &with_effort(ModelThinkingLevel::High),
            CacheRetention::Short,
        );
        assert_eq!(
            params["chat_template_args"],
            json!({"enable_thinking": true})
        );
        assert!(params.get("reasoning_effort").is_none());

        // string-thinking: plain string; off default "none".
        let string_thinking = make_model(json!({"compat": {"thinkingFormat": "string-thinking"}}));
        let params = params_for(&string_thinking, &ctx, &no_effort, CacheRetention::Short);
        assert_eq!(params["thinking"], json!("none"));
        let params = params_for(
            &string_thinking,
            &ctx,
            &with_effort(ModelThinkingLevel::Medium),
            CacheRetention::Short,
        );
        assert_eq!(params["thinking"], json!("medium"));

        // Default openai: reasoning_effort with map fallback; off mapping used
        // when no effort is given.
        let model = make_model(json!({}));
        let params = params_for(
            &model,
            &ctx,
            &with_effort(ModelThinkingLevel::High),
            CacheRetention::Short,
        );
        assert_eq!(params["reasoning_effort"], json!("high"));
        let off_mapped = make_model(json!({"thinkingLevelMap": {"off": "minimal"}}));
        let params = params_for(&off_mapped, &ctx, &no_effort, CacheRetention::Short);
        assert_eq!(params["reasoning_effort"], json!("minimal"));
        // Non-reasoning models emit no thinking params.
        let plain = make_model(json!({"reasoning": false}));
        let params = params_for(
            &plain,
            &ctx,
            &with_effort(ModelThinkingLevel::High),
            CacheRetention::Short,
        );
        assert!(params.get("reasoning_effort").is_none());
        assert!(params.get("thinking").is_none());
    }

    #[test]
    fn test_build_params_routing() {
        let ctx = context(vec![user_text("hi")], None);
        let opts = options(StreamOptions::default());

        let openrouter = make_model(json!({
            "compat": {"openRouterRouting": {"order": ["a", "b"], "allow_fallbacks": false}}
        }));
        let params = params_for(&openrouter, &ctx, &opts, CacheRetention::Short);
        assert_eq!(
            params["provider"],
            json!({"order": ["a", "b"], "allow_fallbacks": false})
        );

        let vercel = make_model(json!({
            "compat": {"vercelGatewayRouting": {"only": ["bedrock"], "order": ["a"]}}
        }));
        let params = params_for(&vercel, &ctx, &opts, CacheRetention::Short);
        assert_eq!(
            params["providerOptions"],
            json!({"gateway": {"only": ["bedrock"], "order": ["a"]}})
        );

        // Empty routing objects produce nothing.
        let empty = make_model(json!({"compat": {"vercelGatewayRouting": {}}}));
        let params = params_for(&empty, &ctx, &opts, CacheRetention::Short);
        assert!(params.get("providerOptions").is_none());
    }

    #[test]
    fn test_build_params_applies_cache_control() {
        let model = make_model(json!({
            "provider": "openrouter", "id": "anthropic/claude"
        }));
        let ctx = crate::utils::transcript::normalize_context(&crate::types::Context {
            system_prompt: Some("sys".to_owned()),
            messages: vec![user_text("hi")],
            tools: Some(vec![tool("bash")]),
        });
        let opts = options(StreamOptions::default());
        let params = params_for(&model, &ctx, &opts, CacheRetention::Short);
        assert_eq!(
            params["messages"][0]["content"],
            json!([{"type": "text", "text": "sys", "cache_control": {"type": "ephemeral"}}])
        );
        assert_eq!(
            params["tools"][0]["cache_control"],
            json!({"type": "ephemeral"})
        );
        assert_eq!(
            params["messages"][1]["content"],
            json!([{"type": "text", "text": "hi", "cache_control": {"type": "ephemeral"}}])
        );
    }

    // -- usage / stop reason ---------------------------------------------------------

    #[test]
    fn test_parse_chunk_usage() {
        let model = make_model(json!({}));
        let usage = parse_chunk_usage(
            &json!({
                "prompt_tokens": 100,
                "completion_tokens": 20,
                "prompt_tokens_details": {"cached_tokens": 30, "cache_write_tokens": 10},
                "completion_tokens_details": {"reasoning_tokens": 5}
            }),
            &model,
        );
        assert_eq!(usage.input, 60);
        assert_eq!(usage.output, 20);
        assert_eq!(usage.cache_read, 30);
        assert_eq!(usage.cache_write, 10);
        assert_eq!(usage.reasoning, Some(5));
        assert_eq!(usage.total_tokens, 120);

        // Legacy prompt_cache_hit_tokens fallback; clamp at zero.
        let usage = parse_chunk_usage(
            &json!({"prompt_tokens": 10, "prompt_cache_hit_tokens": 50}),
            &model,
        );
        assert_eq!(usage.input, 0);
        assert_eq!(usage.cache_read, 50);

        // Cost is calculated (2.5/M input, 10/M output, 1.25/M read, 2.5/M write).
        let usage = parse_chunk_usage(
            &json!({"prompt_tokens": 1_000_000, "completion_tokens": 1_000_000}),
            &model,
        );
        assert_eq!(usage.cost.input, 2.5);
        assert_eq!(usage.cost.output, 10.0);
    }

    #[test]
    fn test_map_stop_reason() {
        assert_eq!(map_stop_reason(None), (StopReason::Stop, None));
        assert_eq!(map_stop_reason(Some("stop")), (StopReason::Stop, None));
        assert_eq!(map_stop_reason(Some("end")), (StopReason::Stop, None));
        assert_eq!(map_stop_reason(Some("length")), (StopReason::Length, None));
        assert_eq!(
            map_stop_reason(Some("tool_calls")),
            (StopReason::ToolUse, None)
        );
        assert_eq!(
            map_stop_reason(Some("function_call")),
            (StopReason::ToolUse, None)
        );
        assert_eq!(
            map_stop_reason(Some("content_filter")),
            (
                StopReason::Error,
                Some("Provider finish_reason: content_filter".to_owned())
            )
        );
        assert_eq!(
            map_stop_reason(Some("something_new")),
            (
                StopReason::Error,
                Some("Provider finish_reason: something_new".to_owned())
            )
        );
    }

    // -- stream processor replay -------------------------------------------------------

    fn replay(
        model: &Model,
        grammar_props: &HashMap<String, String>,
        bytes: &[u8],
    ) -> (
        Vec<StreamEvent>,
        Result<DoneReason, String>,
        AssistantMessage,
    ) {
        let events = AssistantMessageEventStream::new();
        let mut output = initial_output(model);
        let reason = {
            let mut processor = CompletionsProcessor::new(
                &mut output,
                model,
                grammar_props,
                get_compat(model).supports_finish_reason,
            );
            let mut decoder = SseDecoder::new();
            let mut result: Result<(), String> = Ok(());
            let mut saw_done = false;
            for sse in decoder.feed(bytes).unwrap() {
                match processor.handle_sse(&sse, &events) {
                    Ok(SseOutcome::Chunk) => {}
                    Ok(SseOutcome::Done) => {
                        saw_done = true;
                        break;
                    }
                    Err(error) => {
                        result = Err(error);
                        break;
                    }
                }
            }
            if result.is_ok() && !saw_done {
                for sse in decoder.finish().unwrap() {
                    match processor.handle_sse(&sse, &events) {
                        Ok(SseOutcome::Chunk) => {}
                        Ok(SseOutcome::Done) => break,
                        Err(error) => {
                            result = Err(error);
                            break;
                        }
                    }
                }
            }
            match result {
                Ok(()) => processor.finish(None, &events),
                Err(error) => Err(error),
            }
        };
        events.end(None);
        let collected: Vec<StreamEvent> = futures::executor::block_on(events.collect());
        (collected, reason, output)
    }

    fn event_kinds(events: &[StreamEvent]) -> Vec<&str> {
        events
            .iter()
            .map(|event| match event {
                StreamEvent::Start { .. } => "start",
                StreamEvent::TextStart { .. } => "text_start",
                StreamEvent::TextDelta { .. } => "text_delta",
                StreamEvent::TextEnd { .. } => "text_end",
                StreamEvent::ThinkingStart { .. } => "thinking_start",
                StreamEvent::ThinkingDelta { .. } => "thinking_delta",
                StreamEvent::ThinkingEnd { .. } => "thinking_end",
                StreamEvent::ToolCallStart { .. } => "toolcall_start",
                StreamEvent::ToolCallDelta { .. } => "toolcall_delta",
                StreamEvent::ToolCallEnd { .. } => "toolcall_end",
                StreamEvent::Done { .. } => "done",
                StreamEvent::Error { .. } => "error",
            })
            .collect()
    }

    const RECORDED_STREAM: &str = concat!(
        // Encrypted reasoning detail arrives first (b7bb00b93: buffered into
        // the thinking signature, creating the block immediately).
        "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_details\":[{\"type\":\"reasoning.encrypted\",\"id\":\"call_1\",\"data\":\"sig\"}]},\"finish_reason\":null}]}\n",
        "\n",
        "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n",
        "\n",
        // Both reasoning fields present: the first (reasoning_content) wins.
        "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"think\",\"reasoning\":\"think\"},\"finish_reason\":null}]}\n",
        "\n",
        "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n",
        "\n",
        "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"bash\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n",
        "\n",
        "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"cmd\\\":\"}}]},\"finish_reason\":null}]}\n",
        "\n",
        "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"ls\\\"}\"}}]},\"finish_reason\":null}]}\n",
        "\n",
        "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"prompt_tokens_details\":{\"cached_tokens\":4}}}\n",
        "\n",
        "data: [DONE]\n",
        "\n",
    );

    #[test]
    fn test_processor_recorded_stream() {
        let model = make_model(json!({}));
        let (events, reason, output) = replay(&model, &no_grammar(), RECORDED_STREAM.as_bytes());
        assert_eq!(reason, Ok(DoneReason::ToolUse));
        // G2 (b7bb00b93): the first chunk's reasoning detail now creates the
        // thinking block immediately (was: encrypted details waited on their
        // tool call; text/summary were dropped entirely).
        assert_eq!(
            event_kinds(&events),
            vec![
                "thinking_start",
                "text_start",
                "text_delta",
                "thinking_delta",
                "text_delta",
                "toolcall_start",
                "toolcall_delta", // empty arguments chunk still emits a delta
                "toolcall_delta",
                "toolcall_delta",
                "thinking_end",
                "text_end",
                "toolcall_end",
            ]
        );

        // Final message shape.
        assert_eq!(output.response_id.as_deref(), Some("chatcmpl-1"));
        assert_eq!(output.response_model.as_deref(), Some("gpt-4o-mini"));
        assert_eq!(output.stop_reason, StopReason::ToolUse);
        assert_eq!(output.usage.input, 6);
        assert_eq!(output.usage.cache_read, 4);
        assert_eq!(output.usage.output, 5);
        assert_eq!(output.usage.reasoning, Some(0));
        assert_eq!(output.usage.total_tokens, 15);

        let thinking = match &output.content[0] {
            AssistantContent::Thinking(thinking) => thinking,
            other => panic!("expected thinking block, got {other:?}"),
        };
        assert_eq!(thinking.thinking, "think");
        // G2 (b7bb00b93/#8671): the buffered details serialize into the
        // signature once at finalize (was: the reasoning field name).
        assert_eq!(
            thinking.thinking_signature.as_deref(),
            Some("[{\"type\":\"reasoning.encrypted\",\"id\":\"call_1\",\"data\":\"sig\"}]")
        );
        let text = match &output.content[1] {
            AssistantContent::Text(text) => text,
            other => panic!("expected text block, got {other:?}"),
        };
        assert_eq!(text.text, "Hello world");
        let call = match &output.content[2] {
            AssistantContent::ToolCall(call) => call,
            other => panic!("expected tool call block, got {other:?}"),
        };
        assert_eq!(call.id, "call_1");
        assert_eq!(call.name, "bash");
        assert_eq!(
            call.arguments,
            json!({"cmd": "ls"}).as_object().cloned().unwrap()
        );
        // G2 (b7bb00b93): tool calls no longer carry streaming reasoning
        // details — the thinking signature is the only live copy.
        assert_eq!(call.thought_signature, None);

        // The toolcall_end event carries the finalized call.
        let end_call = events.iter().find_map(|event| match event {
            StreamEvent::ToolCallEnd { tool_call, .. } => Some(tool_call.clone()),
            _ => None,
        });
        assert_eq!(
            end_call.as_ref().map(|c| c.arguments.clone()),
            Some(call.arguments.clone())
        );
    }

    #[test]
    fn test_processor_custom_grammar_tool() {
        let model = make_model(json!({}));
        let grammar = HashMap::from([("sql".to_owned(), "query".to_owned())]);
        let stream = concat!(
            "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"type\":\"custom\",\"custom\":{\"name\":\"sql\",\"input\":\"SELECT \"}}]},\"finish_reason\":null}]}\n",
            "\n",
            "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"custom\":{\"input\":\"*\"}}]},\"finish_reason\":null}]}\n",
            "\n",
            "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n",
            "\n",
            "data: [DONE]\n\n",
        );
        let (events, reason, output) = replay(&model, &grammar, stream.as_bytes());
        assert_eq!(reason, Ok(DoneReason::ToolUse));

        // Grammar input deltas are JSON fragments reconstructing the arguments.
        let deltas: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ToolCallDelta { delta, .. } => Some(delta.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, vec!["{\"query\":\"SELECT ", "*", "\"}"]);

        // 8b5899dce: the `toolcall_start` block's arguments are
        // `{[input_property]: ""}` — not a permanently-empty object; the
        // JSON then arrives through `toolcall_delta` (upstream
        // openai-completions.ts:504-528).
        let start = events
            .iter()
            .find_map(|event| match event {
                StreamEvent::ToolCallStart { partial, .. } => Some(partial.clone()),
                _ => None,
            })
            .expect("toolcall_start event");
        match &start.content[0] {
            AssistantContent::ToolCall(call) => assert_eq!(
                call.arguments,
                json!({"query": ""}).as_object().cloned().unwrap()
            ),
            other => panic!("expected tool call block, got {other:?}"),
        }

        let call = match &output.content[0] {
            AssistantContent::ToolCall(call) => call,
            other => panic!("expected tool call block, got {other:?}"),
        };
        assert_eq!(call.name, "sql");
        assert_eq!(
            call.arguments,
            json!({"query": "SELECT *"}).as_object().cloned().unwrap()
        );
    }

    #[test]
    fn test_processor_done_sentinel_stops_early() {
        let model = make_model(json!({}));
        // Chunks after [DONE] are ignored.
        let stream = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"a\"},\"finish_reason\":\"stop\"}]}\n",
            "\n",
            "data: [DONE]\n",
            "\n",
            "data: {not json}\n",
            "\n",
        );
        let (_events, reason, output) = replay(&model, &no_grammar(), stream.as_bytes());
        assert_eq!(reason, Ok(DoneReason::Stop));
        assert_eq!(
            match &output.content[0] {
                AssistantContent::Text(text) => text.text.as_str(),
                other => panic!("expected text, got {other:?}"),
            },
            "a"
        );
    }

    #[test]
    fn test_processor_strict_parse_error() {
        let model = make_model(json!({}));
        let stream = "data: {not json}\n\n";
        let (_events, reason, _output) = replay(&model, &no_grammar(), stream.as_bytes());
        let error = reason.unwrap_err();
        assert!(
            error.starts_with("Could not parse OpenAI SSE chunk: "),
            "unexpected error: {error}"
        );
        assert!(
            error.contains("; data={not json}"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn test_processor_missing_finish_reason_is_error() {
        let model = make_model(json!({}));
        let stream = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"a\"},\"finish_reason\":null}]}\n\n";
        let (_events, reason, _output) = replay(&model, &no_grammar(), stream.as_bytes());
        assert_eq!(reason, Err("Stream ended without finish_reason".to_owned()));
    }

    #[test]
    fn test_processor_content_filter_error() {
        let model = make_model(json!({}));
        let stream = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"a\"},\"finish_reason\":\"content_filter\"}]}\n",
            "\n",
            "data: [DONE]\n\n",
        );
        let (_events, reason, output) = replay(&model, &no_grammar(), stream.as_bytes());
        assert_eq!(
            reason,
            Err("Provider finish_reason: content_filter".to_owned())
        );
        assert_eq!(output.stop_reason, StopReason::Error);
    }

    #[test]
    fn test_processor_choice_usage_fallback() {
        let model = make_model(json!({}));
        // Moonshot-style: usage on the choice instead of the chunk.
        let stream = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"a\"},\"finish_reason\":\"stop\",\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3}}]}\n",
            "\n",
            "data: [DONE]\n\n",
        );
        let (_events, reason, output) = replay(&model, &no_grammar(), stream.as_bytes());
        assert_eq!(reason, Ok(DoneReason::Stop));
        assert_eq!(output.usage.input, 7);
        assert_eq!(output.usage.output, 3);
    }

    #[test]
    fn test_processor_length_stop_maps_to_done_length() {
        let model = make_model(json!({}));
        let stream = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"a\"},\"finish_reason\":\"length\"}]}\n\n";
        let (_events, reason, _output) = replay(&model, &no_grammar(), stream.as_bytes());
        assert_eq!(reason, Ok(DoneReason::Length));
    }

    // -- raw stop reasons (openai-completions-raw-stop-reason.test.ts: fe1c9b6d5 @ 4181f66) --

    /// openai-completions-raw-stop-reason.test.ts: "preserves raw finish
    /// reasons for successful stops".
    #[test]
    fn preserves_raw_finish_reasons_for_successful_stops() {
        let model = make_model(json!({}));
        let stream = concat!(
            "data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n",
            "\n",
            "data: [DONE]\n\n",
        );
        let (_events, reason, output) = replay(&model, &no_grammar(), stream.as_bytes());
        assert_eq!(reason, Ok(DoneReason::Stop));
        assert_eq!(output.stop_reason, StopReason::Stop);
        assert_eq!(output.raw_stop_reason.as_deref(), Some("stop"));
        assert_eq!(output.error_message, None);
    }

    /// openai-completions-raw-stop-reason.test.ts: "preserves raw finish
    /// reasons for provider error stops".
    #[test]
    fn preserves_raw_finish_reasons_for_provider_error_stops() {
        let model = make_model(json!({}));
        let stream = concat!(
            "data: {\"id\":\"chatcmpl-2\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"content_filter\"}]}\n",
            "\n",
            "data: [DONE]\n\n",
        );
        let (_events, reason, output) = replay(&model, &no_grammar(), stream.as_bytes());
        assert_eq!(
            reason,
            Err("Provider finish_reason: content_filter".to_owned())
        );
        assert_eq!(output.stop_reason, StopReason::Error);
        assert_eq!(output.raw_stop_reason.as_deref(), Some("content_filter"));
        assert_eq!(
            output.error_message.as_deref(),
            Some("Provider finish_reason: content_filter")
        );
    }

    // -- supportsFinishReason (openai-completions-tool-choice.test.ts: 2c3041242 @ 4181f66) --

    /// openai-completions-tool-choice.test.ts: "accepts streams without
    /// finish_reason when compat disables it".
    #[test]
    fn accepts_streams_without_finish_reason_when_compat_disables_it() {
        let model = make_model(json!({"compat": {"supportsFinishReason": false}}));
        let stream = concat!(
            "data: {\"id\":\"chatcmpl-no-finish-reason\",\"choices\":[{\"delta\":{\"content\":\"complete answer\"},\"finish_reason\":null}]}\n",
            "\n",
        );
        let (_events, reason, output) = replay(&model, &no_grammar(), stream.as_bytes());
        assert_eq!(reason, Ok(DoneReason::Stop));
        assert_eq!(output.stop_reason, StopReason::Stop);
        assert_eq!(output.error_message, None);
        match &output.content[0] {
            AssistantContent::Text(text) => assert_eq!(text.text, "complete answer"),
            other => panic!("expected text, got {other:?}"),
        }
    }

    /// The toolUse arm of the 2c3041242 content inference (no upstream test
    /// case; anchors the `toolCall` branch of `output.content.some(...)`).
    #[test]
    fn infers_tool_use_without_finish_reason_when_compat_disables_it() {
        let model = make_model(json!({"compat": {"supportsFinishReason": false}}));
        let stream = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"read\",\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n",
            "\n",
        );
        let (_events, reason, output) = replay(&model, &no_grammar(), stream.as_bytes());
        assert_eq!(reason, Ok(DoneReason::ToolUse));
        assert_eq!(output.stop_reason, StopReason::ToolUse);
    }

    // -- tool-call delta (openai-completions-tool-choice.test.ts: 34239180a @ 4181f66) --

    /// openai-completions-tool-choice.test.ts: "ignores empty custom objects
    /// on function tool call deltas".
    #[test]
    fn ignores_empty_custom_objects_on_function_tool_call_deltas() {
        let model = make_model(json!({}));
        let stream = concat!(
            "data: {\"id\":\"chatcmpl-empty-custom\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"},\"custom\":{}}]},\"finish_reason\":\"tool_calls\"}]}\n",
            "\n",
            "data: [DONE]\n\n",
        );
        let (_events, reason, output) = replay(&model, &no_grammar(), stream.as_bytes());
        assert_eq!(reason, Ok(DoneReason::ToolUse));
        let AssistantContent::ToolCall(call) = &output.content[0] else {
            panic!("expected tool call block, got {:?}", output.content);
        };
        assert_eq!(call.id, "call_1");
        assert_eq!(call.name, "read");
        assert_eq!(
            call.arguments,
            json!({"path": "README.md"}).as_object().cloned().unwrap()
        );
    }

    // -- stream_simple -----------------------------------------------------------

    /// 8b5899dce: missing request auth fails synchronously
    /// (types.ts:325-330) — `Err` return, not an in-stream error event.
    #[test]
    fn test_stream_simple_missing_auth_fails_synchronously() {
        let model = make_model(json!({}));
        assert_eq!(
            stream_simple(&model, &context(vec![user_text("hi")], None), None).err(),
            Some("No API key for provider: openai".to_owned())
        );
    }

    #[test]
    fn test_stream_simple_reasoning_mapping() {
        // Off clamps away entirely; supported levels pass through.
        let model = make_model(json!({}));
        let _ = &model;
        // clampThinkingLevel is exercised via Models tests; here we only assert
        // the level→model-level conversion used by stream_simple.
        assert_eq!(
            ThinkingLevel::High.to_model_level(),
            ModelThinkingLevel::High
        );
        let _ = ThinkingBudgets::default();
    }
}
