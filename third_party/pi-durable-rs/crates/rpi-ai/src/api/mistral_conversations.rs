//! Port of `packages/ai/src/api/mistral-conversations.ts` @ pi 4181f66, which
//! since 9dd90a497 uses a native transport (direct POST + bespoke SSE reader)
//! instead of the `@mistralai/mistralai` SDK.
//! (`mistral-conversations.lazy.ts` is the upstream dynamic-import wrapper;
//! rpi adapters are linked statically, so there is no lazy counterpart.)
//! Stream-termination semantics (rawStopReason, unmapped-reason error stops)
//! per 5a53f086e / 5a2539a7b @ 4181f66.
//!
//! Mistral Chat Completions adapter: request construction (promptMode vs
//! reasoningEffort reasoning selection, `promptCacheKey` / `x-affinity`
//! prompt caching, tool schemas), the camelCase payload + `to_mistral_wire_payload`
//! snake_case remap at send time, the 9-char tool-call id normalizer, SSE
//! stream decoding into [`StreamEvent`]s, cached-token accounting across the
//! six usage field variants, and `stream_simple` reasoning mapping.
//!
//! Intentional differences (upstream deviations, D-021):
//! - HTTP is a direct reqwest call (upstream uses `fetch`); transport errors
//!   carry the reqwest message, and reqwest discards the server-sent reason
//!   phrase, so `StatusCode::canonical_reason` (the standard phrase) stands
//!   in for the upstream `MistralHttpError`'s `statusText`.
//! - SSE framing uses the shared rpi [`SseDecoder`] (line-based) rather than
//!   upstream's bespoke boundary-regex reader (`findMistralEventBoundary`);
//!   the two split events identically for every line-ending combination.
//!   `data:` values strip one leading space where upstream `trimStart`s all
//!   whitespace — immaterial once the payload reaches JSON parsing.
//! - Chunk JSON is decoded strictly with `serde_json` into the chunk structs
//!   (upstream's native transport validates only that the payload is a record
//!   with a `choices` array). Field-level failures read
//!   `Could not parse Mistral SSE chunk: {error}; data={data}` where upstream
//!   would surface the raw `JSON.parse` SyntaxError text; the structural
//!   check matches upstream's `Invalid Mistral streaming event` verbatim.
//! - Request timeouts map reqwest's timeout to
//!   `The operation was aborted due to timeout` — the Node `TimeoutError`
//!   DOMException text upstream's `formatMistralError` surfaces when the
//!   `AbortSignal.timeout` fires. The 60s default matches
//!   `options?.timeoutMs ?? 60_000`.
//! - `retry_provider_request` is an rpi-wide opt-in wrapper (upstream's
//!   native transport never retries; callers set
//!   `StreamOptions::max_retries` to enable it).
//! - `AbortSignal.any` / reader-cancel plumbing is modeled with `select!` on
//!   the body stream; upstream's `Mistral response has no body` branch is
//!   unreachable (reqwest always exposes a body stream).
//! - `stripSymbolKeys` is a no-op: `serde_json::Value` cannot carry the
//!   TypeBox symbol keys the upstream helper strips.
//! - `partialArgs` lives in a processor-side scratch map (rpi's [`ToolCall`]
//!   has no such field); it never leaves the processor, matching upstream's
//!   delete-before-finish semantics.
//!
//! Former D-021 deviations closed by upstream's de-SDK move (9dd90a497):
//! the SDK's `user-agent`/telemetry headers and SDK default timeout no longer
//! exist upstream (rpi now applies the same native 60s default); `on_payload`
//! sees the same camelCase payload upstream exposes (the snake_case mapping
//! moved to `to_mistral_wire_payload` at send time); and the `x-affinity`
//! caller-override check is case-insensitive on both sides
//! (`hasMistralHeaderOverride`).

use std::collections::HashMap;
use std::sync::Arc;

use futures::StreamExt;
use serde_json::{Map, Value, json};

use crate::api::constrained_sampling::resolve_json_schema_strict_sampling;
use crate::api::simple_options::build_base_options;
use crate::api::sse::{ServerSentEvent, SseDecoder};
use crate::models::{ProviderStreams, clamp_thinking_level};
use crate::types::{
    AssistantContent, AssistantMessage, AssistantRole, CacheRetention, DoneReason, ErrorReason,
    InputModality, Message, Model, ModelThinkingLevel, ProviderHeaders, ProviderResponse,
    SimpleStreamOptions, StopReason, StreamEvent, StreamOptions, Tool, ToolResultContent,
    TranscriptContext, Usage, UserContent, UserContentBlock,
};
use crate::utils::cost::calculate_cost;
use crate::utils::custom_fetch::{SendFailure, send_provider_request};
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::hash::short_hash;
use crate::utils::headers::{
    headers_to_record, merge_headers_chain, model_headers, provider_headers_to_header_map,
    rpi_user_agent_headers,
};
use crate::utils::json_parse::parse_streaming_json;
use crate::utils::provider_retry::{
    ProviderErrorInfo, ProviderRetryOptions, RetryError, retry_provider_request,
};
use crate::utils::sanitize_unicode::sanitize_surrogates;
use crate::utils::transform_messages::transform_messages;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const MISTRAL_TOOL_CALL_ID_LENGTH: usize = 9;
const MAX_MISTRAL_ERROR_BODY_CHARS: usize = 4000;
/// Upstream default request timeout: `AbortSignal.timeout(options?.timeoutMs
/// ?? 60_000)` (9dd90a497).
const DEFAULT_MISTRAL_TIMEOUT_MS: u64 = 60_000;
/// The Node `TimeoutError` DOMException message upstream surfaces via
/// `formatMistralError` when the timeout signal fires (Node ≥ 22).
const MISTRAL_TIMEOUT_MESSAGE: &str = "The operation was aborted due to timeout";

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// `MistralOptions.promptMode` — only `"reasoning"` exists upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MistralPromptMode {
    Reasoning,
}

impl MistralPromptMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reasoning => "reasoning",
        }
    }
}

/// `MistralOptions.toolChoice`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MistralToolChoice {
    Auto,
    None,
    Any,
    Required,
    /// `{ type: "function", function: { name } }` — force a specific tool.
    Function {
        name: String,
    },
}

/// `MistralOptions` — `StreamOptions` plus Mistral-specific extras.
///
/// `reasoning_effort` carries the upstream `MistralReasoningEffort` values
/// (`"none" | "high"`); values from `Model::thinking_level_map` pass through
/// verbatim, matching upstream's unchecked cast.
#[derive(Debug, Clone, Default)]
pub struct MistralOptions {
    pub stream: StreamOptions,
    pub tool_choice: Option<MistralToolChoice>,
    pub prompt_mode: Option<MistralPromptMode>,
    pub reasoning_effort: Option<String>,
}

// ---------------------------------------------------------------------------
// Tool-call id normalization (9 chars, alphanumeric)
// ---------------------------------------------------------------------------

/// `createMistralToolCallIdNormalizer`: per-request id mapping with collision
/// resolution (a second id deriving an already-taken candidate retries with
/// `:attempt` seeds).
#[derive(Debug, Default)]
struct MistralToolCallIdNormalizer {
    id_map: HashMap<String, String>,
    reverse_map: HashMap<String, String>,
}

impl MistralToolCallIdNormalizer {
    fn normalize(&mut self, id: &str) -> String {
        if let Some(existing) = self.id_map.get(id) {
            return existing.clone();
        }
        let mut attempt = 0u32;
        loop {
            let candidate = derive_mistral_tool_call_id(id, attempt);
            match self.reverse_map.get(&candidate) {
                Some(owner) if owner != id => attempt += 1,
                _ => {
                    self.id_map.insert(id.to_owned(), candidate.clone());
                    self.reverse_map.insert(candidate.clone(), id.to_owned());
                    return candidate;
                }
            }
        }
    }
}

/// `deriveMistralToolCallId`: ids that are already 9 alphanumeric chars pass
/// through; everything else is replaced by a 9-char alphanumeric prefix of
/// `shortHash(seed)`.
/// `deriveMistralToolCallId` — fallback id for chunks with no usable
/// `id` (`toolcall:{index ?? 0}`, attempt 0).
fn derive_mistral_tool_call_id(id: &str, attempt: u32) -> String {
    // JS `id.replace(/[^a-zA-Z0-9]/g, "")`.
    let normalized: String = id.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if attempt == 0 && normalized.len() == MISTRAL_TOOL_CALL_ID_LENGTH {
        return normalized;
    }
    let seed_base = if normalized.is_empty() {
        id
    } else {
        normalized.as_str()
    };
    let seed = if attempt == 0 {
        seed_base.to_owned()
    } else {
        format!("{seed_base}:{attempt}")
    };
    short_hash(&seed)
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(MISTRAL_TOOL_CALL_ID_LENGTH)
        .collect()
}

// ---------------------------------------------------------------------------
// Error formatting
// ---------------------------------------------------------------------------

/// `truncateErrorText`: cap at `max_chars` UTF-16 code units (JS `slice`).
fn truncate_error_text(text: &str, max_chars: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() <= max_chars {
        return text.to_owned();
    }
    let truncated = String::from_utf16_lossy(&units[..max_chars]);
    format!(
        "{truncated}... [truncated {} chars]",
        units.len() - max_chars
    )
}

