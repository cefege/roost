//! Port of `packages/ai/src/api/openai-responses-shared.ts` @ pi 0.82.1
//! (2efa728); terminal-event semantics (rawStopReason, incomplete-reason
//! mapping) updated to 4181f66 (e5ef8d065, 32850ef7c).
//!
//! Shared OpenAI Responses machinery: message/tool conversion
//! (`convertResponsesMessages` / `convertResponsesTools`), text-signature v1
//! encoding, and the streaming event processor (`processResponsesStream`).
//! Used by the openai-responses adapter now and by the azure-openai-responses
//! / openai-codex-responses adapters later (T13).
//!
//! Intentional differences (upstream deviations):
//! - Replay-time `JSON.parse` of thinking signatures uses `serde_json`; parse
//!   failures carry the serde error text (upstream surfaces a `SyntaxError`
//!   message).
//! - The `resolveServiceTier` hook lands with the azure/codex adapters (T13);
//!   the tier defaults to the response's `service_tier`, falling back to the
//!   request's, matching upstream when no hook is supplied.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::api::constrained_sampling::{
    GrammarToolInputJsonBuffer, append_grammar_tool_input_json_delta, get_grammar_tool_input,
    get_json_schema_tool_parameters, resolve_grammar_constrained_sampling,
    resolve_json_schema_strict_sampling,
};
use crate::types::StreamEvent;
use crate::types::{
    AssistantContent, AssistantMessage, InputModality, Message, Model, StopReason,
    TextSignaturePhase, TextSignatureV1, Tool, ToolCall, ToolResultContent, TranscriptContext,
    Usage,
};
use crate::utils::cost::calculate_cost;
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::hash::short_hash;
use crate::utils::json_parse::parse_streaming_json;
use crate::utils::sanitize_unicode::sanitize_surrogates;
use crate::utils::transform_messages::transform_messages;

// =============================================================================
// Text signature v1
// =============================================================================

/// `encodeTextSignatureV1`: `{"v":1,"id":...}` plus `"phase"` when set.
pub fn encode_text_signature_v1(id: &str, phase: Option<TextSignaturePhase>) -> String {
    serde_json::to_string(&TextSignatureV1 {
        v: 1,
        id: id.to_owned(),
        phase,
    })
    .unwrap_or_default()
}

/// `parseTextSignature` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedTextSignature {
    pub id: String,
    pub phase: Option<TextSignaturePhase>,
}

/// `parseTextSignature`: v1 JSON payloads decode fully; anything else is a
/// legacy plain id string. `None` only when no signature was given.
pub fn parse_text_signature(signature: Option<&str>) -> Option<ParsedTextSignature> {
    let signature = signature?;
    if signature.starts_with('{')
        && let Ok(parsed) = serde_json::from_str::<Value>(signature)
        && parsed.get("v").and_then(Value::as_u64) == Some(1)
        && let Some(id) = parsed.get("id").and_then(Value::as_str)
    {
        let phase = match parsed.get("phase").and_then(Value::as_str) {
            Some("commentary") => Some(TextSignaturePhase::Commentary),
            Some("final_answer") => Some(TextSignaturePhase::FinalAnswer),
            _ => None,
        };
        return Some(ParsedTextSignature {
            id: id.to_owned(),
            phase,
        });
    }
    // Fall through to legacy plain-string handling.
    Some(ParsedTextSignature {
        id: signature.to_owned(),
        phase: None,
    })
}

// =============================================================================
// Tool result output
// =============================================================================

/// `convertToolResultOutput`: plain text when there are no images (or the
/// model can't see them); otherwise an input_text/input_image block array.
fn convert_tool_result_output(model: &Model, content: &[ToolResultContent]) -> Value {
    let text_result = content
        .iter()
        .filter_map(|block| match block {
            ToolResultContent::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let images: Vec<&crate::types::ImageContent> = content
        .iter()
        .filter_map(|block| match block {
            ToolResultContent::Image(image) => Some(image),
            _ => None,
        })
        .collect();
    let has_text = !text_result.is_empty();

    if images.is_empty() || !model.input.contains(&InputModality::Image) {
        let text = if has_text {
            text_result
        } else if !images.is_empty() {
            "(see attached image)".to_owned()
        } else {
            "(no tool output)".to_owned()
        };
        return json!(sanitize_surrogates(&text));
    }

    let mut output: Vec<Value> = Vec::new();
    if has_text {
        output.push(json!({"type": "input_text", "text": sanitize_surrogates(&text_result)}));
    }
    for image in images {
        output.push(json!({
            "type": "input_image",
            "detail": "auto",
            "image_url": format!("data:{};base64,{}", image.mime_type, image.data),
        }));
    }
    Value::Array(output)
}

// =============================================================================
// Options
// =============================================================================

/// `ConvertResponsesToolsOptions`. Upstream defaults apply in
/// [`convert_responses_tools`]: `strict` → false, `supportsStrictMode` →
/// true, `supportsOpenAIGrammarTools` → false.
#[derive(Debug, Clone, Default)]
pub struct ConvertResponsesToolsOptions {
    pub strict: Option<bool>,
    pub supports_strict_mode: Option<bool>,
    pub supports_open_ai_grammar_tools: Option<bool>,
    /// `toolSearchResult` (#9548 rename of `deferLoading`): tools surfacing
    /// inside a `tool_search_output` item get `defer_loading: true`.
    pub tool_search_result: bool,
}

/// `ConvertResponsesMessagesOptions`.
pub struct ConvertResponsesMessagesOptions<'a> {
    /// Default: true.
    pub include_system_prompt: bool,
    pub grammar_tool_input_properties: Option<&'a HashMap<String, String>>,
    /// Whether later system messages are sent in place; otherwise they are
    /// folded into the leading prompt (#9548).
    pub supports_mid_convo_system_messages: bool,
    /// `supportsAdditionalTools`: message-anchored `additional_tools` items.
    pub supports_additional_tools: bool,
    /// `supportsToolSearch`: client-executed `tool_search_call`/`_output`
    /// pairs anchor late tool declarations.
    pub supports_tool_search: bool,
    pub tool_options: ConvertResponsesToolsOptions,
}

impl Default for ConvertResponsesMessagesOptions<'_> {
    fn default() -> Self {
        Self {
            include_system_prompt: true,
            grammar_tool_input_properties: None,
            supports_mid_convo_system_messages: false,
            supports_additional_tools: false,
            supports_tool_search: false,
            tool_options: ConvertResponsesToolsOptions::default(),
        }
    }
}

// =============================================================================
// Message conversion
// =============================================================================