/// `formatMistralError`: the native transport's `MistralHttpError.statusCode`
/// / `body` arrive as the HTTP status and raw response body. `fallback` plays
/// the role of the `Error.message` (`statusText || "Request failed with
/// status {statusCode}"` upstream).
fn format_mistral_error(status: Option<u16>, body: Option<&str>, fallback: &str) -> String {
    let body_text = body.map(str::trim).filter(|text| !text.is_empty());
    match (status, body_text) {
        (Some(status), Some(text)) => format!(
            "Mistral API error ({status}): {}",
            truncate_error_text(text, MAX_MISTRAL_ERROR_BODY_CHARS)
        ),
        (Some(status), None) => format!("Mistral API error ({status}): {fallback}"),
        (None, _) => fallback.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Prompt caching (`promptCacheKey` / `x-affinity`)
// ---------------------------------------------------------------------------

/// `shouldUsePromptCaching`: retention `none` disables; a session id is
/// required.
fn should_use_prompt_caching(options: &StreamOptions) -> bool {
    options.cache_retention != Some(CacheRetention::None) && options.session_id.is_some()
}

/// `buildMistralHeaders` (9dd90a497). Mistral infrastructure uses
/// `x-affinity` for KV-cache reuse (prefix caching); an explicit
/// caller-provided value wins, and a `None` (null) override counts as
/// explicit — it suppresses the automatic header
/// (`hasMistralHeaderOverride`, case-insensitive on both sides).
fn build_request_headers(
    model: &Model,
    api_key: &str,
    options: &MistralOptions,
) -> ProviderHeaders {
    let base: ProviderHeaders = [
        ("accept".to_owned(), Some("text/event-stream".to_owned())),
        (
            "authorization".to_owned(),
            Some(format!("Bearer {api_key}")),
        ),
        (
            "content-type".to_owned(),
            Some("application/json".to_owned()),
        ),
    ]
    .into();

    let mut headers = merge_headers_chain(&[
        rpi_user_agent_headers(),
        Some(base),
        model_headers(model),
        options.stream.headers.clone(),
    ]);

    if should_use_prompt_caching(&options.stream)
        && !headers
            .keys()
            .any(|key| key.eq_ignore_ascii_case("x-affinity"))
        && let Some(session_id) = &options.stream.session_id
    {
        headers.insert("x-affinity".to_owned(), Some(session_id.clone()));
    }

    headers
}

// ---------------------------------------------------------------------------
// Payload construction
// ---------------------------------------------------------------------------

/// `buildChatPayload`: builds the camelCase payload that `onPayload`
/// observes; [`to_mistral_wire_payload`] remaps it to snake_case at send
/// time. Note the system prompt rides the leading system message in
/// `messages` (upstream `toChatMessages`), not a separate field (#9548).
fn build_chat_payload(
    model: &Model,
    context: &TranscriptContext,
    messages: &[Message],
    options: &MistralOptions,
) -> Result<Value, String> {
    let supports_images = model.input.contains(&InputModality::Image);
    let chat_messages = to_chat_messages(messages, supports_images);

    let mut payload = Map::new();
    payload.insert("model".to_owned(), json!(model.id));
    payload.insert("stream".to_owned(), json!(true));
    payload.insert("messages".to_owned(), Value::Array(chat_messages));

    let current_tools = crate::utils::transcript::get_current_tools(&context.messages);
    if !current_tools.is_empty() {
        payload.insert(
            "tools".to_owned(),
            Value::Array(to_function_tools(&current_tools)?),
        );
    }
    if let Some(temperature) = options.stream.temperature {
        payload.insert("temperature".to_owned(), json!(temperature));
    }
    if let Some(max_tokens) = options.stream.max_tokens {
        payload.insert("maxTokens".to_owned(), json!(max_tokens));
    }
    if let Some(tool_choice) = &options.tool_choice {
        payload.insert("toolChoice".to_owned(), map_tool_choice(tool_choice));
    }
    if let Some(prompt_mode) = &options.prompt_mode {
        payload.insert("promptMode".to_owned(), json!(prompt_mode.as_str()));
    }
    if let Some(reasoning_effort) = &options.reasoning_effort {
        payload.insert("reasoningEffort".to_owned(), json!(reasoning_effort));
    }
    if should_use_prompt_caching(&options.stream)
        && let Some(session_id) = &options.stream.session_id
    {
        payload.insert("promptCacheKey".to_owned(), json!(session_id));
    }

    Ok(Value::Object(payload))
}

/// `toMistralWirePayload` (9dd90a497): remap the camelCase payload to the
/// snake_case wire format just before sending, including the nested
/// `response_format.json_schema.schemaDefinition` → `schema` remap.
fn to_mistral_wire_payload(payload: &Value) -> Value {
    const REMAPS: [(&str, &str); 12] = [
        ("topP", "top_p"),
        ("maxTokens", "max_tokens"),
        ("randomSeed", "random_seed"),
        ("responseFormat", "response_format"),
        ("toolChoice", "tool_choice"),
        ("presencePenalty", "presence_penalty"),
        ("frequencyPenalty", "frequency_penalty"),
        ("parallelToolCalls", "parallel_tool_calls"),
        ("reasoningEffort", "reasoning_effort"),
        ("promptMode", "prompt_mode"),
        ("promptCacheKey", "prompt_cache_key"),
        ("safePrompt", "safe_prompt"),
    ];
    let Value::Object(object) = payload else {
        return payload.clone();
    };
    let mut wire = object.clone();
    for (source, target) in REMAPS {
        remap_mistral_property(&mut wire, source, target);
    }
    if let Some(messages) = object.get("messages").and_then(Value::as_array) {
        wire.insert(
            "messages".to_owned(),
            Value::Array(messages.iter().map(to_mistral_wire_message).collect()),
        );
    }
    if let Some(Value::Object(mut wire_response_format)) = wire.get("response_format").cloned() {
        remap_mistral_property(&mut wire_response_format, "jsonSchema", "json_schema");
        if let Some(Value::Object(mut wire_json_schema)) =
            wire_response_format.get("json_schema").cloned()
        {
            remap_mistral_property(&mut wire_json_schema, "schemaDefinition", "schema");
            wire_response_format.insert("json_schema".to_owned(), Value::Object(wire_json_schema));
        }
        wire.insert(
            "response_format".to_owned(),
            Value::Object(wire_response_format),
        );
    }
    Value::Object(wire)
}

/// `toMistralWireMessage`.
fn to_mistral_wire_message(message: &Value) -> Value {
    let Value::Object(object) = message else {
        return message.clone();
    };
    let mut wire = object.clone();
    remap_mistral_property(&mut wire, "toolCalls", "tool_calls");
    remap_mistral_property(&mut wire, "toolCallId", "tool_call_id");
    if let Some(content) = object.get("content").and_then(Value::as_array) {
        wire.insert(
            "content".to_owned(),
            Value::Array(content.iter().map(to_mistral_wire_content_chunk).collect()),
        );
    }
    Value::Object(wire)
}

/// `toMistralWireContentChunk`.
fn to_mistral_wire_content_chunk(chunk: &Value) -> Value {
    const REMAPS: [(&str, &str); 6] = [
        ("imageUrl", "image_url"),
        ("documentUrl", "document_url"),
        ("documentName", "document_name"),
        ("fileId", "file_id"),
        ("referenceIds", "reference_ids"),
        ("inputAudio", "input_audio"),
    ];
    let Value::Object(object) = chunk else {
        return chunk.clone();
    };
    let mut wire = object.clone();
    for (source, target) in REMAPS {
        remap_mistral_property(&mut wire, source, target);
    }
    Value::Object(wire)
}

/// `remapMistralProperty`: move `source` to `target` when present.
fn remap_mistral_property(record: &mut Map<String, Value>, source: &str, target: &str) {
    if let Some(value) = record.remove(source) {
        record.insert(target.to_owned(), value);
    }
}

/// `toFunctionTools`. `stripSymbolKeys` is a no-op here: `serde_json::Value`
/// cannot carry TypeBox symbol keys (see module header).
fn to_function_tools(tools: &[Tool]) -> Result<Vec<Value>, String> {
    tools
        .iter()
        .map(|tool| {
            let strict = resolve_json_schema_strict_sampling(tool, true)?;
            Ok(json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters,
                    "strict": strict.unwrap_or(false),
                },
            }))
        })
        .collect()
}

/// `toChatMessages`: user / assistant / tool conversion to Mistral chat
/// messages in the camelCase payload shape (assistant thinking replays as
/// `thinking` chunks; tool results carry `toolCallId` + `name`; the
/// snake_case remap happens in [`to_mistral_wire_payload`]).
fn to_chat_messages(messages: &[Message], supports_images: bool) -> Vec<Value> {
    let mut result: Vec<Value> = Vec::new();

    for (index, msg) in messages.iter().enumerate() {
        match msg {
            // #9548: the leading system message renders its full text; later
            // ones render as framed updates (mistral-conversations.ts:786-790).
            Message::System(system) => {
                let text = if index == 0 {
                    crate::utils::text::get_system_message_text(system)
                } else {
                    crate::utils::text::render_system_message_update(system)
                };
                if !text.is_empty() {
                    result.push(json!({"role": "system", "content": sanitize_surrogates(&text)}));
                }
            }
            Message::User(user) => match &user.content {
                UserContent::Text(text) => {
                    result.push(json!({"role": "user", "content": sanitize_surrogates(text)}));
                }
                UserContent::Blocks(blocks) => {
                    let had_images = blocks
                        .iter()
                        .any(|block| matches!(block, UserContentBlock::Image(_)));
                    let content: Vec<Value> = blocks
                        .iter()
                        .filter(|block| {
                            matches!(block, UserContentBlock::Text(_)) || supports_images
                        })
                        .map(|block| match block {
                            UserContentBlock::Text(text) => json!({
                                "type": "text",
                                "text": sanitize_surrogates(&text.text),
                            }),
                            UserContentBlock::Image(image) => json!({
                                "type": "image_url",
                                "imageUrl": format!("data:{};base64,{}", image.mime_type, image.data),
                            }),
                        })
                        .collect();
                    if !content.is_empty() {
                        result.push(json!({"role": "user", "content": content}));
                        continue;
                    }
                    if had_images && !supports_images {
                        result.push(json!({
                            "role": "user",
                            "content": "(image omitted: model does not support images)",
                        }));
                    }
                }
            },
            Message::Assistant(assistant) => {
                let mut content_parts: Vec<Value> = Vec::new();
                let mut tool_calls: Vec<Value> = Vec::new();

                for block in &assistant.content {
                    match block {
                        AssistantContent::Text(text) if !text.text.trim().is_empty() => {
                            content_parts.push(json!({
                                "type": "text",
                                "text": sanitize_surrogates(&text.text),
                            }));
                        }
                        AssistantContent::Thinking(thinking)
                            if !thinking.thinking.trim().is_empty() =>
                        {
                            content_parts.push(json!({
                                "type": "thinking",
                                "thinking": [{
                                    "type": "text",
                                    "text": sanitize_surrogates(&thinking.thinking),
                                }],
                            }));
                        }
                        AssistantContent::ToolCall(call) => {
                            tool_calls.push(json!({
                                "id": call.id,
                                "type": "function",
                                "function": {
                                    "name": call.name,
                                    "arguments": serde_json::to_string(&call.arguments)
                                        .unwrap_or_else(|_| "{}".to_owned()),
                                },
                                "index": 0,
                            }));
                        }
                        _ => {}
                    }
                }

                if content_parts.is_empty() && tool_calls.is_empty() {
                    continue;
                }
                let mut assistant_message = Map::new();
                assistant_message.insert("role".to_owned(), json!("assistant"));
                assistant_message.insert("prefix".to_owned(), json!(false));
                if !content_parts.is_empty() {
                    assistant_message.insert("content".to_owned(), Value::Array(content_parts));
                }
                if !tool_calls.is_empty() {
                    assistant_message.insert("toolCalls".to_owned(), Value::Array(tool_calls));
                }
                result.push(Value::Object(assistant_message));
            }
            Message::ToolResult(tool_msg) => {
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
                let tool_text = build_tool_result_text(
                    &text_result,
                    has_images,
                    supports_images,
                    tool_msg.is_error,
                );
                let mut tool_content = vec![json!({"type": "text", "text": tool_text})];
                if supports_images {
                    for part in &tool_msg.content {
                        if let ToolResultContent::Image(image) = part {
                            tool_content.push(json!({
                                "type": "image_url",
                                "imageUrl": format!("data:{};base64,{}", image.mime_type, image.data),
                            }));
                        }
                    }
                }
                result.push(json!({
                    "role": "tool",
                    "toolCallId": tool_msg.tool_call_id,
                    "name": tool_msg.tool_name,
                    "content": tool_content,
                }));
            }
        }
    }

    result
}

/// `buildToolResultText`: placeholder text for empty / image-only / error
/// tool results.
fn build_tool_result_text(
    text: &str,
    has_images: bool,
    supports_images: bool,
    is_error: bool,
) -> String {
    let trimmed = text.trim();
    let error_prefix = if is_error { "[tool error] " } else { "" };

    if !trimmed.is_empty() {
        let image_suffix = if has_images && !supports_images {
            "\n[tool image omitted: model does not support images]"
        } else {
            ""
        };
        return format!("{error_prefix}{trimmed}{image_suffix}");
    }

    if has_images {
        if supports_images {
            return if is_error {
                "[tool error] (see attached image)".to_owned()
            } else {
                "(see attached image)".to_owned()
            };
        }
        return if is_error {
            "[tool error] (image omitted: model does not support images)".to_owned()
        } else {
            "(image omitted: model does not support images)".to_owned()
        };
    }

    if is_error {
        "[tool error] (no tool output)".to_owned()
    } else {
        "(no tool output)".to_owned()
    }
}

// ---------------------------------------------------------------------------
// Reasoning selection (promptMode vs reasoningEffort)
// ---------------------------------------------------------------------------

/// `usesReasoningEffort` replacement (#9678 `dc84c1ac0`): models with a
/// thinking level map use `reasoning_effort` with the mapped values; models
/// without one (Magistral) use `prompt_mode`.
fn has_reasoning_effort_map(model: &Model) -> bool {
    model.reasoning && model.thinking_level_map.is_some()
}