/// `convertResponsesMessages`.
///
/// Tool-call ids keep the Responses `{call_id}|{item_id}` composite form for
/// allowed providers (OpenAI family); each part is sanitized to 64 chars and
/// foreign item ids rebuild as `fc_<shortHash>` (the Responses API requires
/// item ids to start with "fc").
pub fn convert_responses_messages(
    model: &Model,
    context: &TranscriptContext,
    allowed_tool_call_providers: &HashSet<&str>,
    options: &ConvertResponsesMessagesOptions,
) -> Result<Vec<Value>, String> {
    let normalized_context = crate::utils::transcript::resolve_transcript(
        context,
        options.supports_mid_convo_system_messages,
    );
    let context = &normalized_context;
    let mut messages: Vec<Value> = Vec::new();

    // Sanitize to allowed chars, cap at 64, strip trailing underscores.
    let normalize_id_part = |part: &str| -> String {
        let sanitized: String = part
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let truncated: String = sanitized.chars().take(64).collect();
        truncated.trim_end_matches('_').to_owned()
    };

    let build_foreign_responses_item_id = |item_id: &str| -> String {
        let normalized = format!("fc_{}", short_hash(item_id));
        normalized.chars().take(64).collect()
    };

    // Owned captures keep the closure `'static` for `transform_messages`.
    let target_provider = model.provider.clone();
    let target_api = model.api.clone();
    let allowed_providers: HashSet<String> = allowed_tool_call_providers
        .iter()
        .map(|provider| (*provider).to_owned())
        .collect();

    let mut normalize_tool_call_id = move |id: &str, _target: &Model, source: &AssistantMessage| {
        if !allowed_providers.contains(target_provider.as_str()) {
            return normalize_id_part(id);
        }
        if !id.contains('|') {
            return normalize_id_part(id);
        }
        // JS `id.split("|")` destructuring keeps only the first two parts.
        let mut parts = id.split('|');
        let call_id = parts.next().unwrap_or("");
        let item_id = parts.next().unwrap_or("");
        let normalized_call_id = normalize_id_part(call_id);
        let is_foreign_tool_call = source.provider != target_provider || source.api != target_api;
        let mut normalized_item_id = if is_foreign_tool_call {
            build_foreign_responses_item_id(item_id)
        } else {
            normalize_id_part(item_id)
        };
        // OpenAI Responses API requires item id to start with "fc".
        if !normalized_item_id.starts_with("fc_") {
            normalized_item_id = normalize_id_part(&format!("fc_{normalized_item_id}"));
        }
        format!("{normalized_call_id}|{normalized_item_id}")
    };
    let transformed_messages =
        transform_messages(&context.messages, model, Some(&mut normalize_tool_call_id));

    let transcript_tools = crate::utils::transcript::resolve_transcript_tools(
        &context.messages,
        options.supports_additional_tools || options.supports_tool_search,
    );
    let append_system_tool_additions = |messages: &mut Vec<Value>,
                                        message: &crate::types::SystemMessage,
                                        seed: &str|
     -> Result<(), String> {
        let tools: Vec<Tool> = if transcript_tools.anchors_additions {
            message.tools_added.clone().unwrap_or_default()
        } else {
            Vec::new()
        };
        if tools.is_empty() {
            return Ok(());
        }
        if options.supports_additional_tools {
            messages.push(json!({
                "type": "additional_tools",
                "role": "developer",
                "tools": convert_responses_tools(&tools, &options.tool_options)?,
            }));
            return Ok(());
        }
        if !options.supports_tool_search {
            return Ok(());
        }
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
        let call_id = format!(
            "pi_tool_load_{}",
            short_hash(&format!("{seed}:{}", names.join(",")))
        );
        messages.push(json!({
            "type": "tool_search_call",
            "call_id": call_id,
            "execution": "client",
            "status": "completed",
            "arguments": {"query": names.join(" "), "limit": names.len()},
        }));
        let tool_options = ConvertResponsesToolsOptions {
            tool_search_result: true,
            ..options.tool_options.clone()
        };
        messages.push(json!({
            "type": "tool_search_output",
            "call_id": call_id,
            "execution": "client",
            "status": "completed",
            "tools": convert_responses_tools(&tools, &tool_options)?,
        }));
        Ok(())
    };
    // `compat?.supportsDeveloperRole !== false`: absent counts as supported.
    let supports_developer_role = model
        .compat
        .as_ref()
        .and_then(|compat| compat.supports_developer_role)
        != Some(false);
    let instruction_role = if model.reasoning && supports_developer_role {
        "developer"
    } else {
        "system"
    };

    let mut msg_index = 0usize;
    for (source_index, msg) in transformed_messages.iter().enumerate() {
        let is_leading_system_message = source_index == 0 && matches!(msg, Message::System(_));
        match msg {
            Message::System(system) => {
                if !is_leading_system_message {
                    append_system_tool_additions(
                        &mut messages,
                        system,
                        &format!("system:{msg_index}"),
                    )?;
                }
                if !is_leading_system_message || options.include_system_prompt {
                    let text = if is_leading_system_message {
                        crate::utils::text::get_system_message_text(system)
                    } else {
                        crate::utils::text::render_system_message_update(system)
                    };
                    if !text.is_empty() {
                        messages.push(json!({
                            "role": instruction_role,
                            "content": sanitize_surrogates(&text),
                        }));
                    }
                }
            }
            Message::User(user) => match &user.content {
                crate::types::UserContent::Text(text) => {
                    messages.push(json!({
                        "role": "user",
                        "content": [{"type": "input_text", "text": sanitize_surrogates(text)}],
                    }));
                }
                crate::types::UserContent::Blocks(blocks) => {
                    let content: Vec<Value> = blocks
                        .iter()
                        .map(|item| match item {
                            crate::types::UserContentBlock::Text(text) => json!({
                                "type": "input_text",
                                "text": sanitize_surrogates(&text.text),
                            }),
                            crate::types::UserContentBlock::Image(image) => json!({
                                "type": "input_image",
                                "detail": "auto",
                                "image_url": format!("data:{};base64,{}", image.mime_type, image.data),
                            }),
                        })
                        .collect();
                    if content.is_empty() {
                        msg_index += 1;
                        continue;
                    }
                    messages.push(json!({"role": "user", "content": content}));
                }
            },
            Message::Assistant(assistant) => {
                let mut output: Vec<Value> = Vec::new();
                // 02bd2d1c6 (#7709): namespace replay is allowed for same-model
                // messages or tools loaded through a deferred-tool marker.
                let is_same_model = assistant.model == model.id
                    && assistant.provider == model.provider
                    && assistant.api == model.api;
                let is_different_model = assistant.model != model.id
                    && assistant.provider == model.provider
                    && assistant.api == model.api;
                let mut text_block_index = 0usize;

                for block in &assistant.content {
                    match block {
                        AssistantContent::Thinking(thinking) => {
                            if let Some(signature) = thinking
                                .thinking_signature
                                .as_deref()
                                .filter(|signature| !signature.is_empty())
                            {
                                let reasoning_item: Value = serde_json::from_str(signature)
                                    .map_err(|error| error.to_string())?;
                                output.push(reasoning_item);
                            }
                        }
                        AssistantContent::Text(text_block) => {
                            let parsed_signature =
                                parse_text_signature(text_block.text_signature.as_deref());
                            let fallback_message_id = if text_block_index == 0 {
                                format!("msg_pi_{msg_index}")
                            } else {
                                format!("msg_pi_{msg_index}_{text_block_index}")
                            };
                            text_block_index += 1;
                            // OpenAI requires id to be max 64 characters.
                            let msg_id = match parsed_signature.as_ref() {
                                None => fallback_message_id,
                                Some(parsed) if parsed.id.chars().count() > 64 => {
                                    format!("msg_{}", short_hash(&parsed.id))
                                }
                                Some(parsed) => parsed.id.clone(),
                            };
                            let mut message = json!({
                                "type": "message",
                                "role": "assistant",
                                "content": [{
                                    "type": "output_text",
                                    "text": sanitize_surrogates(&text_block.text),
                                    "annotations": [],
                                }],
                                "status": "completed",
                                "id": msg_id,
                            });
                            if let Some(phase) = parsed_signature.and_then(|parsed| parsed.phase) {
                                message["phase"] = json!(phase);
                            }
                            output.push(message);
                        }
                        AssistantContent::ToolCall(tool_call) => {
                            let mut parts = tool_call.id.split('|');
                            let call_id = parts.next().unwrap_or("");
                            let item_id_raw = parts.next();
                            let custom_input_property = options
                                .grammar_tool_input_properties
                                .and_then(|properties| properties.get(&tool_call.name));
                            let mut item_id: Option<&str> = item_id_raw;

                            // For different-model messages, set id to undefined to
                            // avoid pairing validation (OpenAI tracks which item IDs
                            // were paired with rs_xxx reasoning items). Also drop ids
                            // that do not match the replayed item type:
                            // function_call ids must be fc_* and custom_tool_call
                            // ids must be ctc_*. Foreign tool call ids are
                            // normalized to fc_*, and a call can switch between the
                            // two types when grammar tool support differs
                            // (bc2d8dc1c, radius#115).
                            let item_id_prefix = if custom_input_property.is_none() {
                                "fc_"
                            } else {
                                "ctc_"
                            };
                            if is_different_model
                                || !item_id.is_some_and(|id| id.starts_with(item_id_prefix))
                            {
                                item_id = None;
                            }

                            // #9548: namespace replay is same-model only
                            // (deferredTools.has went with the old mechanism).
                            let can_replay_namespace = is_same_model;

                            if let Some(property) = custom_input_property {
                                let input = get_grammar_tool_input(
                                    &tool_call.name,
                                    &tool_call.arguments,
                                    property,
                                )?;
                                let mut item = json!({
                                    "type": "custom_tool_call",
                                    "call_id": call_id,
                                    "name": tool_call.name,
                                    "input": sanitize_surrogates(&input),
                                });
                                if let Some(item_id) = item_id {
                                    item["id"] = json!(item_id);
                                }
                                if can_replay_namespace
                                    && let Some(namespace) = &tool_call.namespace
                                {
                                    item["namespace"] = json!(namespace);
                                }
                                output.push(item);
                            } else {
                                let mut item = json!({
                                    "type": "function_call",
                                    "call_id": call_id,
                                    "name": tool_call.name,
                                    "arguments": serde_json::to_string(&tool_call.arguments)
                                        .unwrap_or_else(|_| "{}".to_owned()),
                                });
                                if let Some(item_id) = item_id {
                                    item["id"] = json!(item_id);
                                }
                                if can_replay_namespace
                                    && let Some(namespace) = &tool_call.namespace
                                {
                                    item["namespace"] = json!(namespace);
                                }
                                output.push(item);
                            }
                        }
                    }
                }
                if output.is_empty() {
                    msg_index += 1;
                    continue;
                }
                messages.extend(output);
            }
            Message::ToolResult(result) => {
                let call_id = result.tool_call_id.split('|').next().unwrap_or("");
                let output = convert_tool_result_output(model, &result.content);

                let is_grammar_tool = options
                    .grammar_tool_input_properties
                    .is_some_and(|properties| properties.contains_key(&result.tool_name));
                if is_grammar_tool {
                    messages.push(json!({
                        "type": "custom_tool_call_output",
                        "call_id": call_id,
                        "output": output,
                    }));
                } else {
                    messages.push(json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": output,
                    }));
                }
            }
        }
        // #9548: the leading system message is not a conversation message —
        // indices keep matching the pre-system-message numbering.
        if !is_leading_system_message {
            msg_index += 1;
        }
    }

    Ok(messages)
}

// =============================================================================
// Tool conversion
// =============================================================================

/// `convertResponsesTools`: grammar-constrained tools become `custom` tools;
/// everything else becomes a `function` tool (`strict` only when supported).
pub fn convert_responses_tools(
    tools: &[Tool],
    options: &ConvertResponsesToolsOptions,
) -> Result<Vec<Value>, String> {
    let default_strict = options.strict.unwrap_or(false);
    let supports_strict_mode = options.supports_strict_mode.unwrap_or(true);
    let supports_open_ai_grammar_tools = options.supports_open_ai_grammar_tools.unwrap_or(false);

    tools
        .iter()
        .map(|tool| {
            if let Some(grammar) =
                resolve_grammar_constrained_sampling(tool, supports_open_ai_grammar_tools)?
            {
                let mut converted = json!({
                    "type": "custom",
                    "name": tool.name,
                    "description": tool.description,
                    "format": {
                        "type": "grammar",
                        "syntax": grammar.format.as_str(),
                        "definition": grammar.definition,
                    },
                });
                if options.tool_search_result {
                    converted["defer_loading"] = json!(true);
                }
                return Ok(converted);
            }

            let constrained_strict =
                resolve_json_schema_strict_sampling(tool, supports_strict_mode)?;
            let strict = constrained_strict.unwrap_or(default_strict);
            let mut function_tool = json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                // TypeBox already generates JSON Schema upstream; `parameters`
                // is a schema value here — strict tools send the converted
                // subset (openai-responses-shared.ts:380-388, 7915cdac6).
                "parameters": get_json_schema_tool_parameters(tool, Some(strict))?,
            });
            if options.tool_search_result {
                function_tool["defer_loading"] = json!(true);
            }
            if supports_strict_mode {
                function_tool["strict"] = json!(strict);
            }
            Ok(function_tool)
        })
        .collect()
}

// =============================================================================
// Stream processing
// =============================================================================

/// `applyServiceTierPricing` hook type (openai-responses passes its
/// service-tier cost multiplier).
pub type ApplyServiceTierPricing<'a> = Box<dyn Fn(&mut Usage, Option<&str>) + Send + 'a>;

/// `resolveServiceTier` hook type: maps (response tier, request tier) to the
/// effective tier. `None` keeps the default `response ?? request` resolution
/// (openai-responses); the Codex adapter passes `resolveCodexServiceTier`,
/// which treats a `"default"` response tier as the requested flex/priority
/// tier.
pub type ResolveServiceTier = fn(Option<&str>, Option<&str>) -> Option<String>;

/// `OpenAIResponsesStreamOptions` (processor side).
pub struct ResponsesStreamOptions<'a> {
    pub service_tier: Option<&'a str>,
    pub grammar_tool_input_properties: &'a HashMap<String, String>,
    pub apply_service_tier_pricing: Option<ApplyServiceTierPricing<'a>>,
    pub resolve_service_tier: Option<ResolveServiceTier>,
}

/// Custom (grammar) tool streaming state.
#[derive(Debug, Default)]
struct CustomInputState {
    property: String,
    json_buffer: GrammarToolInputJsonBuffer,
}

/// Per-tool-call streaming scratch state (upstream stores `partialJson` and
/// `customInput` on the streaming block; here keyed by content index).
#[derive(Debug, Default)]
struct ToolScratch {
    partial_json: Option<String>,
    custom_input: Option<CustomInputState>,
}

/// `ResponsesOutputSlot`: which content block an output index maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Thinking { content_index: usize },
    Text { content_index: usize },
    ToolCall { content_index: usize },
}

impl Slot {
    fn content_index(self) -> usize {
        match self {
            Slot::Thinking { content_index }
            | Slot::Text { content_index }
            | Slot::ToolCall { content_index } => content_index,
        }
    }
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
/// `block.arguments` with `{ property: nextInput }`.
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

/// `mapStopReason` (response status); unknown statuses are an error (the API
/// may add new values). For incomplete responses, `max_output_tokens` is the
/// ONLY length-stop reason; any other provider reason is a non-retryable
/// error carrying `Response incomplete: <reason>` (32850ef7c).
fn map_stop_reason(
    status: Option<&str>,
    incomplete_reason: Option<&str>,
) -> Result<(StopReason, Option<String>), String> {
    match status {
        None => Ok((StopReason::Stop, None)),
        Some("completed") => Ok((StopReason::Stop, None)),
        Some("incomplete") => {
            if incomplete_reason == Some("max_output_tokens") {
                return Ok((StopReason::Length, None));
            }
            Ok((
                StopReason::Error,
                Some(match incomplete_reason {
                    Some(reason) => format!("Response incomplete: {reason}"),
                    None => "Response incomplete without a provider reason".to_owned(),
                }),
            ))
        }
        Some("failed") | Some("cancelled") => Ok((StopReason::Error, None)),
        // These two are wonky ...
        Some("in_progress") | Some("queued") => Ok((StopReason::Stop, None)),
        Some(other) => Err(format!("Unhandled stop reason: {other}")),
    }
}

/// `processResponsesStream`: consumes Responses stream events and drives the
/// [`StreamEvent`] protocol, accumulating the final assistant message.
/// Factored out of the HTTP layer so tests can drive it with recorded events.
pub struct ResponsesStreamProcessor<'a> {
    output: &'a mut AssistantMessage,
    model: &'a Model,
    options: ResponsesStreamOptions<'a>,
    saw_terminal_response_event: bool,
    slots: HashMap<usize, Slot>,
    scratch: HashMap<usize, ToolScratch>,
    reasoning_blocks_by_id: HashMap<String, usize>,
}

impl<'a> ResponsesStreamProcessor<'a> {
    pub fn new(
        output: &'a mut AssistantMessage,
        model: &'a Model,
        options: ResponsesStreamOptions<'a>,
    ) -> Self {
        Self {
            output,
            model,
            options,
            saw_terminal_response_event: false,
            slots: HashMap::new(),
            scratch: HashMap::new(),
            reasoning_blocks_by_id: HashMap::new(),
        }
    }

    fn apply_message_phase_stop_reason(&mut self, item: &Value) {
        if item.get("type").and_then(Value::as_str) == Some("message")
            && item.get("phase").and_then(Value::as_str) == Some("final_answer")
        {
            self.output.stop_reason = StopReason::Stop;
        }
    }

    fn get_slot(&self, output_index: usize, expected: SlotKind) -> Option<Slot> {
        let slot = self.slots.get(&output_index).copied()?;
        let matches = matches!(
            (expected, slot),
            (SlotKind::Thinking, Slot::Thinking { .. })
                | (SlotKind::Text, Slot::Text { .. })
                | (SlotKind::ToolCall, Slot::ToolCall { .. })
        );
        matches.then_some(slot)
    }

    fn push_tool_call_delta(
        &self,
        content_index: usize,
        delta: Option<String>,
        events: &AssistantMessageEventStream,
    ) {
        if let Some(delta) = delta {
            events.push(StreamEvent::ToolCallDelta {
                content_index,
                delta,
                partial: Arc::new(self.output.clone()),
            });
        }
    }