/// `mapReasoningEffort`: the model's thinking-level map value, defaulting to
/// `"high"` (upstream `?? "high"` catches both missing and explicit null).
fn map_reasoning_effort(model: &Model, level: ModelThinkingLevel) -> String {
    model
        .thinking_level_map
        .as_ref()
        .and_then(|map| map.get(&level))
        .and_then(|mapped| mapped.clone())
        .unwrap_or_else(|| "high".to_owned())
}

/// `mapToolChoice`.
fn map_tool_choice(choice: &MistralToolChoice) -> Value {
    match choice {
        MistralToolChoice::Auto => json!("auto"),
        MistralToolChoice::None => json!("none"),
        MistralToolChoice::Any => json!("any"),
        MistralToolChoice::Required => json!("required"),
        MistralToolChoice::Function { name } => json!({
            "type": "function",
            "function": { "name": name },
        }),
    }
}

/// `mapChatStopReason` (5a53f086e; text unified by 5a2539a7b). The null case
/// (`reason === null -> "stop"`) is unreachable here: callers only map a
/// present finish reason. Unmapped reasons are provider errors, not stops.
fn map_chat_stop_reason(reason: &str) -> (StopReason, Option<String>) {
    match reason {
        "stop" => (StopReason::Stop, None),
        "length" | "model_length" => (StopReason::Length, None),
        "tool_calls" => (StopReason::ToolUse, None),
        "error" => (
            StopReason::Error,
            // #10487 (7fb59f995): Mistral reports transient server failures
            // this way; "server error" makes the message retryable.
            Some("Provider stopped with: error (server error)".to_owned()),
        ),
        other => (
            StopReason::Error,
            Some(format!("Provider stopped with: {other}")),
        ),
    }
}

// ---------------------------------------------------------------------------
// Cached-token accounting
// ---------------------------------------------------------------------------

/// `getMistralCachedPromptTokens`: reads the cached-token count from any of
/// the six usage field variants the Mistral API has shipped, then clamps to
/// `[0, prompt_tokens]`. The first variant whose leaf exists (non-null) wins —
/// even when it is not a number (JS `??` only falls through on nullish), and
/// a non-finite / non-number leaf counts as 0.
fn get_mistral_cached_prompt_tokens(usage: &Value, prompt_tokens: u64) -> u64 {
    const PATHS: [&[&str]; 6] = [
        &["promptTokensDetails", "cachedTokens"],
        &["prompt_tokens_details", "cached_tokens"],
        &["promptTokenDetails", "cachedTokens"],
        &["prompt_token_details", "cached_tokens"],
        &["numCachedTokens"],
        &["num_cached_tokens"],
    ];
    let raw = PATHS.iter().find_map(|path| {
        let mut current = usage;
        for key in *path {
            current = current.get(*key)?;
        }
        if current.is_null() {
            None
        } else {
            Some(current)
        }
    });
    let cached = raw
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .unwrap_or(0.0);
    cached.clamp(0.0, prompt_tokens as f64) as u64
}

/// `chunk.usage.promptTokens || 0` (and siblings): non-numbers read as 0.
fn usage_number(usage: &Value, key: &str) -> u64 {
    usage
        .get(key)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(0.0) as u64
}

// ---------------------------------------------------------------------------
// SSE wire types
// ---------------------------------------------------------------------------

/// `CompletionChunk` (SDK `CompletionChunk$inboundSchema`, snake_case wire).
/// `usage` stays a JSON value so the six cached-token variants survive.
#[derive(Debug, serde::Deserialize)]
struct CompletionChunk {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    usage: Option<Value>,
    #[serde(default)]
    choices: Vec<CompletionChoice>,
}

/// `CompletionResponseStreamChoice`.
#[derive(Debug, serde::Deserialize)]
struct CompletionChoice {
    #[serde(default)]
    delta: DeltaMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

/// `DeltaMessage` (streaming subset).
#[derive(Debug, Default, serde::Deserialize)]
struct DeltaMessage {
    #[serde(default)]
    content: Option<DeltaContent>,
    #[serde(default)]
    tool_calls: Option<Vec<WireToolCall>>,
}

/// `DeltaMessage.content = string | ContentChunk[]`. Chunks stay JSON values:
/// unknown chunk types are ignored downstream (the SDK's discriminated union
/// produces an `Unknown` entry the adapter likewise skips).
#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum DeltaContent {
    Text(String),
    Chunks(Vec<Value>),
}

/// Tool-call aggregation key — upstream `const key = toolCall.index ?? callId`
/// (mistral-conversations.ts:696, `6c87d9a02` / #8387). Chunks carrying an
/// `index` are keyed by it (Mistral omits the id on continuation chunks, so
/// keying on the id would split one call into several blocks); only chunks
/// with no `index` fall back to the call id. A first chunk keyed by `CallId`
/// followed by an indexed chunk opens a *separate* entry — faithful to the
/// upstream map, not "improved" into a merge.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ToolKey {
    Index(u32),
    CallId(String),
}

/// `ToolCall` (streaming subset). `function.name` / `function.arguments` are
/// required, matching the SDK's zod schema.
#[derive(Debug, serde::Deserialize)]
struct WireToolCall {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    index: Option<u32>,
    function: WireFunctionCall,
}

/// `FunctionCall` (`arguments` is `string | record` on the wire).
#[derive(Debug, serde::Deserialize)]
struct WireFunctionCall {
    name: String,
    arguments: Value,
}

// ---------------------------------------------------------------------------
// Stream processing (`consumeChatStream`)
// ---------------------------------------------------------------------------

/// The current open text/thinking block, by content index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CurrentBlock {
    Text(usize),
    Thinking(usize),
}

/// Outcome of handling one SSE event (the `[DONE]` sentinel ends the stream).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SseOutcome {
    Chunk,
    Done,
}

/// Consumes Mistral SSE chunks and drives the [`StreamEvent`] protocol,
/// accumulating the final assistant message.
struct StreamProcessor<'a> {
    output: &'a mut AssistantMessage,
    model: &'a Model,
    current_block: Option<CurrentBlock>,
    /// Tool-call content index, keyed by `{callId}:{index}`.
    tool_blocks_by_key: HashMap<ToolKey, usize>,
    /// Tool-block content indices in creation order (JS `Map` iteration).
    tool_block_order: Vec<usize>,
    /// `partialArgs` scratch buffer per tool-block content index.
    partial_args: HashMap<usize, String>,
}

impl<'a> StreamProcessor<'a> {
    fn new(output: &'a mut AssistantMessage, model: &'a Model) -> Self {
        Self {
            output,
            model,
            current_block: None,
            tool_blocks_by_key: HashMap::new(),
            tool_block_order: Vec::new(),
            partial_args: HashMap::new(),
        }
    }

    fn block_index(&self) -> usize {
        self.output.content.len() - 1
    }

    /// `finishCurrentBlock`: closes the open text/thinking block.
    fn finish_current_block(&mut self, events: &AssistantMessageEventStream) {
        match self.current_block.take() {
            Some(CurrentBlock::Text(index)) => {
                let text = match &self.output.content[index] {
                    AssistantContent::Text(text) => text.text.clone(),
                    // invariant: Text(index) only indexes text blocks
                    _ => String::new(),
                };
                events.push(StreamEvent::TextEnd {
                    content_index: index,
                    content: text,
                    partial: Arc::new(self.output.clone()),
                });
            }
            Some(CurrentBlock::Thinking(index)) => {
                let thinking = match &self.output.content[index] {
                    AssistantContent::Thinking(thinking) => thinking.thinking.clone(),
                    // invariant: Thinking(index) only indexes thinking blocks
                    _ => String::new(),
                };
                events.push(StreamEvent::ThinkingEnd {
                    content_index: index,
                    content: thinking,
                    partial: Arc::new(self.output.clone()),
                });
            }
            None => {}
        }
    }

    fn open_text_block(&mut self, events: &AssistantMessageEventStream) -> usize {
        if matches!(self.current_block, Some(CurrentBlock::Text(_))) {
            return self.block_index();
        }
        self.finish_current_block(events);
        self.output
            .content
            .push(AssistantContent::Text(Default::default()));
        let index = self.block_index();
        self.current_block = Some(CurrentBlock::Text(index));
        events.push(StreamEvent::TextStart {
            content_index: index,
            partial: Arc::new(self.output.clone()),
        });
        index
    }

    fn open_thinking_block(&mut self, events: &AssistantMessageEventStream) -> usize {
        if matches!(self.current_block, Some(CurrentBlock::Thinking(_))) {
            return self.block_index();
        }
        self.finish_current_block(events);
        self.output
            .content
            .push(AssistantContent::Thinking(Default::default()));
        let index = self.block_index();
        self.current_block = Some(CurrentBlock::Thinking(index));
        events.push(StreamEvent::ThinkingStart {
            content_index: index,
            partial: Arc::new(self.output.clone()),
        });
        index
    }

    fn push_text_delta(&mut self, delta: &str, events: &AssistantMessageEventStream) {
        let index = self.open_text_block(events);
        if let AssistantContent::Text(text) = &mut self.output.content[index] {
            text.text.push_str(delta);
        }
        events.push(StreamEvent::TextDelta {
            content_index: index,
            delta: delta.to_owned(),
            partial: Arc::new(self.output.clone()),
        });
    }

    fn push_thinking_delta(&mut self, delta: &str, events: &AssistantMessageEventStream) {
        let index = self.open_thinking_block(events);
        if let AssistantContent::Thinking(thinking) = &mut self.output.content[index] {
            thinking.thinking.push_str(delta);
        }
        events.push(StreamEvent::ThinkingDelta {
            content_index: index,
            delta: delta.to_owned(),
            partial: Arc::new(self.output.clone()),
        });
    }

    /// `parseMistralEvent` (9dd90a497): empty data is skipped, the `[DONE]`
    /// sentinel ends the stream, the payload must be a record with a
    /// `choices` array (`Invalid Mistral streaming event`), then the strict
    /// chunk decode runs.
    fn handle_sse(
        &mut self,
        sse: &ServerSentEvent,
        events: &AssistantMessageEventStream,
    ) -> Result<SseOutcome, String> {
        let data = sse.data.trim();
        if data.is_empty() {
            return Ok(SseOutcome::Chunk);
        }
        if data == "[DONE]" {
            return Ok(SseOutcome::Done);
        }
        let value: Value = serde_json::from_str(data)
            .map_err(|error| format!("Could not parse Mistral SSE chunk: {error}; data={data}"))?;
        if !value.is_object() || !value.get("choices").is_some_and(Value::is_array) {
            return Err("Invalid Mistral streaming event".to_owned());
        }
        let chunk: CompletionChunk = serde_json::from_value(value)
            .map_err(|error| format!("Could not parse Mistral SSE chunk: {error}; data={data}"))?;
        self.handle_chunk(&chunk, events);
        Ok(SseOutcome::Chunk)
    }