    /// `createSlot`: append the content block for a new output item and emit
    /// its start event.
    fn create_slot(
        &mut self,
        output_index: usize,
        item: &Value,
        events: &AssistantMessageEventStream,
    ) -> Option<Slot> {
        match item.get("type").and_then(Value::as_str) {
            Some("reasoning") => {
                self.output.content.push(AssistantContent::Thinking(
                    crate::types::ThinkingContent {
                        thinking: String::new(),
                        thinking_signature: None,
                        redacted: None,
                    },
                ));
                let slot = Slot::Thinking {
                    content_index: self.output.content.len() - 1,
                };
                self.slots.insert(output_index, slot);
                events.push(StreamEvent::ThinkingStart {
                    content_index: slot.content_index(),
                    partial: Arc::new(self.output.clone()),
                });
                Some(slot)
            }
            Some("message") => {
                self.apply_message_phase_stop_reason(item);
                self.output
                    .content
                    .push(AssistantContent::Text(crate::types::TextContent {
                        text: String::new(),
                        text_signature: None,
                    }));
                let slot = Slot::Text {
                    content_index: self.output.content.len() - 1,
                };
                self.slots.insert(output_index, slot);
                events.push(StreamEvent::TextStart {
                    content_index: slot.content_index(),
                    partial: Arc::new(self.output.clone()),
                });
                Some(slot)
            }
            Some("function_call") => {
                let call_id = item.get("call_id").and_then(Value::as_str).unwrap_or("");
                // JS template `${item.call_id}|${item.id}` renders a missing id
                // as "undefined".
                let item_id = item
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("undefined");
                let partial_json = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                self.output
                    .content
                    .push(AssistantContent::ToolCall(ToolCall {
                        id: format!("{call_id}|{item_id}"),
                        name: item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                        arguments: Map::new(),
                        thought_signature: None,
                        // 02bd2d1c6 (#7709): preserve the Responses namespace.
                        namespace: item
                            .get("namespace")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    }));
                let slot = Slot::ToolCall {
                    content_index: self.output.content.len() - 1,
                };
                self.scratch.insert(
                    slot.content_index(),
                    ToolScratch {
                        partial_json: Some(partial_json),
                        custom_input: None,
                    },
                );
                self.slots.insert(output_index, slot);
                events.push(StreamEvent::ToolCallStart {
                    content_index: slot.content_index(),
                    partial: Arc::new(self.output.clone()),
                });
                Some(slot)
            }
            Some("custom_tool_call") => {
                let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                let call_id = item.get("call_id").and_then(Value::as_str).unwrap_or("");
                let item_id = item
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("undefined");
                let input_property = self
                    .options
                    .grammar_tool_input_properties
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| "input".to_owned());
                let input = item
                    .get("input")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let mut arguments = Map::new();
                arguments.insert(input_property.clone(), json!(input));
                self.output
                    .content
                    .push(AssistantContent::ToolCall(ToolCall {
                        id: format!("{call_id}|{item_id}"),
                        name: name.to_owned(),
                        arguments,
                        thought_signature: None,
                        // 02bd2d1c6 (#7709): preserve the Responses namespace.
                        namespace: item
                            .get("namespace")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    }));
                let slot = Slot::ToolCall {
                    content_index: self.output.content.len() - 1,
                };
                self.scratch.insert(
                    slot.content_index(),
                    ToolScratch {
                        partial_json: None,
                        custom_input: Some(CustomInputState {
                            property: input_property,
                            json_buffer: GrammarToolInputJsonBuffer::default(),
                        }),
                    },
                );
                self.slots.insert(output_index, slot);
                events.push(StreamEvent::ToolCallStart {
                    content_index: slot.content_index(),
                    partial: Arc::new(self.output.clone()),
                });
                Some(slot)
            }
            _ => None,
        }
    }

    fn get_or_create_slot(
        &mut self,
        output_index: usize,
        item: &Value,
        events: &AssistantMessageEventStream,
    ) -> Option<Slot> {
        match self.slots.get(&output_index).copied() {
            Some(slot) => Some(slot),
            None => self.create_slot(output_index, item, events),
        }
    }

    /// `backfillReasoningSignatures`: Azure OpenAI can omit
    /// `reasoning.encrypted_content` from `response.output_item.done` and
    /// provide it only in the terminal response; backfill it so store:false
    /// multi-turn replay stays stateless.
    fn backfill_reasoning_signatures(&mut self, response_output: &[Value]) -> Result<(), String> {
        for item in response_output {
            if item.get("type").and_then(Value::as_str) != Some("reasoning") {
                continue;
            }
            let Some(encrypted_content) = item
                .get("encrypted_content")
                .filter(|value| !value.is_null())
            else {
                continue;
            };
            let Some(item_id) = item.get("id").and_then(Value::as_str) else {
                continue;
            };
            let Some(content_index) = self.reasoning_blocks_by_id.get(item_id).copied() else {
                continue;
            };
            let Some(AssistantContent::Thinking(block)) =
                self.output.content.get_mut(content_index)
            else {
                continue;
            };
            let Some(signature) = block
                .thinking_signature
                .as_deref()
                .filter(|signature| !signature.is_empty())
            else {
                continue;
            };
            let mut stored: Value =
                serde_json::from_str(signature).map_err(|error| error.to_string())?;
            if stored
                .get("encrypted_content")
                .is_some_and(|value| !value.is_null())
            {
                continue;
            }
            stored["encrypted_content"] = encrypted_content.clone();
            block.thinking_signature = Some(serde_json::to_string(&stored).unwrap_or_default());
        }
        Ok(())
    }

    /// `finalizeResponse`: terminal usage/stop-reason accounting.
    fn finalize_response(&mut self, response: &Value) -> Result<(), String> {
        self.saw_terminal_response_event = true;
        if let Some(response_output) = response.get("output").and_then(Value::as_array) {
            self.backfill_reasoning_signatures(response_output)?;
        }
        if let Some(id) = response.get("id").and_then(Value::as_str) {
            self.output.response_id = Some(id.to_owned());
        }
        if let Some(usage) = response.get("usage").filter(|usage| !usage.is_null()) {
            let cached_tokens =
                json_u64(&usage["input_tokens_details"]["cached_tokens"]).unwrap_or(0);
            let cache_write_tokens =
                json_u64(&usage["input_tokens_details"]["cache_write_tokens"]).unwrap_or(0);
            // OpenAI includes cached and cache-write tokens in input_tokens, so
            // subtract both.
            let input = json_u64(&usage["input_tokens"])
                .unwrap_or(0)
                .saturating_sub(cached_tokens + cache_write_tokens);
            self.output.usage = Usage {
                input,
                output: json_u64(&usage["output_tokens"]).unwrap_or(0),
                cache_read: cached_tokens,
                cache_write: cache_write_tokens,
                cache_write1h: None,
                reasoning: Some(
                    json_u64(&usage["output_tokens_details"]["reasoning_tokens"]).unwrap_or(0),
                ),
                total_tokens: json_u64(&usage["total_tokens"]).unwrap_or(0),
                cost: Default::default(),
            };
        }
        calculate_cost(self.model, &mut self.output.usage);
        if let Some(apply) = &self.options.apply_service_tier_pricing {
            let response_tier = response.get("service_tier").and_then(Value::as_str);
            let service_tier = match self.options.resolve_service_tier {
                Some(resolve) => resolve(response_tier, self.options.service_tier),
                None => response_tier
                    .or(self.options.service_tier)
                    .map(str::to_owned),
            };
            apply(&mut self.output.usage, service_tier.as_deref());
        }
        // Map status to stop reason. For incomplete responses, retain the
        // provider's specific reason so max-output truncation and content
        // filtering stay distinct (32850ef7c; rawStopReason e5ef8d065).
        let status = response.get("status").and_then(Value::as_str);
        let incomplete_reason = response
            .get("incomplete_details")
            .and_then(|details| details.get("reason"))
            .and_then(Value::as_str);
        self.output.raw_stop_reason = match incomplete_reason {
            // JS `${status}.${incompleteReason}` interpolates missing status
            // as "undefined".
            Some(reason) => Some(format!("{}.{reason}", status.unwrap_or("undefined"))),
            None => status.map(str::to_owned),
        };
        let mapped_stop = map_stop_reason(status, incomplete_reason)?;
        self.output.stop_reason = mapped_stop.0;
        self.output.error_message = mapped_stop.1;
        if self
            .output
            .content
            .iter()
            .any(|block| matches!(block, AssistantContent::ToolCall(_)))
            && self.output.stop_reason == StopReason::Stop
        {
            self.output.stop_reason = StopReason::ToolUse;
        }
        Ok(())
    }

    /// Per-event body of the `for await` loop upstream.
    pub fn handle_event(
        &mut self,
        event: &Value,
        events: &AssistantMessageEventStream,
    ) -> Result<(), String> {
        let output_index = event.get("output_index").and_then(json_u64).unwrap_or(0) as usize;
        match event.get("type").and_then(Value::as_str) {
            Some("response.created") => {
                if let Some(id) = event
                    .get("response")
                    .and_then(|response| response.get("id"))
                    .and_then(Value::as_str)
                {
                    self.output.response_id = Some(id.to_owned());
                }
            }
            Some("response.output_item.added") => {
                if let Some(item) = event.get("item") {
                    self.create_slot(output_index, item, events);
                }
            }
            Some("response.reasoning_summary_text.delta")
            | Some("response.reasoning_text.delta") => {
                let Some(Slot::Thinking { content_index }) =
                    self.get_slot(output_index, SlotKind::Thinking)
                else {
                    return Ok(());
                };
                let delta = event.get("delta").and_then(Value::as_str).unwrap_or("");
                if let Some(AssistantContent::Thinking(block)) =
                    self.output.content.get_mut(content_index)
                {
                    block.thinking.push_str(delta);
                }
                events.push(StreamEvent::ThinkingDelta {
                    content_index,
                    delta: delta.to_owned(),
                    partial: Arc::new(self.output.clone()),
                });
            }
            Some("response.reasoning_summary_part.done") => {
                let Some(Slot::Thinking { content_index }) =
                    self.get_slot(output_index, SlotKind::Thinking)
                else {
                    return Ok(());
                };
                if let Some(AssistantContent::Thinking(block)) =
                    self.output.content.get_mut(content_index)
                {
                    block.thinking.push_str("\n\n");
                }
                events.push(StreamEvent::ThinkingDelta {
                    content_index,
                    delta: "\n\n".to_owned(),
                    partial: Arc::new(self.output.clone()),
                });
            }
            Some("response.output_text.delta") | Some("response.refusal.delta") => {
                let Some(Slot::Text { content_index }) =
                    self.get_slot(output_index, SlotKind::Text)
                else {
                    return Ok(());
                };
                let delta = event.get("delta").and_then(Value::as_str).unwrap_or("");
                if let Some(AssistantContent::Text(block)) =
                    self.output.content.get_mut(content_index)
                {
                    block.text.push_str(delta);
                }
                events.push(StreamEvent::TextDelta {
                    content_index,
                    delta: delta.to_owned(),
                    partial: Arc::new(self.output.clone()),
                });
            }
            Some("response.function_call_arguments.delta") => {
                let Some(Slot::ToolCall { content_index }) =
                    self.get_slot(output_index, SlotKind::ToolCall)
                else {
                    return Ok(());
                };
                let delta = event.get("delta").and_then(Value::as_str);
                let Some(scratch) = self.scratch.get_mut(&content_index) else {
                    return Ok(());
                };
                let Some(partial_json) = scratch.partial_json.as_mut() else {
                    return Ok(());
                };
                let Some(delta) = delta else { return Ok(()) };
                partial_json.push_str(delta);
                let partial = partial_json.clone();
                if let Some(AssistantContent::ToolCall(block)) =
                    self.output.content.get_mut(content_index)
                {
                    block.arguments = parse_streaming_json(Some(&partial));
                }
                self.push_tool_call_delta(content_index, Some(delta.to_owned()), events);
            }
            Some("response.function_call_arguments.done") => {
                let Some(Slot::ToolCall { content_index }) =
                    self.get_slot(output_index, SlotKind::ToolCall)
                else {
                    return Ok(());
                };
                let Some(scratch) = self.scratch.get_mut(&content_index) else {
                    return Ok(());
                };
                if scratch.partial_json.is_none() {
                    return Ok(());
                }
                let previous_partial_json = scratch.partial_json.clone().unwrap_or_default();
                let arguments = event
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                scratch.partial_json = Some(arguments.clone());
                if let Some(AssistantContent::ToolCall(block)) =
                    self.output.content.get_mut(content_index)
                {
                    block.arguments = parse_streaming_json(Some(&arguments));
                }
                if let Some(delta) = arguments.strip_prefix(&previous_partial_json)
                    && !delta.is_empty()
                {
                    self.push_tool_call_delta(content_index, Some(delta.to_owned()), events);
                }
            }
            Some("response.custom_tool_call_input.delta") => {
                let Some(Slot::ToolCall { content_index }) =
                    self.get_slot(output_index, SlotKind::ToolCall)
                else {
                    return Ok(());
                };
                let delta = event.get("delta").and_then(Value::as_str).unwrap_or("");
                let Some(scratch) = self.scratch.get_mut(&content_index) else {
                    return Ok(());
                };
                if scratch.custom_input.is_none() {
                    return Ok(());
                }
                if let Some(AssistantContent::ToolCall(block)) =
                    self.output.content.get_mut(content_index)
                {
                    let next_input = format!("{}{delta}", custom_tool_call_input(block, scratch));
                    let delta = append_custom_tool_call_input(block, scratch, &next_input, false)?;
                    self.push_tool_call_delta(content_index, delta, events);
                }
            }
            Some("response.custom_tool_call_input.done") => {
                let Some(Slot::ToolCall { content_index }) =
                    self.get_slot(output_index, SlotKind::ToolCall)
                else {
                    return Ok(());
                };
                let input = event.get("input").and_then(Value::as_str).unwrap_or("");
                let Some(scratch) = self.scratch.get_mut(&content_index) else {
                    return Ok(());
                };
                if scratch.custom_input.is_none() {
                    return Ok(());
                }
                if let Some(AssistantContent::ToolCall(block)) =
                    self.output.content.get_mut(content_index)
                {
                    let delta = append_custom_tool_call_input(block, scratch, input, true)?;
                    self.push_tool_call_delta(content_index, delta, events);
                }
            }
            Some("response.output_item.done") => {
                let Some(item) = event.get("item") else {
                    return Ok(());
                };
                self.apply_message_phase_stop_reason(item);
                let Some(slot) = self.get_or_create_slot(output_index, item, events) else {
                    return Ok(());
                };
                let content_index = slot.content_index();
                match (item.get("type").and_then(Value::as_str), slot) {
                    (Some("reasoning"), Slot::Thinking { .. }) => {
                        let summary_text = join_text_parts(item.get("summary"));
                        let content_text = join_text_parts(item.get("content"));
                        let signature = serde_json::to_string(item).unwrap_or_default();
                        if let Some(AssistantContent::Thinking(block)) =
                            self.output.content.get_mut(content_index)
                        {
                            if !summary_text.is_empty() {
                                block.thinking = summary_text;
                            } else if !content_text.is_empty() {
                                block.thinking = content_text;
                            }
                            block.thinking_signature = Some(signature);
                            if let Some(item_id) = item.get("id").and_then(Value::as_str) {
                                self.reasoning_blocks_by_id
                                    .insert(item_id.to_owned(), content_index);
                            }
                            events.push(StreamEvent::ThinkingEnd {
                                content_index,
                                content: block.thinking.clone(),
                                partial: Arc::new(self.output.clone()),
                            });
                        }
                        self.slots.remove(&output_index);
                    }
                    (Some("message"), Slot::Text { .. }) => {
                        let text = item
                            .get("content")
                            .and_then(Value::as_array)
                            .map(|parts| {
                                parts
                                    .iter()
                                    .map(|part| {
                                        if part.get("type").and_then(Value::as_str)
                                            == Some("output_text")
                                        {
                                            part.get("text").and_then(Value::as_str).unwrap_or("")
                                        } else {
                                            part.get("refusal")
                                                .and_then(Value::as_str)
                                                .unwrap_or("")
                                        }
                                    })
                                    .collect::<Vec<_>>()
                                    .join("")
                            })
                            .unwrap_or_default();
                        let phase =
                            item.get("phase").and_then(Value::as_str).and_then(
                                |phase| match phase {
                                    "commentary" => Some(TextSignaturePhase::Commentary),
                                    "final_answer" => Some(TextSignaturePhase::FinalAnswer),
                                    _ => None,
                                },
                            );
                        let signature = encode_text_signature_v1(
                            item.get("id").and_then(Value::as_str).unwrap_or(""),
                            phase,
                        );
                        if let Some(AssistantContent::Text(block)) =
                            self.output.content.get_mut(content_index)
                        {
                            block.text = text;
                            block.text_signature = Some(signature);
                            events.push(StreamEvent::TextEnd {
                                content_index,
                                content: block.text.clone(),
                                partial: Arc::new(self.output.clone()),
                            });
                        }
                        self.slots.remove(&output_index);
                    }
                    (Some("function_call"), Slot::ToolCall { .. }) => {
                        let has_partial_json = self
                            .scratch
                            .get(&content_index)
                            .is_some_and(|scratch| scratch.partial_json.is_some());
                        if !has_partial_json {
                            return Ok(());
                        }
                        let partial = self
                            .scratch
                            .get(&content_index)
                            .and_then(|scratch| scratch.partial_json.clone())
                            .unwrap_or_default();
                        let arguments = item
                            .get("arguments")
                            .and_then(Value::as_str)
                            .filter(|arguments| !arguments.is_empty())
                            .map(str::to_owned)
                            .or(if partial.is_empty() {
                                Some("{}".to_owned())
                            } else {
                                Some(partial)
                            });
                        if let Some(AssistantContent::ToolCall(block)) =
                            self.output.content.get_mut(content_index)
                        {
                            // `item.arguments || partialJson || "{}"`.
                            let fallback;
                            let source = match &arguments {
                                Some(source) => source,
                                None => {
                                    fallback = "{}".to_owned();
                                    &fallback
                                }
                            };
                            block.arguments = parse_streaming_json(Some(source));
                            // 02bd2d1c6: a namespace on the done item wins.
                            if let Some(namespace) = item.get("namespace").and_then(Value::as_str) {
                                block.namespace = Some(namespace.to_owned());
                            }
                            events.push(StreamEvent::ToolCallEnd {
                                content_index,
                                tool_call: block.clone(),
                                partial: Arc::new(self.output.clone()),
                            });
                        }
                        self.scratch.remove(&content_index);
                        self.slots.remove(&output_index);
                    }
                    (Some("custom_tool_call"), Slot::ToolCall { .. }) => {
                        let has_custom_input = self
                            .scratch
                            .get(&content_index)
                            .is_some_and(|scratch| scratch.custom_input.is_some());
                        if !has_custom_input {
                            return Ok(());
                        }
                        let close_delta = {
                            let Some(scratch) = self.scratch.get_mut(&content_index) else {
                                return Ok(());
                            };
                            let input =
                                item.get("input").and_then(Value::as_str).map(str::to_owned);
                            if let Some(AssistantContent::ToolCall(block)) =
                                self.output.content.get_mut(content_index)
                            {
                                let input =
                                    input.unwrap_or_else(|| custom_tool_call_input(block, scratch));
                                append_custom_tool_call_input(block, scratch, &input, true)?
                            } else {
                                None
                            }
                        };
                        self.push_tool_call_delta(content_index, close_delta, events);
                        if let Some(AssistantContent::ToolCall(block)) =
                            self.output.content.get_mut(content_index)
                        {
                            // 02bd2d1c6: a namespace on the done item wins.
                            if let Some(namespace) = item.get("namespace").and_then(Value::as_str) {
                                block.namespace = Some(namespace.to_owned());
                            }
                            events.push(StreamEvent::ToolCallEnd {
                                content_index,
                                tool_call: block.clone(),
                                partial: Arc::new(self.output.clone()),
                            });
                        }
                        self.scratch.remove(&content_index);
                        self.slots.remove(&output_index);
                    }
                    _ => {}
                }
            }
            Some("response.completed") | Some("response.incomplete") => {
                if let Some(response) = event.get("response") {
                    self.finalize_response(response)?;
                }
            }
            Some("error") => {
                let code = event.get("code");
                let message = event.get("message");
                let render = |value: Option<&Value>| match value {
                    Some(Value::String(text)) => text.clone(),
                    _ => "null".to_owned(),
                };
                return Err(format!("Error Code {}: {}", render(code), render(message)));
            }
            Some("response.failed") => {
                self.saw_terminal_response_event = true;
                let response = event.get("response").cloned().unwrap_or(Value::Null);
                // e5ef8d065: preserve the raw terminal status before erroring.
                self.output.raw_stop_reason = response
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let error = response.get("error").filter(|error| !error.is_null());
                let message = if let Some(error) = error {
                    let code = error
                        .get("code")
                        .and_then(Value::as_str)
                        .filter(|code| !code.is_empty())
                        .unwrap_or("unknown");
                    let text = error
                        .get("message")
                        .and_then(Value::as_str)
                        .filter(|message| !message.is_empty())
                        .unwrap_or("no message");
                    format!("{code}: {text}")
                } else {
                    match response
                        .get("incomplete_details")
                        .and_then(|details| details.get("reason"))
                        .and_then(Value::as_str)
                    {
                        Some(reason) => format!("incomplete: {reason}"),
                        None => "Unknown error (no error details in response)".to_owned(),
                    }
                };
                return Err(message);
            }
            _ => {}
        }
        Ok(())
    }

    /// End-of-stream check (upstream throws after the event loop).
    pub fn finish(self) -> Result<(), String> {
        if !self.saw_terminal_response_event {
            return Err(
                "OpenAI Responses stream ended before a terminal response event".to_owned(),
            );
        }
        // #9974 (`1b2aa0ca0`): the agent runs every tool call in the final
        // message. Refuse calls whose `output_item.done` never arrived —
        // their arguments may be cut off or mixed up when a non-compliant
        // server omits `output_index` (llama.cpp). Finished calls removed
        // their scratch buffers.
        if self.output.stop_reason == StopReason::ToolUse {
            for (index, block) in self.output.content.iter().enumerate() {
                let AssistantContent::ToolCall(call) = block else {
                    continue;
                };
                let unfinished = self.scratch.get(&index).is_some_and(|scratch| {
                    scratch.partial_json.is_some() || scratch.custom_input.is_some()
                });
                if unfinished {
                    return Err(format!(
                        "OpenAI Responses stream completed with an unfinished tool call: {} ({})",
                        call.name, call.id
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Which block kind a slot lookup expects (upstream `getSlot` type parameter).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotKind {
    Thinking,
    Text,
    ToolCall,
}

fn json_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_f64().map(|float| float as u64))
}

/// `item.summary?.map((s) => s.text).join("\n\n") || ""`.
fn join_text_parts(parts: Option<&Value>) -> String {
    parts
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use serde_json::{Value, json};

    use super::*;
    use crate::api::openai_completions::tests as common;
    use crate::types::AssistantRole;

    fn model(extra: Value) -> Model {
        let mut overrides = json!({"api": "openai-responses"});
        overrides
            .as_object_mut()
            .expect("object")
            .extend(extra.as_object().cloned().unwrap_or_default());
        common::make_model(overrides)
    }

    fn assistant(api: &str, provider: &str, model_id: &str, content: Value) -> Message {
        serde_json::from_value(json!({
            "role": "assistant", "content": content,
            "api": api, "provider": provider, "model": model_id,
            "usage": {
                "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0}
            },
            "stopReason": "stop", "timestamp": 0
        }))
        .expect("assistant")
    }

    fn same_model_assistant(content: Value) -> Message {
        assistant("openai-responses", "openai", "gpt-4o", content)
    }

    fn convert(
        model: &Model,
        ctx: &crate::types::TranscriptContext,
        options: &ConvertResponsesMessagesOptions,
    ) -> Vec<Value> {
        convert_responses_messages(model, ctx, &HashSet::from(["openai"]), options)
            .expect("convert")
    }

    fn default_options() -> ConvertResponsesMessagesOptions<'static> {
        ConvertResponsesMessagesOptions::default()
    }

    // -- text signature v1 ----------------------------------------------------

    #[test]
    fn test_text_signature_roundtrip() {
        let encoded = encode_text_signature_v1("msg_1", None);
        assert_eq!(
            parse_text_signature(Some(&encoded)),
            Some(ParsedTextSignature {
                id: "msg_1".to_owned(),
                phase: None,
            })
        );
        let encoded = encode_text_signature_v1("msg_2", Some(TextSignaturePhase::FinalAnswer));
        assert_eq!(
            parse_text_signature(Some(&encoded)),
            Some(ParsedTextSignature {
                id: "msg_2".to_owned(),
                phase: Some(TextSignaturePhase::FinalAnswer),
            })
        );
        // Legacy plain id string.
        assert_eq!(
            parse_text_signature(Some("msg_plain")),
            Some(ParsedTextSignature {
                id: "msg_plain".to_owned(),
                phase: None,
            })
        );
        // Broken JSON and non-v1 payloads fall back to the legacy path.
        assert_eq!(
            parse_text_signature(Some("{not json")),
            Some(ParsedTextSignature {
                id: "{not json".to_owned(),
                phase: None,
            })
        );
        assert_eq!(
            parse_text_signature(Some("{\"v\":2,\"id\":\"x\"}")),
            Some(ParsedTextSignature {
                id: "{\"v\":2,\"id\":\"x\"}".to_owned(),
                phase: None,
            })
        );
        assert_eq!(parse_text_signature(None), None);
    }

    // -- system prompt role ---------------------------------------------------

    #[test]
    fn test_convert_system_prompt_role() {
        let ctx = crate::utils::transcript::normalize_context(&crate::types::Context {
            system_prompt: Some("sys".to_owned()),
            messages: vec![common::user_text("hi")],
            tools: None,
        });

        let out = convert(&model(json!({})), &ctx, &default_options());
        assert_eq!(out[0], json!({"role": "developer", "content": "sys"}));

        let out = convert(
            &model(json!({"reasoning": false})),
            &ctx,
            &default_options(),
        );
        assert_eq!(out[0]["role"], json!("system"));

        let out = convert(
            &model(json!({"compat": {"supportsDeveloperRole": false}})),
            &ctx,
            &default_options(),
        );
        assert_eq!(out[0]["role"], json!("system"));

        // include_system_prompt = false skips it entirely.
        let options = ConvertResponsesMessagesOptions {
            include_system_prompt: false,
            ..ConvertResponsesMessagesOptions::default()
        };
        let out = convert(&model(json!({})), &ctx, &options);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], json!("user"));
    }

    // -- message conversion ---------------------------------------------------

    #[test]
    fn test_convert_user_text() {
        let ctx = common::context(vec![common::user_text("hello")], None);
        let out = convert(&model(json!({})), &ctx, &default_options());
        assert_eq!(
            out,
            vec![json!({"role": "user", "content": [{"type": "input_text", "text": "hello"}]})]
        );
    }

    #[test]
    fn test_convert_message_ids() {
        // No signature → synthetic `msg_pi_N` ids per text block.
        let ctx = common::context(
            vec![same_model_assistant(json!([
                {"type": "text", "text": "one"},
                {"type": "text", "text": "two"}
            ]))],
            None,
        );
        let out = convert(&model(json!({})), &ctx, &default_options());
        assert_eq!(out[0]["id"], json!("msg_pi_0"));
        assert_eq!(out[1]["id"], json!("msg_pi_0_1"));

        // v1 signature supplies the id and phase.
        let signature = encode_text_signature_v1("msg_real", Some(TextSignaturePhase::Commentary));
        let ctx = common::context(
            vec![same_model_assistant(json!([
                {"type": "text", "text": "hi", "textSignature": signature}
            ]))],
            None,
        );
        let out = convert(&model(json!({})), &ctx, &default_options());
        assert_eq!(out[0]["id"], json!("msg_real"));
        assert_eq!(out[0]["phase"], json!("commentary"));

        // Over-64-char ids are hashed down.
        let long_id = "m".repeat(100);
        let signature = encode_text_signature_v1(&long_id, None);
        let ctx = common::context(
            vec![same_model_assistant(json!([
                {"type": "text", "text": "hi", "textSignature": signature}
            ]))],
            None,
        );
        let out = convert(&model(json!({})), &ctx, &default_options());
        let id = out[0]["id"].as_str().expect("id");
        assert!(id.starts_with("msg_"), "expected hashed id, got {id}");
        assert!(id.chars().count() <= 64);
    }

    #[test]
    fn test_convert_thinking_signature_replay() {
        let reasoning_item = json!({"type": "reasoning", "id": "rs_1", "summary": []});
        let ctx = common::context(
            vec![same_model_assistant(json!([
                {"type": "thinking", "thinking": "t", "thinkingSignature": reasoning_item.to_string()}
            ]))],
            None,
        );
        let out = convert(&model(json!({})), &ctx, &default_options());
        assert_eq!(out[0], reasoning_item);
    }

    #[test]
    fn test_convert_tool_call_same_model_keeps_composite_id() {
        let ctx = common::context(
            vec![
                same_model_assistant(json!([
                    {"type": "toolCall", "id": "call_1|fc_item1", "name": "bash", "arguments": {"cmd": "ls"}}
                ])),
                common::tool_result(
                    "call_1|fc_item1",
                    json!([{"type": "text", "text": "ok"}]),
                    json!({}),
                ),
            ],
            None,
        );
        let out = convert(&model(json!({})), &ctx, &default_options());
        assert_eq!(
            out[0],
            json!({
                "type": "function_call", "call_id": "call_1", "name": "bash",
                "arguments": "{\"cmd\":\"ls\"}", "id": "fc_item1"
            })
        );
        assert_eq!(
            out[1],
            json!({"type": "function_call_output", "call_id": "call_1", "output": "ok"})
        );
    }

    #[test]
    fn test_convert_tool_call_foreign_rebuilds_item_id() {
        let ctx = common::context(
            vec![assistant(
                "anthropic-messages",
                "anthropic",
                "claude-x",
                json!([
                    {"type": "toolCall", "id": "call_1|item_9", "name": "bash", "arguments": {}}
                ]),
            )],
            None,
        );
        let out = convert(&model(json!({})), &ctx, &default_options());
        assert_eq!(out[0]["call_id"], json!("call_1"));
        let id = out[0]["id"].as_str().expect("item id");
        assert!(id.starts_with("fc_"), "expected rebuilt fc_ id, got {id}");
    }

    #[test]
    fn test_convert_tool_call_plain_id_drops_item_id() {
        let ctx = common::context(
            vec![assistant(
                "anthropic-messages",
                "anthropic",
                "claude-x",
                json!([
                    {"type": "toolCall", "id": "toolu_1", "name": "bash", "arguments": {}}
                ]),
            )],
            None,
        );
        let out = convert(&model(json!({})), &ctx, &default_options());
        assert_eq!(out[0]["call_id"], json!("toolu_1"));
        assert!(out[0].get("id").is_none(), "item id must be dropped");
    }

    #[test]
    fn test_convert_tool_call_id_sanitize_for_disallowed_provider() {
        // Target provider outside OPENAI_TOOL_CALL_PROVIDERS: the whole id is
        // sanitized to safe chars, capped at 64, trailing underscores trimmed.
        let m = model(json!({"provider": "mistral"}));
        let long = format!("{}!!!___", "a".repeat(70));
        let ctx = common::context(
            vec![
                assistant(
                    "anthropic-messages",
                    "anthropic",
                    "claude-x",
                    json!([
                        {"type": "toolCall", "id": long, "name": "bash", "arguments": {}}
                    ]),
                ),
                common::tool_result(&long, json!([{"type": "text", "text": "ok"}]), json!({})),
            ],
            None,
        );
        let out = convert(&m, &ctx, &default_options());
        let call_id = out[0]["call_id"].as_str().expect("call_id");
        assert!(call_id.chars().count() <= 64);
        assert!(!call_id.ends_with('_'));
        assert!(
            call_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        );
        // Tool result id is normalized identically (pairing preserved).
        assert_eq!(out[1]["call_id"], json!(call_id));
    }

    #[test]
    fn test_convert_tool_result_output_variants() {
        // Text + image with a text-only model → image collapses away.
        let ctx = common::context(
            vec![
                same_model_assistant(json!([
                    {"type": "toolCall", "id": "call_1|fc_1", "name": "bash", "arguments": {}}
                ])),
                common::tool_result(
                    "call_1|fc_1",
                    json!([
                        {"type": "text", "text": "see below"},
                        {"type": "image", "data": "QUJD", "mimeType": "image/png"}
                    ]),
                    json!({}),
                ),
            ],
            None,
        );
        let out = convert(&model(json!({})), &ctx, &default_options());
        // `downgrade_unsupported_images` leaves an omission note in the text.
        assert_eq!(
            out[1]["output"],
            json!("see below\n(tool image omitted: model does not support images)")
        );

        // No text of its own: the omission note becomes the whole output.
        let ctx = common::context(
            vec![
                same_model_assistant(json!([
                    {"type": "toolCall", "id": "call_1|fc_1", "name": "bash", "arguments": {}}
                ])),
                common::tool_result(
                    "call_1|fc_1",
                    json!([{"type": "image", "data": "QUJD", "mimeType": "image/png"}]),
                    json!({}),
                ),
            ],
            None,
        );
        let out = convert(&model(json!({})), &ctx, &default_options());
        assert_eq!(
            out[1]["output"],
            json!("(tool image omitted: model does not support images)")
        );

        // Image-capable model → input_text/input_image block array.
        let out = convert(
            &model(json!({"input": ["text", "image"]})),
            &ctx,
            &default_options(),
        );
        assert_eq!(
            out[1]["output"],
            json!([
                {"type": "input_image", "detail": "auto", "image_url": "data:image/png;base64,QUJD"}
            ])
        );
    }

    /// #9548 (transcript-tool-changes.test.ts "anchors OpenAI additions at
    /// their developer message"): a later system message's `toolsAdded`
    /// becomes a message-anchored `additional_tools` item; the request-level
    /// tools keep only the initial set.
    #[test]
    fn test_system_tool_additions_anchor_at_developer_message() {
        let base_tool = common::tool("base_tool");
        let late_tool = common::tool("late_tool");
        let ctx = crate::utils::transcript::normalize_context(&crate::types::Context {
            system_prompt: None,
            messages: vec![
                Message::System(crate::types::SystemMessage {
                    role: Default::default(),
                    content: crate::types::SystemContent::Text("base prompt".to_owned()),
                    sections: None,
                    tools_added: Some(vec![base_tool]),
                    tools_removed: None,
                    timestamp: 0,
                }),
                common::user_text("before"),
                Message::System(crate::types::SystemMessage {
                    role: Default::default(),
                    content: crate::types::SystemContent::Text("updated guidance".to_owned()),
                    sections: None,
                    tools_added: Some(vec![late_tool.clone()]),
                    tools_removed: None,
                    timestamp: 2,
                }),
            ],
            tools: None,
        });
        let options = ConvertResponsesMessagesOptions {
            supports_mid_convo_system_messages: true,
            supports_additional_tools: true,
            ..ConvertResponsesMessagesOptions::default()
        };
        let out = convert(&model(json!({})), &ctx, &options);
        // Leading prompt + update render, addition anchored between them.
        let developer_texts: Vec<&str> = out
            .iter()
            .filter(|item| {
                item.get("role") == Some(&json!("developer")) && item.get("type").is_none()
            })
            .filter_map(|item| item["content"].as_str())
            .collect();
        assert_eq!(developer_texts, vec!["base prompt", "updated guidance"]);
        let addition = out
            .iter()
            .find(|item| item["type"] == json!("additional_tools"))
            .expect("additional_tools item");
        assert_eq!(addition["role"], json!("developer"));
        assert_eq!(addition["tools"][0]["name"], json!("late_tool"));
        assert!(addition["tools"][0].get("defer_loading").is_none());
    }

    /// #9548 (transcript-tool-changes.test.ts "maps system-message additions
    /// into synthetic tool search"): with `supportsToolSearch` (and no
    /// `supportsAdditionalTools`), the addition becomes the client-executed
    /// `tool_search_call`/`tool_search_output` pair with `defer_loading`.
    #[test]
    fn test_system_tool_additions_map_to_synthetic_tool_search() {
        let base_tool = common::tool("base_tool");
        let late_tool = common::tool("late_tool");
        let ctx = crate::utils::transcript::normalize_context(&crate::types::Context {
            system_prompt: None,
            messages: vec![
                Message::System(crate::types::SystemMessage {
                    role: Default::default(),
                    content: crate::types::SystemContent::Text("base prompt".to_owned()),
                    sections: None,
                    tools_added: Some(vec![base_tool]),
                    tools_removed: None,
                    timestamp: 0,
                }),
                common::user_text("before"),
                Message::System(crate::types::SystemMessage {
                    role: Default::default(),
                    content: crate::types::SystemContent::Text("updated guidance".to_owned()),
                    sections: None,
                    tools_added: Some(vec![late_tool.clone()]),
                    tools_removed: None,
                    timestamp: 2,
                }),
            ],
            tools: None,
        });
        let options = ConvertResponsesMessagesOptions {
            supports_mid_convo_system_messages: true,
            supports_tool_search: true,
            ..ConvertResponsesMessagesOptions::default()
        };
        let out = convert(&model(json!({})), &ctx, &options);
        let call = out
            .iter()
            .find(|item| item["type"] == json!("tool_search_call"))
            .expect("tool_search_call");
        assert_eq!(call["execution"], json!("client"));
        assert_eq!(call["arguments"], json!({"query": "late_tool", "limit": 1}));
        let output = out
            .iter()
            .find(|item| item["type"] == json!("tool_search_output"))
            .expect("tool_search_output");
        assert_eq!(output["call_id"], call["call_id"]);
        assert_eq!(output["tools"][0]["name"], json!("late_tool"));
        assert_eq!(output["tools"][0]["defer_loading"], json!(true));
    }

    /// Without mid-conversation support the transcript collapses: the update
    /// folds into the leading developer message and no addition items are
    /// emitted (transcript-tool-changes.test.ts "folds OpenAI updates into
    /// the leading developer message without native support").
    #[test]
    fn test_system_updates_collapse_without_native_support() {
        let base_tool = common::tool("base_tool");
        let late_tool = common::tool("late_tool");
        let ctx = crate::utils::transcript::normalize_context(&crate::types::Context {
            system_prompt: None,
            messages: vec![
                Message::System(crate::types::SystemMessage {
                    role: Default::default(),
                    content: crate::types::SystemContent::Text("base prompt".to_owned()),
                    sections: None,
                    tools_added: Some(vec![base_tool]),
                    tools_removed: None,
                    timestamp: 0,
                }),
                common::user_text("before"),
                Message::System(crate::types::SystemMessage {
                    role: Default::default(),
                    content: crate::types::SystemContent::Text("updated guidance".to_owned()),
                    sections: None,
                    tools_added: Some(vec![late_tool]),
                    tools_removed: None,
                    timestamp: 2,
                }),
            ],
            tools: None,
        });
        let options = ConvertResponsesMessagesOptions {
            supports_additional_tools: true,
            ..ConvertResponsesMessagesOptions::default()
        };
        let out = convert(&model(json!({})), &ctx, &options);
        // Exactly one developer message (collapsed) and no addition items.
        let developer_texts: Vec<&str> = out
            .iter()
            .filter(|item| {
                item.get("role") == Some(&json!("developer")) && item.get("type").is_none()
            })
            .filter_map(|item| item["content"].as_str())
            .collect();
        assert_eq!(developer_texts, vec!["base prompt\n\nupdated guidance"]);
        assert!(
            out.iter()
                .all(|item| item["type"] != json!("additional_tools"))
        );
        assert!(
            out.iter()
                .all(|item| item["type"] != json!("tool_search_call"))
        );
    }

    // -- namespace replay (02bd2d1c6 @ 4181f66, #7709) -------------------------

    /// Upstream openai-responses-namespace.test.ts: "round-trips a function
    /// namespace received only on output_item.done".
    #[test]
    fn test_namespace_round_trip_function_call() {
        let raw = vec![
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "function_call", "id": "fc_test", "call_id": "call_test",
                            "name": "lookup", "arguments": ""}}),
            json!({"type": "response.output_item.done", "output_index": 0,
                   "item": {"type": "function_call", "id": "fc_test", "call_id": "call_test",
                            "name": "lookup", "arguments": "{\"value\":\"hello\"}",
                            "namespace": "dynamic_tools"}}),
            json!({"type": "response.completed", "response": {"id": "resp_test", "status": "completed"}}),
        ];
        let m = model(json!({}));
        let (_events, result, output) = replay(&m, &no_grammar(), &raw);
        assert_eq!(result, Ok(()));
        let AssistantContent::ToolCall(tool_call) = &output.content[0] else {
            panic!("expected toolCall block");
        };
        assert_eq!(tool_call.id, "call_test|fc_test");
        assert_eq!(tool_call.name, "lookup");
        assert_eq!(
            serde_json::to_value(&tool_call.arguments).expect("json"),
            json!({"value": "hello"})
        );
        assert_eq!(tool_call.namespace.as_deref(), Some("dynamic_tools"));

        // Same-model replay keeps the namespace.
        let ctx = common::context(vec![Message::Assistant(output)], None);
        let out = convert(&m, &ctx, &default_options());
        let function_call = out
            .iter()
            .find(|item| item["type"] == json!("function_call"))
            .expect("function_call");
        assert_eq!(function_call["id"], json!("fc_test"));
        assert_eq!(function_call["call_id"], json!("call_test"));
        assert_eq!(function_call["name"], json!("lookup"));
        assert_eq!(function_call["arguments"], json!("{\"value\":\"hello\"}"));
        assert_eq!(function_call["namespace"], json!("dynamic_tools"));
    }

    /// Upstream: "round-trips a custom-tool namespace received only on
    /// output_item.done".
    #[test]
    fn test_namespace_round_trip_custom_tool_call() {
        let grammar_props = HashMap::from([("query".to_owned(), "input".to_owned())]);
        let raw = vec![
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "custom_tool_call", "id": "ctc_test", "call_id": "call_test",
                            "name": "query", "input": ""}}),
            json!({"type": "response.output_item.done", "output_index": 0,
                   "item": {"type": "custom_tool_call", "id": "ctc_test", "call_id": "call_test",
                            "name": "query", "input": "hello", "namespace": "dynamic_tools"}}),
            json!({"type": "response.completed", "response": {"id": "resp_test", "status": "completed"}}),
        ];
        let m = model(json!({}));
        let (_events, result, output) = replay(&m, &grammar_props, &raw);
        assert_eq!(result, Ok(()));
        let AssistantContent::ToolCall(tool_call) = &output.content[0] else {
            panic!("expected toolCall block");
        };
        assert_eq!(tool_call.id, "call_test|ctc_test");
        assert_eq!(tool_call.name, "query");
        assert_eq!(
            serde_json::to_value(&tool_call.arguments).expect("json"),
            json!({"input": "hello"})
        );
        assert_eq!(tool_call.namespace.as_deref(), Some("dynamic_tools"));

        let ctx = common::context(vec![Message::Assistant(output)], None);
        let options = ConvertResponsesMessagesOptions {
            grammar_tool_input_properties: Some(&grammar_props),
            ..ConvertResponsesMessagesOptions::default()
        };
        let out = convert(&m, &ctx, &options);
        let custom_call = out
            .iter()
            .find(|item| item["type"] == json!("custom_tool_call"))
            .expect("custom_tool_call");
        assert_eq!(custom_call["id"], json!("ctc_test"));
        assert_eq!(custom_call["call_id"], json!("call_test"));
        assert_eq!(custom_call["name"], json!("query"));
        assert_eq!(custom_call["input"], json!("hello"));
        assert_eq!(custom_call["namespace"], json!("dynamic_tools"));
    }

    /// Upstream: "drops namespaces when the target cannot replay their load
    /// items" (different model id, different provider, or a target without
    /// the deferred tool in scope).
    #[test]
    fn test_namespace_dropped_when_target_cannot_replay() {
        let ctx = common::context(
            vec![same_model_assistant(json!([
                {"type": "toolCall", "id": "call_function|fc_test", "name": "lookup",
                 "arguments": {"value": "hello"}, "namespace": "dynamic_tools"},
                {"type": "toolCall", "id": "call_custom|ctc_test", "name": "query",
                 "arguments": {"input": "hello"}, "namespace": "dynamic_tools"}
            ]))],
            None,
        );
        let grammar_props = HashMap::from([("query".to_owned(), "input".to_owned())]);
        let targets = [
            // Same provider/api, different model id.
            model(json!({"id": "gpt-5.2"})),
            // Different provider.
            model(json!({"provider": "azure"})),
            // Codex target.
            model(json!({
                "api": "openai-codex-responses", "provider": "openai-codex",
                "id": "gpt-5.3-codex-spark"
            })),
        ];
        for target in &targets {
            let options = ConvertResponsesMessagesOptions {
                grammar_tool_input_properties: Some(&grammar_props),
                ..ConvertResponsesMessagesOptions::default()
            };
            let out = convert(target, &ctx, &options);
            let function_call = out
                .iter()
                .find(|item| item["type"] == json!("function_call"))
                .expect("function_call");
            let custom_call = out
                .iter()
                .find(|item| item["type"] == json!("custom_tool_call"))
                .expect("custom_tool_call");
            assert!(
                function_call.get("namespace").is_none(),
                "target {}",
                target.id
            );
            assert!(
                custom_call.get("namespace").is_none(),
                "target {}",
                target.id
            );
        }
    }

    /// Upstream: "does not add a namespace to ordinary function calls".
    #[test]
    fn test_namespace_absent_on_ordinary_function_calls() {
        let ctx = common::context(
            vec![same_model_assistant(json!([
                {"type": "toolCall", "id": "call_test|fc_test", "name": "lookup",
                 "arguments": {"value": "hello"}}
            ]))],
            None,
        );
        let out = convert(&model(json!({})), &ctx, &default_options());
        let function_call = out
            .iter()
            .find(|item| item["type"] == json!("function_call"))
            .expect("function_call");
        assert!(function_call.get("namespace").is_none());
    }

    // -- tool conversion ------------------------------------------------------

    #[test]
    fn test_convert_tools_strict_and_defer_loading() {
        let tools = vec![common::tool("bash")];
        // Defaults: strict flag emitted as false (strict mode supported).
        let out = convert_responses_tools(&tools, &ConvertResponsesToolsOptions::default())
            .expect("tools");
        assert_eq!(out[0]["type"], json!("function"));
        assert_eq!(out[0]["strict"], json!(false));

        // Strict mode unsupported → no strict key at all.
        let options = ConvertResponsesToolsOptions {
            supports_strict_mode: Some(false),
            ..ConvertResponsesToolsOptions::default()
        };
        let out = convert_responses_tools(&tools, &options).expect("tools");
        assert!(out[0].get("strict").is_none());

        // defer_loading marks the tool.
        let options = ConvertResponsesToolsOptions {
            tool_search_result: true,
            ..ConvertResponsesToolsOptions::default()
        };
        let out = convert_responses_tools(&tools, &options).expect("tools");
        assert_eq!(out[0]["defer_loading"], json!(true));
    }

    #[test]
    fn test_convert_tools_grammar_custom() {
        let tool: Tool = serde_json::from_value(json!({
            "name": "sql", "description": "d",
            "parameters": {"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]},
            "constrainedSampling": {"type": "grammar", "variants": {"openai_lark": "start: /x/"}}
        }))
        .expect("tool");
        let options = ConvertResponsesToolsOptions {
            supports_open_ai_grammar_tools: Some(true),
            ..ConvertResponsesToolsOptions::default()
        };
        let out = convert_responses_tools(&[tool], &options).expect("tools");
        assert_eq!(out[0]["type"], json!("custom"));
        assert_eq!(
            out[0]["format"],
            json!({"type": "grammar", "syntax": "lark", "definition": "start: /x/"})
        );
    }

    // -- stream processor ------------------------------------------------------

    fn output_for(model: &Model) -> AssistantMessage {
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
            timestamp: 0,
            deferred: None,
            end_turn: None,
            raw_stop_reason: None,
            duration_ms: None,
        }
    }

    fn replay(
        model: &Model,
        grammar_props: &HashMap<String, String>,
        raw_events: &[Value],
    ) -> (Vec<StreamEvent>, Result<(), String>, AssistantMessage) {
        let events = AssistantMessageEventStream::new();
        let mut output = output_for(model);
        let result = {
            let mut processor = ResponsesStreamProcessor::new(
                &mut output,
                model,
                ResponsesStreamOptions {
                    service_tier: None,
                    grammar_tool_input_properties: grammar_props,
                    apply_service_tier_pricing: None,
                    resolve_service_tier: None,
                },
            );
            let mut result: Result<(), String> = Ok(());
            for event in raw_events {
                if let Err(error) = processor.handle_event(event, &events) {
                    result = Err(error);
                    break;
                }
            }
            match result {
                Ok(()) => processor.finish(),
                Err(error) => Err(error),
            }
        };
        events.end(None);
        let collected: Vec<StreamEvent> = futures::executor::block_on(events.collect());
        (collected, result, output)
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

    fn no_grammar() -> HashMap<String, String> {
        HashMap::new()
    }

    #[test]
    fn test_processor_text_flow() {
        let raw = vec![
            json!({"type": "response.created", "response": {"id": "resp_1"}}),
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "message", "id": "msg_1", "role": "assistant", "content": []}}),
            json!({"type": "response.output_text.delta", "output_index": 0, "delta": "Hello"}),
            json!({"type": "response.output_text.delta", "output_index": 0, "delta": " world"}),
            json!({"type": "response.output_item.done", "output_index": 0,
                   "item": {"type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
                            "content": [{"type": "output_text", "text": "Hello world", "annotations": []}]}}),
            json!({"type": "response.completed", "response": {"id": "resp_1", "status": "completed",
                   "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15,
                             "input_tokens_details": {"cached_tokens": 2},
                             "output_tokens_details": {"reasoning_tokens": 0}}}}),
        ];
        let m = model(json!({}));
        let (events, result, output) = replay(&m, &no_grammar(), &raw);
        assert_eq!(result, Ok(()));
        assert_eq!(
            event_kinds(&events),
            vec!["text_start", "text_delta", "text_delta", "text_end"]
        );
        assert_eq!(output.response_id.as_deref(), Some("resp_1"));
        assert_eq!(output.stop_reason, StopReason::Stop);
        let AssistantContent::Text(text) = &output.content[0] else {
            panic!("expected text block");
        };
        assert_eq!(text.text, "Hello world");
        assert_eq!(
            parse_text_signature(text.text_signature.as_deref()),
            Some(ParsedTextSignature {
                id: "msg_1".to_owned(),
                phase: None,
            })
        );
        // Cached tokens are subtracted from input.
        assert_eq!(output.usage.input, 8);
        assert_eq!(output.usage.cache_read, 2);
        assert_eq!(output.usage.output, 5);
        assert_eq!(output.usage.total_tokens, 15);
        assert_eq!(output.usage.reasoning, Some(0));
    }

    #[test]
    fn test_processor_reasoning_signature_backfill() {
        let raw = vec![
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "reasoning", "id": "rs_1"}}),
            json!({"type": "response.reasoning_text.delta", "output_index": 0, "delta": "thinking"}),
            json!({"type": "response.output_item.done", "output_index": 0,
                   "item": {"type": "reasoning", "id": "rs_1",
                            "summary": [{"type": "summary_text", "text": "sum"}]}}),
            // Azure-style: encrypted_content only in the terminal response.
            json!({"type": "response.completed", "response": {"id": "resp_1", "status": "completed",
                   "output": [{"type": "reasoning", "id": "rs_1", "encrypted_content": "enc_sig"}]}}),
        ];
        let m = model(json!({}));
        let (events, result, output) = replay(&m, &no_grammar(), &raw);
        assert_eq!(result, Ok(()));
        assert_eq!(
            event_kinds(&events),
            vec!["thinking_start", "thinking_delta", "thinking_end"]
        );
        let AssistantContent::Thinking(thinking) = &output.content[0] else {
            panic!("expected thinking block");
        };
        assert_eq!(thinking.thinking, "sum");
        let stored: Value =
            serde_json::from_str(thinking.thinking_signature.as_deref().expect("signature"))
                .expect("signature json");
        assert_eq!(stored["encrypted_content"], json!("enc_sig"));
    }

    #[test]
    fn test_processor_function_call_flow() {
        let raw = vec![
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "function_call", "call_id": "call_1", "id": "fc_1",
                            "name": "bash", "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta", "output_index": 0,
                   "delta": "{\"cmd\":"}),
            json!({"type": "response.function_call_arguments.done", "output_index": 0,
                   "arguments": "{\"cmd\":\"ls\"}"}),
            json!({"type": "response.output_item.done", "output_index": 0,
                   "item": {"type": "function_call", "call_id": "call_1", "id": "fc_1",
                            "name": "bash", "arguments": "{\"cmd\":\"ls\"}"}}),
            json!({"type": "response.completed", "response": {"id": "resp_1", "status": "completed"}}),
        ];
        let m = model(json!({}));
        let (events, result, output) = replay(&m, &no_grammar(), &raw);
        assert_eq!(result, Ok(()));
        assert_eq!(
            event_kinds(&events),
            vec![
                "toolcall_start",
                "toolcall_delta",
                "toolcall_delta",
                "toolcall_end"
            ]
        );
        // Tool call present + completed status → toolUse.
        assert_eq!(output.stop_reason, StopReason::ToolUse);
        let AssistantContent::ToolCall(call) = &output.content[0] else {
            panic!("expected tool call block");
        };
        assert_eq!(call.id, "call_1|fc_1");
        assert_eq!(call.name, "bash");
        assert_eq!(
            call.arguments,
            json!({"cmd": "ls"})
                .as_object()
                .cloned()
                .unwrap_or_default()
        );
    }

    #[test]
    fn test_processor_message_phase_final_answer() {
        let raw = vec![
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "message", "id": "msg_1", "role": "assistant",
                            "phase": "final_answer", "content": []}}),
            json!({"type": "response.output_item.done", "output_index": 0,
                   "item": {"type": "message", "id": "msg_1", "role": "assistant",
                            "phase": "final_answer", "status": "completed",
                            "content": [{"type": "output_text", "text": "done", "annotations": []}]}}),
            json!({"type": "response.completed", "response": {"id": "resp_1", "status": "completed"}}),
        ];
        let m = model(json!({}));
        let (_events, result, output) = replay(&m, &no_grammar(), &raw);
        assert_eq!(result, Ok(()));
        let AssistantContent::Text(text) = &output.content[0] else {
            panic!("expected text block");
        };
        assert_eq!(
            parse_text_signature(text.text_signature.as_deref()),
            Some(ParsedTextSignature {
                id: "msg_1".to_owned(),
                phase: Some(TextSignaturePhase::FinalAnswer),
            })
        );
    }

    #[test]
    fn test_processor_custom_tool_call_flow() {
        let grammar_props = HashMap::from([("sql".to_owned(), "query".to_owned())]);
        let raw = vec![
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "custom_tool_call", "call_id": "ctc_1", "id": "ctc_item",
                            "name": "sql", "input": ""}}),
            json!({"type": "response.custom_tool_call_input.delta", "output_index": 0, "delta": "SELECT"}),
            json!({"type": "response.custom_tool_call_input.done", "output_index": 0, "input": "SELECT 1"}),
            json!({"type": "response.output_item.done", "output_index": 0,
                   "item": {"type": "custom_tool_call", "call_id": "ctc_1", "id": "ctc_item",
                            "name": "sql", "input": "SELECT 1"}}),
            json!({"type": "response.completed", "response": {"id": "resp_1", "status": "completed"}}),
        ];
        let m = model(json!({}));
        let (events, result, output) = replay(&m, &grammar_props, &raw);
        assert_eq!(result, Ok(()));
        assert!(event_kinds(&events).contains(&"toolcall_end"));
        let AssistantContent::ToolCall(call) = &output.content[0] else {
            panic!("expected tool call block");
        };
        assert_eq!(call.id, "ctc_1|ctc_item");
        assert_eq!(call.arguments.get("query"), Some(&json!("SELECT 1")));
    }

    /// 8b5899dce (custom-tool face): a `custom_tool_call` item's start block
    /// carries `{[input_property]: item.input}` as its `toolcall_start`
    /// arguments and emits no synthesized first delta — the next delta is
    /// the explicit `custom_tool_call_input.delta`
    /// (openai-responses-shared.ts:505-526).
    #[test]
    fn test_processor_custom_tool_call_start_carries_input_without_first_delta() {
        let grammar_props = HashMap::from([("sql".to_owned(), "query".to_owned())]);
        let raw = vec![
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "custom_tool_call", "call_id": "ctc_9", "id": "ctc_item",
                            "name": "sql", "input": "SELECT"}}),
            json!({"type": "response.custom_tool_call_input.delta", "output_index": 0, "delta": " 1"}),
            json!({"type": "response.custom_tool_call_input.done", "output_index": 0, "input": "SELECT 1"}),
            json!({"type": "response.output_item.done", "output_index": 0,
                   "item": {"type": "custom_tool_call", "call_id": "ctc_9", "id": "ctc_item",
                            "name": "sql", "input": "SELECT 1"}}),
            json!({"type": "response.completed", "response": {"id": "resp_1", "status": "completed"}}),
        ];
        let m = model(json!({}));
        let (events, result, output) = replay(&m, &grammar_props, &raw);
        assert_eq!(result, Ok(()));

        // The start block's arguments already carry the item input.
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
                json!({"query": "SELECT"}).as_object().cloned().unwrap()
            ),
            other => panic!("expected tool call block, got {other:?}"),
        }

        // No synthesized first delta after start: every delta comes from the
        // explicit `custom_tool_call_input.delta`/`.done`, and their
        // concatenation is exactly the complete arguments JSON.
        let deltas: String = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ToolCallDelta { delta, .. } => Some(delta.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&deltas).expect("deltas concatenate to JSON"),
            json!({"query": "SELECT 1"})
        );

        let AssistantContent::ToolCall(call) = &output.content[0] else {
            panic!("expected tool call block");
        };
        assert_eq!(call.arguments.get("query"), Some(&json!("SELECT 1")));
    }

    #[test]
    fn test_processor_error_event() {
        let raw = vec![json!({"type": "error", "code": "rate_limit", "message": "slow down"})];
        let m = model(json!({}));
        let (_events, result, _output) = replay(&m, &no_grammar(), &raw);
        assert_eq!(result, Err("Error Code rate_limit: slow down".to_owned()));
    }

    #[test]
    fn test_processor_response_failed() {
        let raw = vec![json!({"type": "response.failed", "response": {
            "status": "failed", "error": {"code": "server_error", "message": "boom"}
        }})];
        let m = model(json!({}));
        let (_events, result, output) = replay(&m, &no_grammar(), &raw);
        assert_eq!(result, Err("server_error: boom".to_owned()));
        // e5ef8d065: the raw terminal status is preserved.
        assert_eq!(output.raw_stop_reason.as_deref(), Some("failed"));

        // No error object: fall back to incomplete_details.
        let raw = vec![json!({"type": "response.failed", "response": {
            "status": "failed", "incomplete_details": {"reason": "max_output_tokens"}
        }})];
        let (_events, result, _output) = replay(&m, &no_grammar(), &raw);
        assert_eq!(result, Err("incomplete: max_output_tokens".to_owned()));
    }

    #[test]
    fn test_processor_requires_terminal_event() {
        let raw = vec![json!({"type": "response.created", "response": {"id": "resp_1"}})];
        let m = model(json!({}));
        let (_events, result, _output) = replay(&m, &no_grammar(), &raw);
        assert_eq!(
            result,
            Err("OpenAI Responses stream ended before a terminal response event".to_owned())
        );
    }

    /// #9974 (`1b2aa0ca0`): a completed stream whose tool call never
    /// received `output_item.done` fails instead of running cut-off or
    /// mixed-up arguments.
    #[test]
    fn test_processor_rejects_unfinished_tool_call() {
        let raw = vec![
            json!({"type": "response.output_item.added", "sequence_number": 0, "output_index": 0,
                   "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "bash", "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta", "sequence_number": 1, "output_index": 0,
                   "item_id": "fc_1", "delta": "{\"command\":\"rm -rf /tmp/build"}),
            json!({"type": "response.completed", "sequence_number": 2,
                   "response": {"id": "resp_unfinished", "status": "completed"}}),
        ];
        let m = model(json!({}));
        let (_events, result, _output) = replay(&m, &no_grammar(), &raw);
        let error = result.expect_err("unfinished tool calls must fail the stream");
        assert!(
            error.starts_with("OpenAI Responses stream completed with an unfinished tool call:"),
            "got: {error}"
        );
        assert!(error.contains("bash"), "got: {error}");
    }

    /// #9974: parallel tool calls without `output_index` (llama.cpp) used to
    /// run mixed-up calls; the stream now ends with an error.
    #[test]
    fn test_processor_rejects_parallel_tool_calls_without_output_index() {
        let raw = vec![
            json!({"type": "response.output_item.added",
                   "item": {"type": "function_call", "id": "fc_a", "call_id": "call_a", "name": "bash", "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc_a", "delta": "{\"command\":\"echo a\"}"}),
            json!({"type": "response.output_item.added",
                   "item": {"type": "function_call", "id": "fc_b", "call_id": "call_b", "name": "bash", "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc_b", "delta": "{\"command\":\"echo b\"}"}),
            json!({"type": "response.output_item.done",
                   "item": {"type": "function_call", "id": "fc_a", "call_id": "call_a", "name": "bash", "arguments": "{\"command\":\"echo a\"}"}}),
            json!({"type": "response.output_item.done",
                   "item": {"type": "function_call", "id": "fc_b", "call_id": "call_b", "name": "bash", "arguments": "{\"command\":\"echo b\"}"}}),
            json!({"type": "response.completed", "response": {"id": "resp_no_output_index", "status": "completed"}}),
        ];
        let m = model(json!({}));
        let (_events, result, _output) = replay(&m, &no_grammar(), &raw);
        let error = result.expect_err("unfinished tool calls must fail the stream");
        assert!(
            error.starts_with("OpenAI Responses stream completed with an unfinished tool call:"),
            "got: {error}"
        );
    }

    // -- terminal event raw stop reasons -------------------------------------
    // openai-responses-terminal-event.test.ts: e5ef8d065 + 32850ef7c @ 4181f66.

    /// "finalizes completed terminal events as stop" (usage assertions
    /// included, ported byte-for-byte).
    #[test]
    fn finalizes_completed_terminal_events_as_stop() {
        let raw = vec![json!({"type": "response.completed", "response": {
            "id": "resp_completed", "status": "completed",
            "usage": {"input_tokens": 20, "output_tokens": 7, "total_tokens": 27,
                      "input_tokens_details": {"cached_tokens": 2, "cache_write_tokens": 3}}
        }})];
        let m = model(json!({}));
        let (_events, result, output) = replay(&m, &no_grammar(), &raw);
        assert_eq!(result, Ok(()));
        assert_eq!(output.response_id.as_deref(), Some("resp_completed"));
        assert_eq!(output.stop_reason, StopReason::Stop);
        assert_eq!(output.raw_stop_reason.as_deref(), Some("completed"));
        assert_eq!(output.usage.input, 15);
        assert_eq!(output.usage.output, 7);
        assert_eq!(output.usage.cache_read, 2);
        assert_eq!(output.usage.cache_write, 3);
        assert_eq!(output.usage.total_tokens, 27);
    }

    /// "finalizes incomplete terminal events as length stops".
    #[test]
    fn finalizes_incomplete_terminal_events_as_length_stops() {
        let raw = vec![json!({"type": "response.incomplete", "response": {
            "id": "resp_incomplete", "status": "incomplete",
            "incomplete_details": {"reason": "max_output_tokens"},
            "usage": {"input_tokens": 30, "output_tokens": 12, "total_tokens": 42,
                      "input_tokens_details": {"cached_tokens": 5}}
        }})];
        let m = model(json!({}));
        let (_events, result, output) = replay(&m, &no_grammar(), &raw);
        assert_eq!(result, Ok(()));
        assert_eq!(output.response_id.as_deref(), Some("resp_incomplete"));
        assert_eq!(output.stop_reason, StopReason::Length);
        assert_eq!(
            output.raw_stop_reason.as_deref(),
            Some("incomplete.max_output_tokens")
        );
        assert_eq!(output.usage.input, 25);
        assert_eq!(output.usage.output, 12);
        assert_eq!(output.usage.cache_read, 5);
        assert_eq!(output.usage.cache_write, 0);
        assert_eq!(output.usage.total_tokens, 42);
        // V14-06 FR-I (:591): a length stop maps to no error message and the
        // plain assignment clears any earlier value — upstream `delete
        // output.errorMessage`.
        assert_eq!(output.error_message, None);
    }

    /// "finalizes content-filtered incomplete responses as non-retryable
    /// errors".
    #[test]
    fn finalizes_content_filtered_incomplete_responses_as_non_retryable_errors() {
        let raw = vec![json!({"type": "response.incomplete", "response": {
            "id": "resp_incomplete", "status": "incomplete",
            "incomplete_details": {"reason": "content_filter"},
            "usage": {"input_tokens": 30, "output_tokens": 12, "total_tokens": 42,
                      "input_tokens_details": {"cached_tokens": 5}}
        }})];
        let m = model(json!({}));
        let (_events, result, output) = replay(&m, &no_grammar(), &raw);
        assert_eq!(result, Ok(()));
        assert_eq!(output.stop_reason, StopReason::Error);
        assert_eq!(
            output.raw_stop_reason.as_deref(),
            Some("incomplete.content_filter")
        );
        assert_eq!(
            output.error_message.as_deref(),
            Some("Response incomplete: content_filter")
        );
    }

    /// "preserves unknown provider incomplete reasons as non-retryable
    /// errors".
    #[test]
    fn preserves_unknown_provider_incomplete_reasons_as_non_retryable_errors() {
        let raw = vec![json!({"type": "response.incomplete", "response": {
            "id": "resp_incomplete", "status": "incomplete",
            "incomplete_details": {"reason": "max_time_limit"},
            "usage": {"input_tokens": 30, "output_tokens": 12, "total_tokens": 42,
                      "input_tokens_details": {"cached_tokens": 5}}
        }})];
        let m = model(json!({}));
        let (_events, result, output) = replay(&m, &no_grammar(), &raw);
        assert_eq!(result, Ok(()));
        assert_eq!(output.stop_reason, StopReason::Error);
        assert_eq!(
            output.raw_stop_reason.as_deref(),
            Some("incomplete.max_time_limit")
        );
        assert_eq!(
            output.error_message.as_deref(),
            Some("Response incomplete: max_time_limit")
        );
    }
}