    /// `consumeChatStream` per-chunk body.
    fn handle_chunk(&mut self, chunk: &CompletionChunk, events: &AssistantMessageEventStream) {
        // Keep the first non-empty response id (`output.responseId ||= chunk.id`).
        if self.output.response_id.as_deref().unwrap_or("").is_empty()
            && let Some(id) = chunk.id.as_ref().filter(|id| !id.is_empty())
        {
            self.output.response_id = Some(id.clone());
        }

        if let Some(usage) = &chunk.usage {
            let prompt_tokens = usage_number(usage, "prompt_tokens");
            let cached_prompt_tokens = get_mistral_cached_prompt_tokens(usage, prompt_tokens);

            self.output.usage.input = prompt_tokens.saturating_sub(cached_prompt_tokens);
            self.output.usage.output = usage_number(usage, "completion_tokens");
            self.output.usage.cache_read = cached_prompt_tokens;
            self.output.usage.cache_write = 0;
            let total_tokens = usage_number(usage, "total_tokens");
            self.output.usage.total_tokens = if total_tokens > 0 {
                total_tokens
            } else {
                self.output.usage.input
                    + self.output.usage.output
                    + self.output.usage.cache_read
                    + self.output.usage.cache_write
            };
            calculate_cost(self.model, &mut self.output.usage);
        }

        let Some(choice) = chunk.choices.first() else {
            return;
        };

        if let Some(finish_reason) = choice
            .finish_reason
            .as_deref()
            .filter(|reason| !reason.is_empty())
        {
            // 5a53f086e: preserve the raw provider reason before mapping;
            // unmapped reasons become provider errors with a message.
            self.output.raw_stop_reason = Some(finish_reason.to_owned());
            let (stop_reason, error_message) = map_chat_stop_reason(finish_reason);
            self.output.stop_reason = stop_reason;
            if let Some(error_message) = error_message {
                self.output.error_message = Some(error_message);
            }
        }

        if let Some(content) = &choice.delta.content {
            match content {
                DeltaContent::Text(text) => {
                    let text_delta = sanitize_surrogates(text);
                    // GLM models on Mistral send empty content deltas around
                    // thinking and tool calls; opening a block for them
                    // splits thinking, which Mistral rejects on replay
                    // (#9674 `8930b9ec0`).
                    if !text_delta.is_empty() {
                        self.push_text_delta(text_delta, events);
                    }
                }
                DeltaContent::Chunks(items) => {
                    for item in items {
                        match item.get("type").and_then(Value::as_str) {
                            Some("thinking") => {
                                let delta_text = item
                                    .get("thinking")
                                    .and_then(Value::as_array)
                                    .map(|parts| {
                                        parts
                                            .iter()
                                            .filter_map(|part| {
                                                part.get("text").and_then(Value::as_str)
                                            })
                                            .collect::<Vec<_>>()
                                            .join("")
                                    })
                                    .unwrap_or_default();
                                let thinking_delta = sanitize_surrogates(&delta_text);
                                if thinking_delta.is_empty() {
                                    continue;
                                }
                                self.push_thinking_delta(thinking_delta, events);
                            }
                            Some("text") => {
                                let text_delta = sanitize_surrogates(
                                    item.get("text").and_then(Value::as_str).unwrap_or(""),
                                );
                                if text_delta.is_empty() {
                                    continue;
                                }
                                self.push_text_delta(text_delta, events);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        for tool_call in choice.delta.tool_calls.as_deref().unwrap_or(&[]) {
            self.finish_current_block(events);
            self.handle_tool_call_delta(tool_call, events);
        }
    }

    /// `consumeChatStream` tool-call accumulation.
    fn handle_tool_call_delta(
        &mut self,
        tool_call: &WireToolCall,
        events: &AssistantMessageEventStream,
    ) {
        // `toolCall.index ?? 0` for the fallback id derivation.
        let fallback_index = tool_call.index.unwrap_or(0);
        let call_id = match tool_call
            .id
            .as_deref()
            .filter(|id| !id.is_empty() && *id != "null")
        {
            Some(id) => id.to_owned(),
            None => derive_mistral_tool_call_id(&format!("toolcall:{fallback_index}"), 0),
        };
        // `const key = toolCall.index ?? callId` (6c87d9a02 / #8387): indexed
        // chunks aggregate by index — Mistral omits the id on continuation
        // chunks, so an id-scoped key would split one call into many blocks.
        let key = match tool_call.index {
            Some(index) => ToolKey::Index(index),
            None => ToolKey::CallId(call_id.clone()),
        };

        let content_index = match self.tool_blocks_by_key.get(&key) {
            Some(existing) => *existing,
            None => {
                self.output
                    .content
                    .push(AssistantContent::ToolCall(crate::types::ToolCall {
                        id: call_id,
                        name: tool_call.function.name.clone(),
                        arguments: Map::new(),
                        thought_signature: None,
                        namespace: None,
                    }));
                let content_index = self.output.content.len() - 1;
                self.tool_blocks_by_key.insert(key.clone(), content_index);
                self.tool_block_order.push(content_index);
                events.push(StreamEvent::ToolCallStart {
                    content_index,
                    partial: Arc::new(self.output.clone()),
                });
                content_index
            }
        };

        let args_delta = match &tool_call.function.arguments {
            Value::String(text) => text.clone(),
            // `JSON.stringify(toolCall.function.arguments || {})`.
            Value::Null => "{}".to_owned(),
            other => serde_json::to_string(other).unwrap_or_else(|_| "{}".to_owned()),
        };
        let partial = self.partial_args.entry(content_index).or_default();
        partial.push_str(&args_delta);
        if let AssistantContent::ToolCall(call) = &mut self.output.content[content_index] {
            call.arguments = parse_streaming_json(Some(partial));
        }
        events.push(StreamEvent::ToolCallDelta {
            content_index,
            delta: args_delta,
            partial: Arc::new(self.output.clone()),
        });
    }

    /// `consumeChatStream` tail: close the open block, then finalize each
    /// tool call (final `parseStreamingJson` + `toolcall_end`).
    fn finish(&mut self, events: &AssistantMessageEventStream) {
        self.finish_current_block(events);
        for index in std::mem::take(&mut self.tool_block_order) {
            let Some(partial) = self.partial_args.remove(&index) else {
                continue;
            };
            let arguments = parse_streaming_json(Some(&partial));
            let AssistantContent::ToolCall(call) = &mut self.output.content[index] else {
                continue;
            };
            call.arguments = arguments;
            events.push(StreamEvent::ToolCallEnd {
                content_index: index,
                tool_call: call.clone(),
                partial: Arc::new(self.output.clone()),
            });
        }
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

/// The streaming body: everything that runs inside upstream's async IIFE.
/// Errors return the upstream `Error.message`; `output` carries the partial
/// message either way.
async fn run(
    model: &Model,
    context: &TranscriptContext,
    options: &MistralOptions,
    output: &mut AssistantMessage,
    events: &AssistantMessageEventStream,
) -> Result<DoneReason, String> {
    let api_key = options
        .stream
        .api_key
        .as_deref()
        .ok_or_else(|| format!("No API key for provider: {}", model.provider))?;

    let mut normalizer = MistralToolCallIdNormalizer::default();
    let mut normalize =
        move |id: &str, _model: &Model, _msg: &AssistantMessage| normalizer.normalize(id);
    let transformed_messages = transform_messages(&context.messages, model, Some(&mut normalize));

    let mut payload = build_chat_payload(model, context, &transformed_messages, options)?;
    if let Some(on_payload) = &options.stream.on_payload
        && let Some(next_payload) = on_payload(payload.clone(), model).await
    {
        payload = next_payload;
    }
    // `JSON.stringify(toMistralWirePayload(payload))` (9dd90a497).
    let wire_payload = to_mistral_wire_payload(&payload);
    let body = serde_json::to_string(&wire_payload).map_err(|error| error.to_string())?;

    let headers = build_request_headers(model, api_key, options);
    let url = format!(
        "{}/v1/chat/completions",
        model.base_url.trim_end_matches('/')
    );
    let header_map = provider_headers_to_header_map(&headers)?;
    let mut client_builder =
        crate::api::http_client::adapter_client_builder(options.stream.env.as_ref(), &url)?;
    // Upstream: `AbortSignal.timeout(options?.timeoutMs ?? 60_000)` — a
    // total deadline that aborts even actively streaming bodies. rpi keeps
    // the same budget but applies idle-timeout semantics instead (connect +
    // headers wait + inter-chunk idle; see api::stream_timeouts), so long
    // inferences that keep streaming are not killed by their total
    // duration — an intentional improvement over the upstream quirk.
    let timeout_ms = options
        .stream
        .timeout_ms
        .unwrap_or(DEFAULT_MISTRAL_TIMEOUT_MS);
    client_builder = client_builder.connect_timeout(std::time::Duration::from_millis(timeout_ms));
    let client = client_builder.build().map_err(|error| error.to_string())?;

    let response = retry_provider_request(
        || {
            let request = client
                .post(&url)
                .headers(header_map.clone())
                .body(body.clone());
            let signal = options.stream.signal.clone();
            let fetch = options.stream.fetch.clone();
            async move {
                // 027a58479 (R2.7.4): per-request custom fetch channel; `None`
                // keeps the reqwest default path unchanged.
                let result = send_provider_request(
                    request,
                    fetch.as_ref(),
                    signal.as_ref(),
                    Some(timeout_ms),
                )
                .await;
                match result {
                    Ok(response) => {
                        let status = response.status();
                        if status.is_success() {
                            Ok(response)
                        } else {
                            let status = status.as_u16();
                            // `MistralHttpError`: `statusText || "Request
                            // failed with status {statusCode}"` (reqwest sees
                            // only the standard reason phrase).
                            let fallback = response
                                .status()
                                .canonical_reason()
                                .map(str::to_owned)
                                .unwrap_or_else(|| format!("Request failed with status {status}"));
                            let response_headers = headers_to_record(response.headers());
                            let body = response.text().await.unwrap_or_default();
                            Err(ProviderErrorInfo {
                                status: Some(status),
                                headers: Some(response_headers),
                                message: format_mistral_error(Some(status), Some(&body), &fallback),
                            })
                        }
                    }
                    Err(error) => Err(match error {
                        // The timeout mapping only exists on the reqwest
                        // default path; a custom fetch owns its own timing.
                        SendFailure::Reqwest(error) => ProviderErrorInfo {
                            status: error.status().map(|status| status.as_u16()),
                            headers: None,
                            message: if error.is_timeout() {
                                MISTRAL_TIMEOUT_MESSAGE.to_owned()
                            } else {
                                format_mistral_error(
                                    error.status().map(|status| status.as_u16()),
                                    None,
                                    &crate::utils::transport_error::format_reqwest_error(&error),
                                )
                            },
                        },
                        SendFailure::HeadersTimedOut(_) => ProviderErrorInfo {
                            status: None,
                            headers: None,
                            message: MISTRAL_TIMEOUT_MESSAGE.to_owned(),
                        },
                        other => other.into_provider_error_info(),
                    }),
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
    .await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            // 9dd90a497: `onResponse` observes the HTTP response before the
            // `!response.ok` throw, so it also fires for error statuses.
            if let RetryError::Provider(info) = &error
                && let (Some(on_response), Some(status)) =
                    (&options.stream.on_response, info.status)
            {
                on_response(
                    ProviderResponse {
                        status,
                        headers: info.headers.clone().unwrap_or_default(),
                    },
                    model,
                )
                .await;
            }
            return Err(error.message());
        }
    };

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

    let mut processor = StreamProcessor::new(output, model);
    let mut decoder = SseDecoder::new();
    // Inter-chunk idle timeout (see api::stream_timeouts).
    let mut byte_stream =
        crate::api::stream_timeouts::wrap(response.bytes_stream(), Some(timeout_ms));
    loop {
        // Upstream checks the abort signal around every `reader.read()`;
        // `select!` lets the cancellation fire while waiting for a chunk.
        let next = match &options.stream.signal {
            Some(token) => tokio::select! {
                outcome = byte_stream.next() => outcome,
                () = token.cancelled() => return Err("Request was aborted".to_owned()),
            },
            None => byte_stream.next().await,
        };
        let Some(chunk) = next else { break };
        let bytes = chunk.map_err(|error| match error {
            crate::api::stream_timeouts::IdleStreamError::IdleTimeout { .. } => {
                MISTRAL_TIMEOUT_MESSAGE.to_owned()
            }
            crate::api::stream_timeouts::IdleStreamError::Transport(error) => {
                if error.is_timeout() {
                    MISTRAL_TIMEOUT_MESSAGE.to_owned()
                } else {
                    crate::utils::transport_error::format_reqwest_error(&error)
                }
            }
        })?;
        for sse in decoder.feed(&bytes)? {
            if processor.handle_sse(&sse, events)? == SseOutcome::Done {
                // `[DONE]` terminates the stream (`parseMistralEvent`).
                processor.finish(events);
                return finalize(options, processor.output);
            }
        }
    }
    // P1 (rc.12 review): same as the other SSE adapters — propagate the
    // end-of-stream line-cap failure instead of panicking the task.
    for sse in decoder.finish()? {
        processor.handle_sse(&sse, events)?;
    }
    processor.finish(events);
    finalize(options, processor.output)
}

/// The upstream `stream` tail checks after the chat stream is consumed.
fn finalize(options: &MistralOptions, output: &AssistantMessage) -> Result<DoneReason, String> {
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
        // treated as "stream ended without a usable finish reason".
        StopReason::Pending | StopReason::Deferred => {
            Err("Mistral stream ended without a finish reason".to_owned())
        }
        // 5a53f086e (R2.3.3): carry the mapped provider error message.
        StopReason::Aborted | StopReason::Error => Err(output
            .error_message
            .clone()
            .unwrap_or_else(|| "An unknown error occurred".to_owned())),
        StopReason::Stop => Ok(DoneReason::Stop),
        StopReason::Length => Ok(DoneReason::Length),
        StopReason::ToolUse => Ok(DoneReason::ToolUse),
    }
}

/// `stream` (mistral-conversations).
pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: MistralOptions,
) -> AssistantMessageEventStream {
    let event_stream = AssistantMessageEventStream::new();
    let task_stream = event_stream.clone();
    let model = model.clone();
    // #9548 (mistral-conversations.ts:130).
    let context = crate::utils::transcript::resolve_transcript(
        context,
        model
            .compat
            .as_ref()
            .and_then(|compat| compat.supports_mid_convo_system_messages)
            .unwrap_or(false),
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

/// `streamSimple` (mistral-conversations): maps the provider-agnostic
/// reasoning level to `promptMode` (Magistral-style models) or
/// `reasoningEffort` (Mistral Small 4 / Medium 3.5).
pub fn stream_simple(
    model: &Model,
    context: &TranscriptContext,
    options: Option<SimpleStreamOptions>,
) -> Result<AssistantMessageEventStream, String> {
    // Auth check at the entry, before any stream is constructed
    // (8b5899dce: mistral-conversations.ts streamSimple throws
    // synchronously on a missing key — the Rust equivalent is `Err`).
    let Some(api_key) = options.as_ref().and_then(|o| o.stream.api_key.clone()) else {
        return Err(format!("No API key for provider: {}", model.provider));
    };

    let base = build_base_options(model, context, options.as_ref(), Some(api_key));
    // `base` carries `toolChoice` on every branch upstream
    // (mistral-conversations.ts:193 — rc.12 review: previously dropped).
    let tool_choice = options
        .as_ref()
        .and_then(|o| o.tool_choice)
        .map(|choice| match choice {
            crate::types::SimpleToolChoice::Auto => MistralToolChoice::Auto,
            crate::types::SimpleToolChoice::None => MistralToolChoice::None,
        });
    let reasoning = options
        .as_ref()
        .and_then(|o| o.reasoning)
        .map(|reasoning| clamp_thinking_level(model, reasoning.to_model_level()))
        .filter(|level| *level != ModelThinkingLevel::Off);
    // #9678 (`dc84c1ac0`): models with a thinking level map send
    // `reasoning_effort` (including the model's off value when thinking is
    // off); other reasoning models (Magistral) send `prompt_mode`.
    let reasoning_effort = if has_reasoning_effort_map(model) {
        match reasoning {
            Some(level) => Some(map_reasoning_effort(model, level)),
            None => model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.get(&ModelThinkingLevel::Off))
                .and_then(|mapped| mapped.clone()),
        }
    } else {
        None
    };
    let prompt_mode = (model.reasoning && !has_reasoning_effort_map(model) && reasoning.is_some())
        .then_some(MistralPromptMode::Reasoning);

    Ok(stream(
        model,
        context,
        MistralOptions {
            stream: base,
            tool_choice,
            prompt_mode,
            reasoning_effort,
        },
    ))
}

/// `ProviderStreams` implementation for `ApiKind::MISTRAL_CONVERSATIONS`.
///
/// The trait carries plain [`StreamOptions`]; Mistral-specific extras
/// ([`MistralOptions`]) reach [`stream`] only through direct calls or via
/// [`stream_simple`] reasoning mapping (design §3.3 collapses per-API extras).
#[derive(Debug, Clone, Copy, Default)]
pub struct MistralConversations;

impl ProviderStreams for MistralConversations {
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        stream(
            model,
            context,
            MistralOptions {
                stream: options.unwrap_or_default(),
                ..MistralOptions::default()
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
mod tests {
    use futures::StreamExt;
    use serde_json::json;

    use super::*;
    use crate::api::sse::SseDecoder;
    use crate::types::{ApiKind, ThinkingLevel, ToolCall, ToolResultMessage, ToolResultRole};

    fn make_model(extra: Value) -> Model {
        let mut value = json!({
            "id": "mistral-large-latest", "name": "Large", "api": "mistral-conversations",
            "provider": "mistral", "baseUrl": "https://api.mistral.ai",
            "reasoning": false, "input": ["text"],
            "cost": {"input": 2.0, "output": 6.0, "cacheRead": 0.2, "cacheWrite": 2.0},
            "contextWindow": 128000, "maxTokens": 8192
        });
        value
            .as_object_mut()
            .expect("object")
            .extend(extra.as_object().cloned().unwrap_or_default());
        serde_json::from_value(value).expect("model")
    }

    fn user_text(text: &str) -> Message {
        serde_json::from_value(json!({"role": "user", "content": text, "timestamp": 0}))
            .expect("user")
    }

    fn tool(name: &str, strict: Option<Value>) -> Tool {
        let mut value = json!({
            "name": name, "description": "d",
            "parameters": {"type": "object", "properties": {"x": {"type": "string"}}, "required": ["x"]}
        });
        if let Some(strict) = strict {
            value["constrainedSampling"] = strict;
        }
        serde_json::from_value(value).expect("tool")
    }

    fn tool_result(tool_call_id: &str, content: &str, is_error: bool) -> Message {
        Message::ToolResult(ToolResultMessage {
            role: ToolResultRole::ToolResult,
            tool_call_id: tool_call_id.to_owned(),
            tool_name: "t".to_owned(),
            content: vec![ToolResultContent::Text(crate::types::TextContent {
                text: content.to_owned(),
                text_signature: None,
            })],
            details: None,
            usage: None,
            nested_calls: None,
            is_error,
            timestamp: 0,
            duration_ms: None,
        })
    }

    fn context(
        messages: Vec<Message>,
        tools: Option<Vec<Tool>>,
    ) -> crate::types::TranscriptContext {
        crate::utils::transcript::normalize_context(&crate::types::Context {
            system_prompt: None,
            messages,
            tools,
        })
    }

    fn options() -> MistralOptions {
        MistralOptions {
            stream: StreamOptions {
                request: crate::types::ProviderRequestOptions {
                    api_key: Some("test-key".to_owned()),
                    ..Default::default()
                },
                ..StreamOptions::default()
            },
            ..MistralOptions::default()
        }
    }

    /// Drives the processor with a recorded SSE stream and collects events.
    fn run_processor(model: &Model, sse_payload: &str) -> (Vec<StreamEvent>, AssistantMessage) {
        let events = AssistantMessageEventStream::new();
        let mut output = initial_output(model);
        let mut processor = StreamProcessor::new(&mut output, model);
        let mut decoder = SseDecoder::new();
        for sse in decoder.feed(sse_payload.as_bytes()).unwrap() {
            if processor.handle_sse(&sse, &events).expect("sse") == SseOutcome::Done {
                break;
            }
        }
        for sse in decoder.finish().unwrap() {
            processor.handle_sse(&sse, &events).expect("sse");
        }
        processor.finish(&events);
        events.end(None);
        let collected: Vec<StreamEvent> = futures::executor::block_on(events.collect());
        (collected, output)
    }

    // -----------------------------------------------------------------------
    // Tool-call id normalization
    // -----------------------------------------------------------------------

    #[test]
    fn test_derive_mistral_tool_call_id_passthrough() {
        // Already 9 alphanumeric chars: pass through unchanged.
        assert_eq!(derive_mistral_tool_call_id("abc123XYZ", 0), "abc123XYZ");
    }

    #[test]
    fn test_derive_mistral_tool_call_id_sanitizes_and_hashes() {
        // Special chars stripped; a 9-char result passes through.
        assert_eq!(derive_mistral_tool_call_id("abc 123_X-YZ!", 0), "abc123XYZ");
        // Longer ids hash down to 9 alphanumeric chars, deterministically.
        let derived = derive_mistral_tool_call_id("call_abc123def456", 0);
        assert_eq!(derived.len(), MISTRAL_TOOL_CALL_ID_LENGTH);
        assert!(derived.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_eq!(derived, derive_mistral_tool_call_id("call_abc123def456", 0));
        // Attempt seed changes the hash.
        assert_ne!(
            derive_mistral_tool_call_id("call_abc123def456", 0),
            derive_mistral_tool_call_id("call_abc123def456", 1)
        );
        // All-special-char id falls back to the raw id as the hash seed.
        let derived = derive_mistral_tool_call_id("!!!", 0);
        assert_eq!(derived.len(), MISTRAL_TOOL_CALL_ID_LENGTH);
    }

    #[test]
    fn test_normalizer_stable_mapping_and_collision_resolution() {
        let mut normalizer = MistralToolCallIdNormalizer::default();
        // Same id maps to the same candidate twice.
        let first = normalizer.normalize("call_abc123def456");
        assert_eq!(first, normalizer.normalize("call_abc123def456"));

        // Collision: a second id that derives the same 9-char candidate is
        // retried with the `:1` seed.
        let mut other = normalizer.normalize("abc123XYZ");
        assert_eq!(other, "abc123XYZ");
        other = normalizer.normalize("abc-123-XYZ");
        assert_eq!(other.len(), MISTRAL_TOOL_CALL_ID_LENGTH);
        assert_ne!(other, "abc123XYZ");
        assert_eq!(other, normalizer.normalize("abc-123-XYZ"));
    }

    // -----------------------------------------------------------------------
    // Cached-token variants
    // -----------------------------------------------------------------------

    #[test]
    fn test_get_mistral_cached_prompt_tokens_all_six_variants() {
        let cases = [
            json!({"promptTokensDetails": {"cachedTokens": 4}}),
            json!({"prompt_tokens_details": {"cached_tokens": 4}}),
            json!({"promptTokenDetails": {"cachedTokens": 4}}),
            json!({"prompt_token_details": {"cached_tokens": 4}}),
            json!({"numCachedTokens": 4}),
            json!({"num_cached_tokens": 4}),
        ];
        for usage in cases {
            assert_eq!(get_mistral_cached_prompt_tokens(&usage, 10), 4, "{usage}");
        }
    }

    #[test]
    fn test_get_mistral_cached_prompt_tokens_precedence_and_fallthrough() {
        // First present variant wins.
        let usage = json!({"prompt_tokens_details": {"cached_tokens": 2}, "numCachedTokens": 5});
        assert_eq!(get_mistral_cached_prompt_tokens(&usage, 10), 2);
        // Null leaves fall through to later variants.
        let usage = json!({"promptTokensDetails": null, "num_cached_tokens": 3});
        assert_eq!(get_mistral_cached_prompt_tokens(&usage, 10), 3);
        // A present but non-number leaf does NOT fall through (JS `??`).
        let usage = json!({"promptTokensDetails": {"cachedTokens": "abc"}, "numCachedTokens": 5});
        assert_eq!(get_mistral_cached_prompt_tokens(&usage, 10), 0);
        // Missing entirely: 0.
        assert_eq!(get_mistral_cached_prompt_tokens(&json!({}), 10), 0);
    }

    #[test]
    fn test_get_mistral_cached_prompt_tokens_clamped() {
        // Clamped to [0, promptTokens].
        let usage = json!({"numCachedTokens": 99});
        assert_eq!(get_mistral_cached_prompt_tokens(&usage, 10), 10);
        let usage = json!({"numCachedTokens": -5});
        assert_eq!(get_mistral_cached_prompt_tokens(&usage, 10), 0);
    }

    // -----------------------------------------------------------------------
    // Error formatting
    // -----------------------------------------------------------------------

    #[test]
    fn test_format_mistral_error() {
        assert_eq!(
            format_mistral_error(Some(400), Some("  bad request  "), "fallback"),
            "Mistral API error (400): bad request"
        );
        assert_eq!(
            format_mistral_error(Some(500), Some("   "), "Request failed with status 500"),
            "Mistral API error (500): Request failed with status 500"
        );
        assert_eq!(
            format_mistral_error(None, None, "connection refused"),
            "connection refused"
        );
    }

    #[test]
    fn test_truncate_error_text() {
        let short = "short";
        assert_eq!(truncate_error_text(short, 10), "short");
        let long = "x".repeat(4100);
        let truncated = truncate_error_text(&long, MAX_MISTRAL_ERROR_BODY_CHARS);
        assert!(truncated.starts_with(&"x".repeat(MAX_MISTRAL_ERROR_BODY_CHARS)));
        assert!(truncated.ends_with("... [truncated 100 chars]"));
    }

    // -----------------------------------------------------------------------
    // Prompt caching
    // -----------------------------------------------------------------------

    #[test]
    fn test_should_use_prompt_caching() {
        let mut stream = StreamOptions::default();
        assert!(!should_use_prompt_caching(&stream)); // no session id
        stream.session_id = Some("s".to_owned());
        assert!(should_use_prompt_caching(&stream)); // unset retention: cache on
        stream.cache_retention = Some(CacheRetention::Short);
        assert!(should_use_prompt_caching(&stream));
        stream.cache_retention = Some(CacheRetention::None);
        assert!(!should_use_prompt_caching(&stream));
    }

    #[test]
    fn test_build_request_headers_x_affinity() {
        let model = make_model(json!({}));
        let mut opts = options();
        opts.stream.session_id = Some("session-123".to_owned());
        let headers = build_request_headers(&model, "test-key", &opts);
        assert_eq!(
            headers.get("x-affinity").and_then(|v| v.as_deref()),
            Some("session-123")
        );
        assert_eq!(
            headers.get("authorization").and_then(|v| v.as_deref()),
            Some("Bearer test-key")
        );
        // 9dd90a497: the native transport sets content-type explicitly.
        assert_eq!(
            headers.get("content-type").and_then(|v| v.as_deref()),
            Some("application/json")
        );

        // Caller-provided value wins (case-insensitively).
        opts.stream.headers = Some([("X-Affinity".to_owned(), Some("custom".to_owned()))].into());
        let headers = build_request_headers(&model, "test-key", &opts);
        assert_eq!(
            headers.get("X-Affinity").and_then(|v| v.as_deref()),
            Some("custom")
        );
        assert!(!headers.contains_key("x-affinity"));

        // Retention none: no x-affinity.
        opts.stream.headers = None;
        opts.stream.cache_retention = Some(CacheRetention::None);
        let headers = build_request_headers(&model, "test-key", &opts);
        assert!(!headers.contains_key("x-affinity"));
    }

    // -----------------------------------------------------------------------
    // Payload construction
    // -----------------------------------------------------------------------

    #[test]
    fn test_build_chat_payload_prompt_cache_key() {
        let model = make_model(json!({}));
        let ctx = context(vec![user_text("hi")], None);

        let mut opts = options();
        opts.stream.session_id = Some("session-123".to_owned());
        let payload =
            build_chat_payload(&model, &ctx, &ctx.messages.clone(), &opts).expect("payload");
        // 9dd90a497: the payload is camelCase until `toMistralWirePayload`.
        assert_eq!(payload["promptCacheKey"], json!("session-123"));

        // Cache retention disabled: key omitted.
        opts.stream.cache_retention = Some(CacheRetention::None);
        let payload =
            build_chat_payload(&model, &ctx, &ctx.messages.clone(), &opts).expect("payload");
        assert!(payload.get("promptCacheKey").is_none());
    }

    #[test]
    fn test_build_chat_payload_system_prompt_prepended() {
        let model = make_model(json!({}));
        let ctx = crate::utils::transcript::normalize_context(&crate::types::Context {
            system_prompt: Some("be nice".to_owned()),
            messages: vec![user_text("hi")],
            tools: None,
        });
        // #9548: the prompt rides the leading system message in
        // (toChatMessages renders it; no separate unshift).
        let payload =
            build_chat_payload(&model, &ctx, &ctx.messages.clone(), &options()).expect("payload");
        let messages = payload["messages"].as_array().expect("messages");
        assert_eq!(messages[0], json!({"role": "system", "content": "be nice"}));
        assert_eq!(messages[1], json!({"role": "user", "content": "hi"}));
        assert_eq!(payload["model"], json!("mistral-large-latest"));
        assert_eq!(payload["stream"], json!(true));
    }

    // -----------------------------------------------------------------------
    // Wire payload remap (toMistralWirePayload, 9dd90a497)
    // -----------------------------------------------------------------------

    #[test]
    fn test_to_mistral_wire_payload_remaps_camel_case_keys() {
        // mistral-http-transport.test.ts "serializes SDK-style payloads to
        // the Mistral wire format": every known camelCase key is remapped,
        // the source key deleted, unknown keys pass through.
        let payload = json!({
            "model": "mistral-large-latest",
            "stream": true,
            "messages": [],
            "temperature": 0.5,
            "topP": 0.9,
            "maxTokens": 123,
            "randomSeed": 42,
            "toolChoice": {"type": "function", "function": {"name": "lookup"}},
            "presencePenalty": 0.1,
            "frequencyPenalty": 0.2,
            "parallelToolCalls": true,
            "reasoningEffort": "high",
            "promptMode": "reasoning",
            "promptCacheKey": "session-1",
            "safePrompt": true,
            "unknownKey": {"nested": true},
        });
        let wire = to_mistral_wire_payload(&payload);
        assert_eq!(wire["top_p"], json!(0.9));
        assert_eq!(wire["max_tokens"], json!(123));
        assert_eq!(wire["random_seed"], json!(42));
        assert_eq!(
            wire["tool_choice"],
            json!({"type": "function", "function": {"name": "lookup"}})
        );
        assert_eq!(wire["presence_penalty"], json!(0.1));
        assert_eq!(wire["frequency_penalty"], json!(0.2));
        assert_eq!(wire["parallel_tool_calls"], json!(true));
        assert_eq!(wire["reasoning_effort"], json!("high"));
        assert_eq!(wire["prompt_mode"], json!("reasoning"));
        assert_eq!(wire["prompt_cache_key"], json!("session-1"));
        assert_eq!(wire["safe_prompt"], json!(true));
        // Already-snake / unknown keys pass through untouched.
        assert_eq!(wire["temperature"], json!(0.5));
        assert_eq!(wire["unknownKey"], json!({"nested": true}));
        for source in [
            "topP",
            "maxTokens",
            "randomSeed",
            "toolChoice",
            "presencePenalty",
            "frequencyPenalty",
            "parallelToolCalls",
            "reasoningEffort",
            "promptMode",
            "promptCacheKey",
            "safePrompt",
        ] {
            assert!(wire.get(source).is_none(), "{source} must be deleted");
        }
    }

    #[test]
    fn test_to_mistral_wire_payload_nested_response_format() {
        // mistral-http-transport.test.ts: `responseFormat.jsonSchema.
        // schemaDefinition` remaps to `response_format.json_schema.schema`;
        // the user-supplied schema body keeps its own key casing.
        let payload = json!({
            "messages": [],
            "responseFormat": {
                "type": "json_schema",
                "jsonSchema": {
                    "name": "result",
                    "schemaDefinition": {
                        "type": "object",
                        "properties": {"maxTokens": {"type": "number"}},
                    },
                },
            },
        });
        let wire = to_mistral_wire_payload(&payload);
        assert_eq!(
            wire["response_format"],
            json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "result",
                    "schema": {
                        "type": "object",
                        "properties": {"maxTokens": {"type": "number"}},
                    },
                },
            })
        );
        assert!(wire.get("responseFormat").is_none());
    }

    #[test]
    fn test_to_mistral_wire_message_and_content_chunk() {
        // `toolCalls`/`toolCallId` and the content-chunk keys remap; other
        // fields (role, name, prefix, index) pass through.
        let payload = json!({
            "messages": [
                {
                    "role": "assistant",
                    "prefix": false,
                    "content": [
                        {"type": "text", "text": "answer"},
                        {"type": "image_url", "imageUrl": "data:image/png;base64,aGVsbG8="},
                    ],
                    "toolCalls": [{
                        "id": "abc123456",
                        "type": "function",
                        "function": {"name": "lookup", "arguments": "{}"},
                        "index": 0,
                    }],
                },
                {
                    "role": "tool",
                    "toolCallId": "abc123456",
                    "name": "lookup",
                    "content": [
                        {"type": "text", "text": "found"},
                        {"type": "image_url", "imageUrl": "data:image/png;base64,aGVsbG8="},
                    ],
                },
            ],
        });
        let wire = to_mistral_wire_payload(&payload);
        assert_eq!(
            wire["messages"],
            json!([
                {
                    "role": "assistant",
                    "prefix": false,
                    "content": [
                        {"type": "text", "text": "answer"},
                        {"type": "image_url", "image_url": "data:image/png;base64,aGVsbG8="},
                    ],
                    "tool_calls": [{
                        "id": "abc123456",
                        "type": "function",
                        "function": {"name": "lookup", "arguments": "{}"},
                        "index": 0,
                    }],
                },
                {
                    "role": "tool",
                    "tool_call_id": "abc123456",
                    "name": "lookup",
                    "content": [
                        {"type": "text", "text": "found"},
                        {"type": "image_url", "image_url": "data:image/png;base64,aGVsbG8="},
                    ],
                },
            ])
        );
    }

    #[test]
    fn test_to_mistral_wire_content_chunk_extra_keys() {
        // The remaining four remap pairs (`toMistralWireContentChunk`).
        let chunk = json!({
            "type": "document_url",
            "documentUrl": "https://example.com/d.pdf",
            "documentName": "d.pdf",
            "fileId": "file-1",
            "referenceIds": [1, 2],
            "inputAudio": "aXVkaW8=",
        });
        let wire = to_mistral_wire_content_chunk(&chunk);
        assert_eq!(wire["document_url"], json!("https://example.com/d.pdf"));
        assert_eq!(wire["document_name"], json!("d.pdf"));
        assert_eq!(wire["file_id"], json!("file-1"));
        assert_eq!(wire["reference_ids"], json!([1, 2]));
        assert_eq!(wire["input_audio"], json!("aXVkaW8="));
        for source in [
            "documentUrl",
            "documentName",
            "fileId",
            "referenceIds",
            "inputAudio",
        ] {
            assert!(wire.get(source).is_none(), "{source} must be deleted");
        }
    }

    #[test]
    fn test_to_function_tools_strict() {
        // mistral-tool-schema.test.ts intent: strict sampling request yields
        // `"strict": true` on the function tool; default tools get `false`.
        let tools = vec![
            tool("plain", None),
            tool(
                "strict_tool",
                Some(json!({"type": "json_schema", "strict": "require"})),
            ),
        ];
        let converted = to_function_tools(&tools).expect("tools");
        assert_eq!(converted[0]["function"]["strict"], json!(false));
        assert_eq!(converted[1]["function"]["strict"], json!(true));
        assert_eq!(
            converted[1]["function"]["parameters"]["type"],
            json!("object")
        );
        assert_eq!(
            converted[1]["function"]["parameters"]["properties"]["x"]["type"],
            json!("string")
        );
    }

    #[test]
    fn test_map_tool_choice() {
        assert_eq!(map_tool_choice(&MistralToolChoice::Auto), json!("auto"));
        assert_eq!(
            map_tool_choice(&MistralToolChoice::Required),
            json!("required")
        );
        assert_eq!(
            map_tool_choice(&MistralToolChoice::Function {
                name: "bash".to_owned()
            }),
            json!({"type": "function", "function": {"name": "bash"}})
        );
    }

    #[test]
    fn test_map_chat_stop_reason() {
        assert_eq!(map_chat_stop_reason("stop"), (StopReason::Stop, None));
        assert_eq!(map_chat_stop_reason("length"), (StopReason::Length, None));
        assert_eq!(
            map_chat_stop_reason("model_length"),
            (StopReason::Length, None)
        );
        assert_eq!(
            map_chat_stop_reason("tool_calls"),
            (StopReason::ToolUse, None)
        );
        assert_eq!(
            map_chat_stop_reason("error"),
            (
                StopReason::Error,
                Some("Provider stopped with: error (server error)".to_owned())
            )
        );
        // 5a53f086e: unmapped reasons are provider errors, not stops.
        assert_eq!(
            map_chat_stop_reason("something_new"),
            (
                StopReason::Error,
                Some("Provider stopped with: something_new".to_owned())
            )
        );
    }

    // -----------------------------------------------------------------------
    // Reasoning selection (mistral-reasoning-mode.test.ts intents)
    // -----------------------------------------------------------------------

    fn reasoning_model(id: &str) -> Model {
        make_model(json!({"id": id, "reasoning": true}))
    }

    #[test]
    fn test_reasoning_mode_selection() {
        // #9678 (`dc84c1ac0`): a thinking level map decides between
        // reasoning_effort and prompt_mode — no hardcoded model ids.
        let small = make_model(json!({
            "id": "mistral-small-2603",
            "reasoning": true,
            "thinkingLevelMap": {"off": "none", "high": "high"}
        }));
        assert!(has_reasoning_effort_map(&small));
        let magistral = reasoning_model("magistral-medium-latest");
        assert!(!has_reasoning_effort_map(&magistral));
        let non_reasoning = make_model(json!({}));
        assert!(!has_reasoning_effort_map(&non_reasoning));
    }

    #[test]
    fn test_map_reasoning_effort_defaults_high() {
        let model = reasoning_model("mistral-small-2603");
        assert_eq!(
            map_reasoning_effort(&model, ModelThinkingLevel::Medium),
            "high"
        );
        // thinkingLevelMap value passes through verbatim.
        let model = make_model(json!({
            "id": "mistral-small-2603", "reasoning": true,
            "thinkingLevelMap": {"medium": "none"}
        }));
        assert_eq!(
            map_reasoning_effort(&model, ModelThinkingLevel::Medium),
            "none"
        );
        // Explicit null maps to the "high" default (JS `?? "high"`).
        let model = make_model(json!({
            "id": "mistral-small-2603", "reasoning": true,
            "thinkingLevelMap": {"medium": null}
        }));
        assert_eq!(
            map_reasoning_effort(&model, ModelThinkingLevel::Medium),
            "high"
        );
    }

    // -----------------------------------------------------------------------
    // Message conversion
    // -----------------------------------------------------------------------

    #[test]
    fn test_to_chat_messages_assistant_thinking_and_tool_calls() {
        let assistant = Message::Assistant(AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![
                AssistantContent::Thinking(crate::types::ThinkingContent {
                    thinking: "deep thought".to_owned(),
                    thinking_signature: None,
                    redacted: None,
                }),
                AssistantContent::Text(crate::types::TextContent {
                    text: "answer".to_owned(),
                    text_signature: None,
                }),
                AssistantContent::ToolCall(ToolCall {
                    id: "abc123XYZ".to_owned(),
                    name: "bash".to_owned(),
                    arguments: serde_json::from_value(json!({"cmd": "ls"})).expect("map"),
                    thought_signature: None,
                    namespace: None,
                }),
            ],
            api: ApiKind::from(ApiKind::MISTRAL_CONVERSATIONS),
            provider: "mistral".to_owned(),
            model: "mistral-large-latest".to_owned(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::ToolUse,
            error_message: None,
            timestamp: 0,
            deferred: None,
            end_turn: None,
            raw_stop_reason: None,
            duration_ms: None,
        });
        let converted = to_chat_messages(&[assistant], false);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0]["prefix"], json!(false));
        assert_eq!(
            converted[0]["content"],
            json!([
                {"type": "thinking", "thinking": [{"type": "text", "text": "deep thought"}]},
                {"type": "text", "text": "answer"},
            ])
        );
        // 9dd90a497: camelCase `toolCalls` with a wire `index: 0` entry.
        assert_eq!(
            converted[0]["toolCalls"],
            json!([{
                "id": "abc123XYZ",
                "type": "function",
                "function": {"name": "bash", "arguments": "{\"cmd\":\"ls\"}"},
                "index": 0,
            }])
        );

        // Empty assistant messages are skipped.
        let empty = Message::Assistant(AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![],
            api: ApiKind::from(ApiKind::MISTRAL_CONVERSATIONS),
            provider: "mistral".to_owned(),
            model: "mistral-large-latest".to_owned(),
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
        });
        assert!(to_chat_messages(&[empty], false).is_empty());
    }

    #[test]
    fn test_to_chat_messages_tool_result() {
        let converted = to_chat_messages(&[tool_result("id1", "result body", false)], false);
        assert_eq!(
            converted[0],
            json!({
                "role": "tool",
                "toolCallId": "id1",
                "name": "t",
                "content": [{"type": "text", "text": "result body"}],
            })
        );
        let converted = to_chat_messages(&[tool_result("id1", "", true)], false);
        assert_eq!(
            converted[0]["content"][0],
            json!({"type": "text", "text": "[tool error] (no tool output)"})
        );
    }

    #[test]
    fn test_build_tool_result_text() {
        assert_eq!(build_tool_result_text("ok", false, false, false), "ok");
        assert_eq!(
            build_tool_result_text("ok", true, false, false),
            "ok\n[tool image omitted: model does not support images]"
        );
        assert_eq!(build_tool_result_text("ok", true, true, false), "ok");
        assert_eq!(
            build_tool_result_text("", false, false, false),
            "(no tool output)"
        );
        assert_eq!(
            build_tool_result_text("", false, false, true),
            "[tool error] (no tool output)"
        );
        assert_eq!(
            build_tool_result_text("", true, true, false),
            "(see attached image)"
        );
        assert_eq!(
            build_tool_result_text("", true, true, true),
            "[tool error] (see attached image)"
        );
        assert_eq!(
            build_tool_result_text("", true, false, false),
            "(image omitted: model does not support images)"
        );
        assert_eq!(
            build_tool_result_text("", true, false, true),
            "[tool error] (image omitted: model does not support images)"
        );
    }

    // -----------------------------------------------------------------------
    // Stream processing
    // -----------------------------------------------------------------------

    /// Drives `handle_sse` over one raw SSE payload; returns the error if any.
    fn run_handle_sse(model: &Model, sse_payload: &str) -> Result<(), String> {
        let events = AssistantMessageEventStream::new();
        let mut output = initial_output(model);
        let mut processor = StreamProcessor::new(&mut output, model);
        let mut decoder = SseDecoder::new();
        for sse in decoder.feed(sse_payload.as_bytes()).unwrap() {
            processor.handle_sse(&sse, &events)?;
        }
        for sse in decoder.finish().unwrap() {
            processor.handle_sse(&sse, &events)?;
        }
        Ok(())
    }

    #[test]
    fn test_handle_sse_skips_empty_data_events() {
        // 9dd90a497 `parseMistralEvent`: events whose joined `data:` payload
        // is empty are skipped, not parsed.
        let model = make_model(json!({}));
        run_handle_sse(&model, "data:\n\nevent: ping\n\ndata:   \n\n").expect("empty events");
    }

    /// [`run_handle_sse`] that returns the accumulated assistant message.
    fn run_handle_sse_output(model: &Model, sse_payload: &str) -> AssistantMessage {
        let events = AssistantMessageEventStream::new();
        let mut output = initial_output(model);
        let mut processor = StreamProcessor::new(&mut output, model);
        let mut decoder = SseDecoder::new();
        for sse in decoder.feed(sse_payload.as_bytes()).expect("decode") {
            processor.handle_sse(&sse, &events).expect("handle sse");
        }
        for sse in decoder.finish().expect("decode") {
            processor.handle_sse(&sse, &events).expect("handle sse");
        }
        output
    }

    /// #9674 (`8930b9ec0`): GLM models on Mistral send empty content deltas
    /// around thinking and tool calls; they must not open empty text blocks
    /// or split thinking into two blocks.
    #[test]
    fn test_handle_sse_ignores_empty_content_deltas() {
        let model = make_model(json!({"id": "zai-glm-5-3"}));
        let thinking =
            |text: &str| json!({"type": "thinking", "thinking": [{"type": "text", "text": text}]});
        let tool_call = |args: &str, first: bool| {
            let mut call = json!({
                "index": 0,
                "function": {"name": if first { "read" } else { "" }, "arguments": args}
            });
            if first {
                call["id"] = json!("abc123456");
            }
            call
        };
        let deltas = [
            json!({"content": ""}),
            json!({"content": [thinking("first part,")]}),
            json!({"content": ""}),
            json!({"content": [{"type": "text", "text": ""}]}),
            json!({"content": [thinking(" second part."), {"type": "text", "text": "Reading."}]}),
            json!({"content": "", "tool_calls": [tool_call("", true)]}),
            json!({"content": "", "tool_calls": [tool_call("{\"path\":", false)]}),
            json!({"content": "", "tool_calls": [tool_call("\"a.txt\"}", false)]}),
            json!({"content": ""}),
        ];
        let mut payload = String::new();
        for (index, delta) in deltas.iter().enumerate() {
            let finish = if index == deltas.len() - 1 {
                json!("tool_calls")
            } else {
                Value::Null
            };
            let event = json!({
                "id": "response-1",
                "model": model.id,
                "choices": [{"index": 0, "finish_reason": finish, "delta": delta}],
            });
            payload.push_str(&format!("data: {event}\n\n"));
        }
        let output = run_handle_sse_output(&model, &payload);
        let thinking_texts: Vec<String> = output
            .content
            .iter()
            .filter_map(|block| match block {
                AssistantContent::Thinking(thinking) => Some(thinking.thinking.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(thinking_texts, vec!["first part, second part."]);
        let text_texts: Vec<String> = output
            .content
            .iter()
            .filter_map(|block| match block {
                AssistantContent::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(text_texts, vec!["Reading."]);
        let tool_calls: Vec<&ToolCall> = output
            .content
            .iter()
            .filter_map(|block| match block {
                AssistantContent::ToolCall(call) => Some(call),
                _ => None,
            })
            .collect();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].name, "read");
        assert_eq!(
            Value::Object(tool_calls[0].arguments.clone()),
            json!({"path": "a.txt"})
        );
    }

    #[test]
    fn test_handle_sse_multiline_data_joined() {
        // Multi-line `data:` payloads join with `\n` before parsing.
        let model = make_model(json!({}));
        run_handle_sse(
            &model,
            "data: {\"id\":\"cmpl-1\",\ndata: \"choices\":[]}\n\ndata: [DONE]\n\n",
        )
        .expect("multiline data");
    }

    #[test]
    fn test_handle_sse_rejects_payload_without_choices() {
        // 9dd90a497 `parseMistralEvent`: non-record payloads or records
        // without a `choices` array fail with this exact message.
        let model = make_model(json!({}));
        for payload in [
            "data: {\"id\":\"cmpl-1\"}\n\n",
            "data: {\"id\":\"cmpl-1\",\"choices\":{}}\n\n",
            "data: 42\n\n",
            "data: [1,2]\n\n",
        ] {
            let error = run_handle_sse(&model, payload).expect_err("invalid event");
            assert_eq!(error, "Invalid Mistral streaming event", "{payload}");
        }
    }

    #[test]
    fn test_handle_sse_malformed_json_names_the_data() {
        // rpi deviation (D-021): strict serde decode names the serde error
        // and the offending payload where upstream surfaces the raw
        // `JSON.parse` SyntaxError text.
        let model = make_model(json!({}));
        let error = run_handle_sse(&model, "data: {oops\n\n").expect_err("malformed");
        assert!(
            error.starts_with("Could not parse Mistral SSE chunk: "),
            "{error}"
        );
        assert!(error.ends_with("; data={oops"), "{error}");
    }

    #[test]
    fn test_processor_text_stream_with_cached_usage() {
        let sse = concat!(
            "data: {\"id\":\"cmpl-1\",\"model\":\"mistral-large-latest\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hello\"},\"finish_reason\":null}]}\n",
            "\n",
            "data: {\"id\":\"cmpl-1\",\"model\":\"mistral-large-latest\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n",
            "\n",
            "data: {\"id\":\"cmpl-1\",\"model\":\"mistral-large-latest\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15,\"prompt_tokens_details\":{\"cached_tokens\":4}}}\n",
            "\n",
            "data: [DONE]\n",
            "\n",
        );
        let model = make_model(json!({}));
        let (events, output) = run_processor(&model, sse);
        let kinds: Vec<&str> = events
            .iter()
            .map(|event| match event {
                StreamEvent::TextStart { .. } => "text_start",
                StreamEvent::TextDelta { .. } => "text_delta",
                StreamEvent::TextEnd { .. } => "text_end",
                _ => "other",
            })
            .collect();
        assert_eq!(
            kinds,
            vec!["text_start", "text_delta", "text_delta", "text_end"]
        );
        assert_eq!(output.response_id.as_deref(), Some("cmpl-1"));
        assert_eq!(output.stop_reason, StopReason::Stop);
        // Cached tokens split out of input; total from the wire.
        assert_eq!(output.usage.input, 6);
        assert_eq!(output.usage.cache_read, 4);
        assert_eq!(output.usage.output, 5);
        assert_eq!(output.usage.total_tokens, 15);
    }

    #[test]
    fn test_processor_thinking_stream() {
        let sse = concat!(
            "data: {\"id\":\"cmpl-1\",\"model\":\"magistral-medium-latest\",\"choices\":[{\"index\":0,\"delta\":{\"content\":[{\"type\":\"thinking\",\"thinking\":[{\"type\":\"text\",\"text\":\"let me \"},{\"type\":\"reference\",\"reference_ids\":[1]}]}]},\"finish_reason\":null}]}\n",
            "\n",
            "data: {\"id\":\"cmpl-1\",\"model\":\"magistral-medium-latest\",\"choices\":[{\"index\":0,\"delta\":{\"content\":[{\"type\":\"thinking\",\"thinking\":[{\"type\":\"text\",\"text\":\"think\"}]}]},\"finish_reason\":null}]}\n",
            "\n",
            "data: {\"id\":\"cmpl-1\",\"model\":\"magistral-medium-latest\",\"choices\":[{\"index\":0,\"delta\":{\"content\":[{\"type\":\"text\",\"text\":\"answer\"}]},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5}}\n",
            "\n",
            "data: [DONE]\n",
            "\n",
        );
        let model = reasoning_model("magistral-medium-latest");
        let (events, output) = run_processor(&model, sse);
        let kinds: Vec<&str> = events
            .iter()
            .map(|event| match event {
                StreamEvent::ThinkingStart { .. } => "thinking_start",
                StreamEvent::ThinkingDelta { .. } => "thinking_delta",
                StreamEvent::ThinkingEnd { .. } => "thinking_end",
                StreamEvent::TextStart { .. } => "text_start",
                StreamEvent::TextDelta { .. } => "text_delta",
                StreamEvent::TextEnd { .. } => "text_end",
                _ => "other",
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "thinking_start",
                "thinking_delta",
                "thinking_delta",
                "thinking_end",
                "text_start",
                "text_delta",
                "text_end"
            ]
        );
        match &output.content[0] {
            AssistantContent::Thinking(thinking) => {
                // Reference chunks contribute no text (upstream `"text" in part`).
                assert_eq!(thinking.thinking, "let me think");
            }
            other => panic!("expected thinking block, got {other:?}"),
        }
    }

    #[test]
    fn test_processor_tool_call_stream_derives_missing_id() {
        // The first chunk carries a null/absent id: derived from the index.
        let sse = concat!(
            "data: {\"id\":\"cmpl-1\",\"model\":\"mistral-large-latest\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"null\",\"type\":\"function\",\"function\":{\"name\":\"bash\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n",
            "\n",
            "data: {\"id\":\"cmpl-1\",\"model\":\"mistral-large-latest\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"bash\",\"arguments\":\"{\\\"cmd\\\":\"}}]},\"finish_reason\":null}]}\n",
            "\n",
            "data: {\"id\":\"cmpl-1\",\"model\":\"mistral-large-latest\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"bash\",\"arguments\":\"\\\"ls\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15}}\n",
            "\n",
            "data: [DONE]\n",
            "\n",
        );
        let model = make_model(json!({}));
        let (events, output) = run_processor(&model, sse);
        let kinds: Vec<&str> = events
            .iter()
            .map(|event| match event {
                StreamEvent::ToolCallStart { .. } => "toolcall_start",
                StreamEvent::ToolCallDelta { .. } => "toolcall_delta",
                StreamEvent::ToolCallEnd { .. } => "toolcall_end",
                _ => "other",
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "toolcall_start",
                "toolcall_delta",
                "toolcall_delta",
                "toolcall_delta",
                "toolcall_end"
            ]
        );
        assert_eq!(output.stop_reason, StopReason::ToolUse);
        let AssistantContent::ToolCall(call) = &output.content[0] else {
            panic!("expected tool call block");
        };
        // `"null"` id → derived 9-char alphanumeric id (`toolcall:0` seed).
        assert_eq!(call.id, derive_mistral_tool_call_id("toolcall:0", 0));
        assert_eq!(call.id.len(), MISTRAL_TOOL_CALL_ID_LENGTH);
        assert_eq!(call.name, "bash");
        assert_eq!(Value::Object(call.arguments.clone()), json!({"cmd": "ls"}));
        // The toolcall_end event carries the finalized call.
        let StreamEvent::ToolCallEnd { tool_call, .. } = events.last().expect("end") else {
            panic!("expected toolcall_end");
        };
        assert_eq!(tool_call.id, call.id);
    }

    #[test]
    fn test_processor_num_cached_tokens_variant() {
        let sse = concat!(
            "data: {\"id\":\"cmpl-1\",\"model\":\"mistral-large-latest\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"num_cached_tokens\":99}}\n",
            "\n",
            "data: [DONE]\n",
            "\n",
        );
        let model = make_model(json!({}));
        let (_, output) = run_processor(&model, sse);
        // Clamped to promptTokens; total falls back to the component sum
        // (wire `total_tokens` absent).
        assert_eq!(output.usage.input, 0);
        assert_eq!(output.usage.cache_read, 10);
        assert_eq!(output.usage.total_tokens, 15);
    }

    // -----------------------------------------------------------------------
    // stream_simple mapping
    // -----------------------------------------------------------------------

    /// 8b5899dce: missing request auth fails synchronously (types.ts:325-330).
    #[test]
    fn test_stream_simple_requires_api_key() {
        let model = make_model(json!({}));
        assert_eq!(
            stream_simple(&model, &context(vec![], None), None).err(),
            Some("No API key for provider: mistral".to_owned())
        );
    }

    /// Captures the payload stream_simple builds, via an unreachable server
    /// (the upstream mistral-reasoning-mode.test.ts technique). The captured
    /// payload is the pre-send camelCase shape (9dd90a497); the wire remap
    /// itself is covered by the mistral-http-transport contract tests.
    async fn capture_payload(model: &Model, reasoning: Option<ThinkingLevel>) -> Value {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured.clone();
        let mut simple = SimpleStreamOptions {
            stream: StreamOptions {
                request: crate::types::ProviderRequestOptions {
                    api_key: Some("fake-key".to_owned()),
                    ..Default::default()
                },
                ..StreamOptions::default()
            },
            reasoning,
            thinking_budgets: None,

            tool_choice: None,
        };
        simple.stream.on_payload = Some(std::sync::Arc::new(move |payload, _model| {
            let captured = captured_clone.clone();
            Box::pin(async move {
                *captured.lock().expect("lock") = Some(payload);
                None
            })
        }));
        let mut unreachable = model.clone();
        unreachable.base_url = "http://127.0.0.1:9".to_owned();
        let ctx = context(vec![user_text("Hello")], None);
        let _: Vec<StreamEvent> = stream_simple(&unreachable, &ctx, Some(simple))
            .expect("stream_simple")
            .collect()
            .await;
        let payload = captured.lock().expect("lock").clone();
        payload.expect("payload captured before request failure")
    }

    #[tokio::test]
    async fn test_stream_simple_reasoning_effort_models() {
        // mistral-reasoning-mode.test.ts: reasoning_effort for models with a
        // thinking level map; off sends the map's off value (#9678).
        // (9dd90a497: the captured payload is camelCase until the wire remap.)
        let model = make_model(json!({
            "id": "mistral-small-2603",
            "reasoning": true,
            "thinkingLevelMap": {
                "off": "none", "minimal": null, "low": null,
                "medium": null, "high": "high", "xhigh": null, "max": null
            }
        }));
        let payload = capture_payload(&model, Some(ThinkingLevel::Medium)).await;
        assert_eq!(payload["reasoningEffort"], json!("high"));
        assert!(payload.get("promptMode").is_none());

        // Thinking off: the model's off value is sent (none), not dropped.
        let payload = capture_payload(&model, None).await;
        assert_eq!(payload["reasoningEffort"], json!("none"));
        assert!(payload.get("promptMode").is_none());

        // GLM 5.3 map: low/high/max pass through; medium maps to high.
        let glm = make_model(json!({
            "id": "zai-glm-5-3",
            "reasoning": true,
            "thinkingLevelMap": {
                "off": null, "minimal": null, "low": "low",
                "medium": null, "high": "high", "xhigh": null, "max": "max"
            }
        }));
        for (level, expected) in [
            (ThinkingLevel::Low, "low"),
            (ThinkingLevel::High, "high"),
            (ThinkingLevel::Max, "max"),
            (ThinkingLevel::Medium, "high"),
        ] {
            let payload = capture_payload(&glm, Some(level)).await;
            assert_eq!(payload["reasoningEffort"], json!(expected), "{level:?}");
            assert!(payload.get("promptMode").is_none(), "{level:?}");
        }
    }

    #[tokio::test]
    async fn test_stream_simple_prompt_mode_models() {
        // mistral-reasoning-mode.test.ts: prompt_mode for reasoning models
        // without a thinking level map (Magistral).
        let model = reasoning_model("magistral-medium-latest");
        let payload = capture_payload(&model, Some(ThinkingLevel::Medium)).await;
        assert_eq!(payload["promptMode"], json!("reasoning"));
        assert!(payload.get("reasoningEffort").is_none());

        // Thinking off: no reasoning controls.
        let payload = capture_payload(&model, None).await;
        assert!(payload.get("promptMode").is_none());
        assert!(payload.get("reasoningEffort").is_none());
    }
}
