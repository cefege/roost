//! Port of `packages/ai/src/api/pi-messages.ts` @ pi 0.82.1 (2efa728).
//!
//! pi-messages adapter: streams pi's own message protocol to a backend. The
//! request is a single POST of `{ model, context, options }` to
//! `<baseUrl>/messages` (`?debug=1` when [`PiMessagesOptions::debug`]); the
//! response is an SSE stream of serialized assistant-message events plus a
//! terminal `done`/`error` event. This is the wire protocol spoken by the
//! Radius gateway, usable by any backend via a models.json custom provider
//! with `"api": "rpi-messages"` (legacy `"pi-messages"` still accepted).
//!
//! `pi-messages.lazy.ts` (`lazyApi(() => import("./pi-messages.ts"))`) is a
//! code-splitting shim; Rust links statically, so [`PiMessages`] is always
//! available and no lazy-registration equivalent exists.
//!
//! Intentional differences (upstream deviations, see D-021):
//! - `streamSimple`'s `toolChoice`/`debug` smuggling (upstream reads them off
//!   the `SimpleStreamOptions` object) is not ported: rpi's
//!   `SimpleStreamOptions` struct has no such fields. They are available via
//!   [`PiMessagesOptions`] on direct [`stream`] calls.
//! - The bespoke SSE reader is ported as written (first `data:` line per
//!   `\n\n`-separated block, `\r\n` normalization, trailing-block flush),
//!   not via the shared [`crate::api::sse::SseDecoder`]; a malformed `data:`
//!   payload fails the stream with the `serde_json` error text (upstream
//!   surfaces the JS `SyntaxError` message instead).
//! - JS sparse-array / loose-cast semantics have no Rust equivalent: a
//!   `contentIndex` beyond the current content length pads with empty text
//!   blocks, and deltas addressed at a block of the wrong type are dropped
//!   (upstream would throw a `TypeError`, caught into an error event).
//! - `statusText` in error messages/diagnostics uses the canonical reason
//!   phrase (`reqwest::StatusCode::canonical_reason`), not the raw reason
//!   phrase sent by the server.
//! - The legacy env var is renamed `PI_CACHE_RETENTION` →
//!   `RPI_CACHE_RETENTION` (requirements §5.5); unlike the anthropic adapter,
//!   an unset retention stays unset (backend default), only the explicit
//!   value and the env `"long"` opt-in are mapped.
//! - `truncateDiagnosticString` counts Unicode scalars; JS slices UTF-16
//!   code units (identical for the BMP).
//! - `Response.body` nullability is not checked (reqwest always exposes a
//!   body stream; the `"response has no body"` branch is unreachable).

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::api::stream_cancel::{StreamNext, next_chunk_or_cancelled};
use crate::models::ProviderStreams;
use crate::types::{
    AssistantContent, AssistantMessage, AssistantMessageDiagnostic, AssistantRole, CacheRetention,
    DiagnosticErrorInfo, DoneReason, ErrorReason, Model, NumberOrString, ProviderEnv,
    ProviderResponse, SimpleStreamOptions, StopReason, StreamEvent, StreamOptions, TextContent,
    ThinkingContent, ThinkingLevel, ToolCall, TranscriptContext, Usage, tagged_tool_call,
};
use crate::utils::custom_fetch::send_provider_request;
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::json_parse::parse_streaming_json;
use crate::utils::provider_env::get_provider_env_value;

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// `PiMessagesOptions` — `StreamOptions` plus pi-messages extras.
#[derive(Debug, Clone, Default)]
pub struct PiMessagesOptions {
    pub stream: StreamOptions,
    pub reasoning: Option<ThinkingLevel>,
    /// Raw `toolChoice` JSON: `"auto" | "none" | "required" |
    /// { type: "function", function: { name } }`.
    pub tool_choice: Option<Value>,
    /// Ask the backend for debug metadata (e.g. routing response headers);
    /// appends `?debug=1` to the request URL.
    pub debug: Option<bool>,
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// `PiMessagesRewriteImpact` — impact summary of a server-side message
/// rewrite (e.g. a gateway policy).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiMessagesRewriteImpact {
    pub policy_id: String,
    pub policy_version: u64,
    pub changed: bool,
    pub token_count_change: i64,
    pub message_count_change: i64,
    pub system_prompt_changed: bool,
}

/// `PiMessagesEvent` — serialized assistant-message event as sent by a
/// pi-messages backend.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum PiMessagesEvent {
    #[serde(rename = "start")]
    Start,
    #[serde(rename = "text_start")]
    TextStart { content_index: usize },
    #[serde(rename = "text_delta")]
    TextDelta { content_index: usize, delta: String },
    #[serde(rename = "text_end")]
    TextEnd {
        content_index: usize,
        content: String,
        content_signature: Option<String>,
    },
    #[serde(rename = "thinking_start")]
    ThinkingStart { content_index: usize },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta { content_index: usize, delta: String },
    #[serde(rename = "thinking_end")]
    ThinkingEnd {
        content_index: usize,
        content: String,
        content_signature: Option<String>,
        redacted: Option<bool>,
    },
    #[serde(rename = "toolcall_start")]
    ToolCallStart {
        content_index: usize,
        id: String,
        tool_name: String,
    },
    #[serde(rename = "toolcall_delta")]
    ToolCallDelta { content_index: usize, delta: String },
    #[serde(rename = "toolcall_end")]
    ToolCallEnd {
        content_index: usize,
        #[serde(with = "tagged_tool_call")]
        tool_call: ToolCall,
    },
    #[serde(rename = "done")]
    Done {
        reason: DoneReason,
        usage: Usage,
        response_id: Option<String>,
        rewrite: Option<PiMessagesRewriteImpact>,
    },
    #[serde(rename = "error")]
    Error {
        reason: ErrorReason,
        usage: Usage,
        error_message: Option<String>,
        response_id: Option<String>,
        rewrite: Option<PiMessagesRewriteImpact>,
    },
}

/// `PiMessagesResponseError` — non-2xx response with a machine-readable code
/// and redacted diagnostic details (attached to the error event's message as
/// a `pi_messages_response_failure` diagnostic).
#[derive(Debug, Clone)]
pub struct PiMessagesResponseError {
    pub message: String,
    pub code: Option<String>,
    pub diagnostic_details: Map<String, Value>,
}

/// A streaming failure: the upstream `Error.message`, plus the structured
/// response error when the failure was a non-2xx response.
struct StreamFailure {
    message: String,
    response_error: Option<PiMessagesResponseError>,
}

impl StreamFailure {
    fn plain(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            response_error: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Error body parsing / formatting
// ---------------------------------------------------------------------------

/// `parsePiMessagesErrorBody`: the parsed `error` object, only when the body
/// is JSON with a non-array object `error` member.
fn parse_pi_messages_error_body(body: &str) -> Option<Value> {
    let parsed: Value = serde_json::from_str(body).ok()?;
    let error = parsed.get("error")?;
    error.as_object()?;
    Some(error.clone())
}

/// `truncateDiagnosticString`.
fn truncate_diagnostic_string(value: &str) -> String {
    const MAX_LENGTH: usize = 8192;
    if value.chars().count() > MAX_LENGTH {
        format!("{}…", value.chars().take(MAX_LENGTH).collect::<String>())
    } else {
        value.to_owned()
    }
}

fn error_body_field<'a>(error_body: Option<&'a Value>, field: &str) -> Option<&'a str> {
    error_body
        .and_then(|error| error.get(field))
        .and_then(Value::as_str)
}

/// `formatPiMessagesResponseError`:
/// `{status} {statusText}: {error.message ?? body}{code ? ` (${code})` : ""}`.
fn format_pi_messages_response_error(
    status: u16,
    status_text: &str,
    body: &str,
    error_body: Option<&Value>,
) -> String {
    let message = error_body_field(error_body, "message");
    let code = error_body_field(error_body, "code");
    let suffix = message.unwrap_or(body);
    let code_suffix = code.map(|code| format!(" ({code})")).unwrap_or_default();
    format!("{status} {status_text}: {suffix}{code_suffix}")
}

/// `createPiMessagesResponseError`.
fn create_pi_messages_response_error(
    model: &Model,
    url: &reqwest::Url,
    status: u16,
    status_text: &str,
    body: &str,
) -> PiMessagesResponseError {
    let error_body = parse_pi_messages_error_body(body);
    let code = error_body_field(error_body.as_ref(), "code").map(str::to_owned);
    let mut details = Map::new();
    details.insert("version".to_owned(), json!(1));
    details.insert("provider".to_owned(), json!(model.provider));
    details.insert("model".to_owned(), json!(model.id));
    details.insert("url".to_owned(), json!(url.as_str()));
    details.insert("status".to_owned(), json!(status));
    details.insert("statusText".to_owned(), json!(status_text));
    if let Some(error) = &error_body {
        details.insert("error".to_owned(), error.clone());
    }
    if error_body.is_none() {
        details.insert("body".to_owned(), json!(truncate_diagnostic_string(body)));
    }
    details.insert("timestampMs".to_owned(), json!(now_ms()));
    PiMessagesResponseError {
        message: format_pi_messages_response_error(status, status_text, body, error_body.as_ref()),
        code,
        diagnostic_details: details,
    }
}

// ---------------------------------------------------------------------------
// Diagnostics (utils/diagnostics.ts helpers, inlined per call shape)
// ---------------------------------------------------------------------------

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `appendAssistantMessageDiagnostic`.
fn append_diagnostic(message: &mut AssistantMessage, diagnostic: AssistantMessageDiagnostic) {
    message
        .diagnostics
        .get_or_insert_with(Vec::new)
        .push(diagnostic);
}

/// `appendRewriteDiagnostic`: a `pi_messages_rewrite` diagnostic carrying the
/// rewrite impact as details (no `error` member, matching upstream).
fn append_rewrite_diagnostic(
    message: &mut AssistantMessage,
    rewrite: Option<&PiMessagesRewriteImpact>,
) {
    let Some(rewrite) = rewrite else { return };
    let Ok(Value::Object(details)) = serde_json::to_value(rewrite) else {
        return;
    };
    append_diagnostic(
        message,
        AssistantMessageDiagnostic {
            kind: "pi_messages_rewrite".to_owned(),
            timestamp: now_ms(),
            error: None,
            details: Some(details),
        },
    );
}

// ---------------------------------------------------------------------------
// Cache retention
// ---------------------------------------------------------------------------

/// `resolveCacheRetention` (pi-messages): an explicit value wins; otherwise
/// only the legacy env opt-in maps to `"long"` — unset stays unset so the
/// backend default applies.
fn resolve_cache_retention(
    cache_retention: Option<CacheRetention>,
    env: Option<&ProviderEnv>,
) -> Option<CacheRetention> {
    if cache_retention.is_some() {
        return cache_retention;
    }
    if get_provider_env_value("RPI_CACHE_RETENTION", env).as_deref() == Some("long") {
        return Some(CacheRetention::Long);
    }
    None
}

// ---------------------------------------------------------------------------
// Event converter (`createEventConverter`)
// ---------------------------------------------------------------------------

fn initial_partial(model: &Model) -> AssistantMessage {
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

/// JS assigns `partial.content[contentIndex]` directly (sparse arrays
/// allowed). Rust pads with empty text blocks so the index is addressable;
/// well-formed backends always address `content.len()` or an existing index.
fn content_slot(content: &mut Vec<AssistantContent>, index: usize) -> &mut AssistantContent {
    while content.len() <= index {
        content.push(AssistantContent::Text(TextContent::default()));
    }
    &mut content[index]
}

/// `createEventConverter`: folds pi-messages events into a partial
/// [`AssistantMessage`] and emits the corresponding [`StreamEvent`]s.
struct EventConverter {
    partial: AssistantMessage,
    tool_json: HashMap<usize, String>,
}

impl EventConverter {
    fn new(model: &Model) -> Self {
        Self {
            partial: initial_partial(model),
            tool_json: HashMap::new(),
        }
    }

    fn convert(&mut self, event: PiMessagesEvent) -> StreamEvent {
        match event {
            PiMessagesEvent::Done {
                reason,
                usage,
                response_id,
                rewrite,
            } => {
                self.partial.stop_reason = match reason {
                    DoneReason::Stop => StopReason::Stop,
                    DoneReason::Length => StopReason::Length,
                    DoneReason::ToolUse => StopReason::ToolUse,
                    // Placeholder variant (R2.1.1); no rpi provider produces
                    // it, but the mapping is explicit rather than swallowed.
                    DoneReason::Deferred => StopReason::Deferred,
                };
                self.partial.usage = usage;
                self.partial.response_id = response_id;
                append_rewrite_diagnostic(&mut self.partial, rewrite.as_ref());
                StreamEvent::Done {
                    reason,
                    message: self.partial.clone(),
                }
            }
            PiMessagesEvent::Error {
                reason,
                usage,
                error_message,
                response_id,
                rewrite,
            } => {
                self.partial.stop_reason = match reason {
                    ErrorReason::Aborted => StopReason::Aborted,
                    ErrorReason::Error => StopReason::Error,
                };
                self.partial.usage = usage;
                self.partial.error_message = error_message;
                self.partial.response_id = response_id;
                append_rewrite_diagnostic(&mut self.partial, rewrite.as_ref());
                StreamEvent::Error {
                    reason,
                    error: self.partial.clone(),
                }
            }
            PiMessagesEvent::Start => StreamEvent::Start {
                partial: Arc::new(self.partial.clone()),
            },
            PiMessagesEvent::TextStart { content_index } => {
                *content_slot(&mut self.partial.content, content_index) =
                    AssistantContent::Text(TextContent::default());
                StreamEvent::TextStart {
                    content_index,
                    partial: Arc::new(self.partial.clone()),
                }
            }
            PiMessagesEvent::TextDelta {
                content_index,
                delta,
            } => {
                // Upstream's `as` cast would throw on a non-text block; rpi
                // drops the delta instead of failing the stream.
                if let AssistantContent::Text(text) =
                    content_slot(&mut self.partial.content, content_index)
                {
                    text.text.push_str(&delta);
                }
                StreamEvent::TextDelta {
                    content_index,
                    delta,
                    partial: Arc::new(self.partial.clone()),
                }
            }
            PiMessagesEvent::TextEnd {
                content_index,
                content,
                content_signature,
            } => {
                let slot = content_slot(&mut self.partial.content, content_index);
                match slot {
                    AssistantContent::Text(text) => {
                        text.text = content.clone();
                        text.text_signature = content_signature;
                    }
                    other => {
                        *other = AssistantContent::Text(TextContent {
                            text: content.clone(),
                            text_signature: content_signature,
                        });
                    }
                }
                StreamEvent::TextEnd {
                    content_index,
                    content,
                    partial: Arc::new(self.partial.clone()),
                }
            }
            PiMessagesEvent::ThinkingStart { content_index } => {
                *content_slot(&mut self.partial.content, content_index) =
                    AssistantContent::Thinking(ThinkingContent::default());
                StreamEvent::ThinkingStart {
                    content_index,
                    partial: Arc::new(self.partial.clone()),
                }
            }
            PiMessagesEvent::ThinkingDelta {
                content_index,
                delta,
            } => {
                if let AssistantContent::Thinking(thinking) =
                    content_slot(&mut self.partial.content, content_index)
                {
                    thinking.thinking.push_str(&delta);
                }
                StreamEvent::ThinkingDelta {
                    content_index,
                    delta,
                    partial: Arc::new(self.partial.clone()),
                }
            }
            PiMessagesEvent::ThinkingEnd {
                content_index,
                content,
                content_signature,
                redacted,
            } => {
                let slot = content_slot(&mut self.partial.content, content_index);
                match slot {
                    AssistantContent::Thinking(thinking) => {
                        thinking.thinking = content.clone();
                        thinking.thinking_signature = content_signature;
                        thinking.redacted = redacted;
                    }
                    other => {
                        *other = AssistantContent::Thinking(ThinkingContent {
                            thinking: content.clone(),
                            thinking_signature: content_signature,
                            redacted,
                        });
                    }
                }
                StreamEvent::ThinkingEnd {
                    content_index,
                    content,
                    partial: Arc::new(self.partial.clone()),
                }
            }
            PiMessagesEvent::ToolCallStart {
                content_index,
                id,
                tool_name,
            } => {
                *content_slot(&mut self.partial.content, content_index) =
                    AssistantContent::ToolCall(ToolCall {
                        id,
                        name: tool_name,
                        arguments: Map::new(),
                        thought_signature: None,
                        namespace: None,
                    });
                self.tool_json.insert(content_index, String::new());
                StreamEvent::ToolCallStart {
                    content_index,
                    partial: Arc::new(self.partial.clone()),
                }
            }
            PiMessagesEvent::ToolCallDelta {
                content_index,
                delta,
            } => {
                let json = format!(
                    "{}{delta}",
                    self.tool_json
                        .get(&content_index)
                        .cloned()
                        .unwrap_or_default()
                );
                self.tool_json.insert(content_index, json.clone());
                if let AssistantContent::ToolCall(tool_call) =
                    content_slot(&mut self.partial.content, content_index)
                {
                    tool_call.arguments = parse_streaming_json(Some(&json));
                }
                StreamEvent::ToolCallDelta {
                    content_index,
                    delta,
                    partial: Arc::new(self.partial.clone()),
                }
            }
            PiMessagesEvent::ToolCallEnd {
                content_index,
                tool_call,
            } => {
                // `Object.assign(partial.content[i], event.toolCall)`: the end
                // event's fields win; a locally-held thought signature survives.
                let slot = content_slot(&mut self.partial.content, content_index);
                match slot {
                    AssistantContent::ToolCall(existing) => {
                        existing.id = tool_call.id;
                        existing.name = tool_call.name;
                        existing.arguments = tool_call.arguments;
                    }
                    other => {
                        *other = AssistantContent::ToolCall(tool_call);
                    }
                }
                self.tool_json.remove(&content_index);
                let AssistantContent::ToolCall(final_call) = &self.partial.content[content_index]
                else {
                    unreachable!("slot was just assigned a ToolCall");
                };
                StreamEvent::ToolCallEnd {
                    content_index,
                    tool_call: final_call.clone(),
                    partial: Arc::new(self.partial.clone()),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SSE reader (`readPiMessagesEvents` / `parsePiMessagesEvent`)
// ---------------------------------------------------------------------------

/// Incremental UTF-8 decode mirroring `TextDecoder` with `{ stream: true }`
/// (invalid sequences become U+FFFD; an incomplete tail stays buffered
/// unless `flush`). Same logic as `api::sse`, duplicated because the
/// pi-messages frame splitter differs from the generic SSE decoder.
fn decode_utf8_streaming(tail: &mut Vec<u8>, flush: bool) -> String {
    let mut decoded = String::new();
    let mut bytes = std::mem::take(tail);
    let mut offset = 0;
    while offset < bytes.len() {
        match std::str::from_utf8(&bytes[offset..]) {
            Ok(valid) => {
                decoded.push_str(valid);
                offset = bytes.len();
            }
            Err(error) => {
                let valid_up_to = error.valid_up_to();
                // invariant: from_utf8 guarantees bytes[..valid_up_to] is valid UTF-8
                decoded.push_str(
                    std::str::from_utf8(&bytes[offset..offset + valid_up_to]).unwrap_or(""),
                );
                offset += valid_up_to;
                match error.error_len() {
                    Some(invalid_len) => {
                        decoded.push('\u{FFFD}');
                        offset += invalid_len;
                    }
                    None => {
                        if flush {
                            decoded.push('\u{FFFD}');
                            offset = bytes.len();
                        } else {
                            break;
                        }
                    }
                }
            }
        }
    }
    bytes.drain(..offset);
    *tail = bytes;
    decoded
}

/// `parsePiMessagesEvent`: the first `data:` line of a raw block, trimmed;
/// empty or `[DONE]` yields no event. Malformed JSON is an error (upstream
/// `JSON.parse` throws).
fn parse_pi_messages_event(raw: &str) -> Result<Option<PiMessagesEvent>, String> {
    let data = raw
        .split('\n')
        .find(|line| line.starts_with("data:"))
        .map(|line| line["data:".len()..].trim());
    match data {
        None | Some("") | Some("[DONE]") => Ok(None),
        Some(data) => serde_json::from_str(data)
            .map(Some)
            .map_err(|error| error.to_string()),
    }
}

/// `readPiMessagesEvents`: buffer chunks, normalize `\r\n` to `\n`, and split
/// events on blank lines; a non-empty tail is parsed at end-of-stream.
///
/// P2 (rc.12 review): like the shared [`SseDecoder`](crate::api::sse), the
/// *unterminated* tail is capped — a gateway streaming bytes without a
/// blank-line separator would otherwise grow the buffer unbounded until the
/// idle timeout (same memory-amplification class rc.11 P2-4 closed for SSE).
const MAX_PI_MESSAGES_TAIL_BYTES: usize = 1024 * 1024;

#[derive(Default)]
struct PiMessagesEventReader {
    utf8_tail: Vec<u8>,
    buffer: String,
}

impl PiMessagesEventReader {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<PiMessagesEvent>, String> {
        self.utf8_tail.extend_from_slice(bytes);
        let decoded = decode_utf8_streaming(&mut self.utf8_tail, false);
        self.buffer.push_str(&decoded);
        self.buffer = self.buffer.replace("\r\n", "\n");
        let mut events = Vec::new();
        while let Some(split) = self.buffer.find("\n\n") {
            let raw = self.buffer[..split].to_owned();
            self.buffer.drain(..split + 2);
            if let Some(event) = parse_pi_messages_event(&raw)? {
                events.push(event);
            }
        }
        self.check_tail_bound()?;
        Ok(events)
    }

    fn finish(mut self) -> Result<Vec<PiMessagesEvent>, String> {
        let decoded = decode_utf8_streaming(&mut self.utf8_tail, true);
        self.buffer.push_str(&decoded);
        self.buffer = self.buffer.replace("\r\n", "\n");
        self.check_tail_bound()?;
        if self.buffer.trim().is_empty() {
            return Ok(Vec::new());
        }
        let raw = std::mem::take(&mut self.buffer);
        Ok(parse_pi_messages_event(&raw)?.into_iter().collect())
    }

    /// The unterminated tail must stay bounded (see
    /// [`MAX_PI_MESSAGES_TAIL_BYTES`]).
    fn check_tail_bound(&self) -> Result<(), String> {
        if self.buffer.len() > MAX_PI_MESSAGES_TAIL_BYTES {
            return Err(format!(
                "pi-messages event exceeds {MAX_PI_MESSAGES_TAIL_BYTES} bytes \
                 without a blank-line separator — malformed stream"
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Error event (`createErrorEvent`)
// ---------------------------------------------------------------------------

/// `createErrorEvent` message construction: a fresh zero-usage assistant
/// message (not the partial), with a `pi_messages_response_failure`
/// diagnostic for non-abort HTTP response failures.
fn create_error_message(model: &Model, failure: StreamFailure, aborted: bool) -> AssistantMessage {
    let mut message = AssistantMessage {
        stop_reason: if aborted {
            StopReason::Aborted
        } else {
            StopReason::Error
        },
        error_message: Some(failure.message.clone()),
        ..initial_partial(model)
    };
    if !aborted && let Some(response_error) = failure.response_error {
        append_diagnostic(
            &mut message,
            AssistantMessageDiagnostic {
                kind: "pi_messages_response_failure".to_owned(),
                timestamp: now_ms(),
                error: Some(DiagnosticErrorInfo {
                    name: Some("PiMessagesResponseError".to_owned()),
                    message: response_error.message,
                    stack: None,
                    code: response_error.code.map(NumberOrString::String),
                }),
                details: Some(response_error.diagnostic_details),
            },
        );
    }
    message
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

/// The streaming body: everything that runs inside upstream's async IIFE.
/// Terminal `done`/`error` events are pushed before returning; `Err` carries
/// the upstream `Error.message`.
///
/// `result_large_err` allow: `StreamFailure` (message + the structured
/// response error) is crate-private and threaded through every helper in
/// this adapter; boxing it would ripple through the whole error plumbing
/// for zero behavioral gain. Pinned here when clippy 1.98 raised the
/// threshold check on CI (local gate runs 1.97).
#[allow(clippy::result_large_err)]
async fn run(
    model: &Model,
    context: &TranscriptContext,
    options: &PiMessagesOptions,
    events: &AssistantMessageEventStream,
) -> Result<(), StreamFailure> {
    let Some(api_key) = options.stream.api_key.as_deref() else {
        return Err(StreamFailure::plain(format!(
            "No API key provided for provider \"{}\"",
            model.provider
        )));
    };

    let mut url = reqwest::Url::parse(&format!(
        "{}/messages",
        model.base_url.trim_end_matches('/')
    ))
    .map_err(|error| StreamFailure::plain(error.to_string()))?;
    if options.debug == Some(true) {
        url.query_pairs_mut().append_pair("debug", "1");
    }

    let mut request_options = Map::new();
    if let Some(temperature) = options.stream.temperature {
        request_options.insert("temperature".to_owned(), json!(temperature));
    }
    if let Some(max_tokens) = options.stream.max_tokens {
        request_options.insert("maxTokens".to_owned(), json!(max_tokens));
    }
    if let Some(reasoning) = options.reasoning {
        request_options.insert("reasoning".to_owned(), json!(reasoning));
    }
    if let Some(cache_retention) =
        resolve_cache_retention(options.stream.cache_retention, options.stream.env.as_ref())
    {
        request_options.insert("cacheRetention".to_owned(), json!(cache_retention));
    }
    if let Some(session_id) = &options.stream.session_id {
        request_options.insert("sessionId".to_owned(), json!(session_id));
    }
    if let Some(tool_choice) = &options.tool_choice {
        request_options.insert("toolChoice".to_owned(), tool_choice.clone());
    }
    let mut payload = json!({
        "model": model.id,
        "context": context,
        "options": request_options,
    });
    if let Some(on_payload) = &options.stream.on_payload
        && let Some(next_payload) = on_payload(payload.clone(), model).await
    {
        payload = next_payload;
    }

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        reqwest::header::HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|error| StreamFailure::plain(error.to_string()))?,
    );
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    // `...providerHeadersToRecord(options?.headers)`: custom headers override
    // the defaults; `None` suppression markers are dropped at the boundary.
    if let Some(custom) = &options.stream.headers {
        for (name, value) in custom {
            let Some(value) = value else { continue };
            let header_name =
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
                    StreamFailure::plain(format!("Invalid header name {name:?}: {error}"))
                })?;
            let header_value = reqwest::header::HeaderValue::from_str(value).map_err(|error| {
                StreamFailure::plain(format!("Invalid value for header {name:?}: {error}"))
            })?;
            headers.insert(header_name, header_value);
        }
    }

    let mut client_builder =
        crate::api::http_client::adapter_client_builder(options.stream.env.as_ref(), url.as_str())
            .map_err(StreamFailure::plain)?;
    // Idle-timeout semantics (upstream undici headersTimeout/bodyTimeout;
    // see api::stream_timeouts) — never a total-request deadline.
    if let Some(timeout_ms) = options.stream.timeout_ms {
        client_builder =
            client_builder.connect_timeout(std::time::Duration::from_millis(timeout_ms));
    }
    let client = client_builder
        .build()
        .map_err(|error| StreamFailure::plain(error.to_string()))?;

    let body =
        serde_json::to_string(&payload).map_err(|error| StreamFailure::plain(error.to_string()))?;
    let request = client.post(url.clone()).headers(headers).body(body);
    let signal = options.stream.signal.clone();
    // 027a58479 (R2.7.4): per-request custom fetch channel; `None` keeps the
    // reqwest default path unchanged.
    let response = send_provider_request(
        request,
        options.stream.fetch.as_ref(),
        signal.as_ref(),
        options.stream.timeout_ms,
    )
    .await
    .map_err(|error| StreamFailure::plain(error.message()))?;

    // Upstream invokes `onResponse` before the `response.ok` check.
    if let Some(on_response) = &options.stream.on_response {
        on_response(
            ProviderResponse {
                status: response.status().as_u16(),
                headers: crate::utils::headers::headers_to_record(response.headers()),
            },
            model,
        )
        .await;
    }

    if !response.status().is_success() {
        let status = response.status();
        let status_text = status.canonical_reason().unwrap_or("").to_owned();
        let body = response.text().await.unwrap_or_default();
        let error =
            create_pi_messages_response_error(model, &url, status.as_u16(), &status_text, &body);
        return Err(StreamFailure {
            message: error.message.clone(),
            response_error: Some(error),
        });
    }

    let mut converter = EventConverter::new(model);
    let mut reader = PiMessagesEventReader::default();
    // Inter-chunk idle timeout (upstream bodyTimeout; see
    // api::stream_timeouts).
    let mut byte_stream =
        crate::api::stream_timeouts::wrap(response.bytes_stream(), options.stream.timeout_ms);

    loop {
        // P1-5: race the body read against the abort token — upstream hands
        // the signal to fetch (cancellation interrupts the body read
        // immediately); a check only after a chunk arrives would stall on an
        // idle body until the idle timeout (5 min default, unbounded when
        // disabled).
        let chunk = match next_chunk_or_cancelled(&mut byte_stream, signal.as_ref()).await {
            StreamNext::Item(chunk) => chunk,
            StreamNext::Cancelled => {
                return Err(StreamFailure::plain("Request was aborted"));
            }
        };
        let Some(chunk) = chunk else { break };
        let bytes = chunk.map_err(|error| StreamFailure::plain(error.message()))?;
        let parsed = reader.feed(&bytes).map_err(StreamFailure::plain)?;
        if push_converted(&mut converter, events, parsed) {
            // Terminal event pushed: upstream `return`s out of the async IIFE.
            return Ok(());
        }
    }
    let parsed = reader.finish().map_err(StreamFailure::plain)?;
    if push_converted(&mut converter, events, parsed) {
        return Ok(());
    }

    Err(StreamFailure::plain(format!(
        "{} stream ended without a terminal event",
        model.provider
    )))
}

/// Converts and pushes parsed events; returns true once a terminal
/// `done`/`error` event has been pushed.
fn push_converted(
    converter: &mut EventConverter,
    events: &AssistantMessageEventStream,
    parsed: Vec<PiMessagesEvent>,
) -> bool {
    for pi_event in parsed {
        let terminal = matches!(
            pi_event,
            PiMessagesEvent::Done { .. } | PiMessagesEvent::Error { .. }
        );
        events.push(converter.convert(pi_event));
        if terminal {
            return true;
        }
    }
    false
}

/// `stream` (pi-messages).
pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: PiMessagesOptions,
) -> AssistantMessageEventStream {
    let event_stream = AssistantMessageEventStream::new();
    let task_stream = event_stream.clone();
    let model = model.clone();
    let context = context.clone();
    tokio::spawn(async move {
        let signal = options.stream.signal.clone();
        match run(&model, &context, &options, &task_stream).await {
            Ok(()) => {
                task_stream.end(None);
            }
            Err(failure) => {
                let aborted = signal.as_ref().is_some_and(|signal| signal.is_cancelled());
                let error = create_error_message(&model, failure, aborted);
                task_stream.push(StreamEvent::Error {
                    reason: if aborted {
                        ErrorReason::Aborted
                    } else {
                        ErrorReason::Error
                    },
                    error,
                });
                task_stream.end(None);
            }
        }
    });
    event_stream
}

/// `streamSimple` (pi-messages). Upstream also smuggles `debug` off the
/// options object; `toolChoice` forwards verbatim into the request options
/// (pi-messages.ts:438 — the gateway takes the same `"auto" | "none"`
/// literals). rpi's [`SimpleStreamOptions`] has no `debug` field, so only
/// `reasoning`/`tool_choice` are mapped here (use [`stream`] for `debug`).
pub fn stream_simple(
    model: &Model,
    context: &TranscriptContext,
    options: Option<SimpleStreamOptions>,
) -> Result<AssistantMessageEventStream, String> {
    let reasoning = options.as_ref().and_then(|options| options.reasoning);
    Ok(stream(
        model,
        context,
        PiMessagesOptions {
            stream: options
                .as_ref()
                .map(|options| options.stream.clone())
                .unwrap_or_default(),
            reasoning,
            tool_choice: options.as_ref().and_then(|o| o.tool_choice).map(|choice| {
                serde_json::json!(match choice {
                    crate::types::SimpleToolChoice::Auto => "auto",
                    crate::types::SimpleToolChoice::None => "none",
                })
            }),
            debug: None,
        },
    ))
}

/// `ProviderStreams` implementation for `ApiKind::RPI_MESSAGES`.
///
/// The trait carries plain [`StreamOptions`]; pi-messages extras
/// ([`PiMessagesOptions`]) reach [`stream`] only through direct calls or via
/// [`stream_simple`] reasoning mapping (design §3.3 collapses per-API extras).
#[derive(Debug, Clone, Copy, Default)]
pub struct PiMessages;

impl ProviderStreams for PiMessages {
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        stream(
            model,
            context,
            PiMessagesOptions {
                stream: options.unwrap_or_default(),
                ..PiMessagesOptions::default()
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
    use super::*;

    #[test]
    fn test_parse_pi_messages_event_first_data_line() {
        // Only the first `data:` line is read; other fields are ignored.
        let event =
            parse_pi_messages_event("event: something\ndata: {\"type\":\"start\"}\ndata: {}")
                .expect("parses");
        assert_eq!(event, Some(PiMessagesEvent::Start));
    }

    #[test]
    fn test_parse_pi_messages_event_skips_empty_and_done_sentinel() {
        assert_eq!(parse_pi_messages_event("data:").expect("ok"), None);
        assert_eq!(parse_pi_messages_event("data: [DONE]").expect("ok"), None);
        assert_eq!(parse_pi_messages_event(": comment").expect("ok"), None);
    }

    #[test]
    fn test_parse_pi_messages_event_malformed_json_errors() {
        assert!(parse_pi_messages_event("data: {oops").is_err());
    }

    #[test]
    fn test_reader_crlf_split_chunks_and_trailing_block() {
        let mut reader = PiMessagesEventReader::default();
        // CRLF normalizes; an event split across chunks completes on the
        // blank line; the final block has no trailing blank line.
        let first = reader
            .feed(b"data: {\"type\":\"start\"}\r\n\r\ndata: {\"type\":\"text")
            .expect("feed");
        assert_eq!(first, vec![PiMessagesEvent::Start]);
        let second = reader
            .feed(b"_start\",\"contentIndex\":0}\n\n")
            .expect("feed");
        assert_eq!(
            second,
            vec![PiMessagesEvent::TextStart { content_index: 0 }]
        );
        let third = reader
            .feed(b"data: {\"type\":\"text_delta\",\"contentIndex\":0,\"delta\":\"hi\"}")
            .expect("feed");
        assert!(third.is_empty());
        let tail = reader.finish().expect("finish");
        assert_eq!(
            tail,
            vec![PiMessagesEvent::TextDelta {
                content_index: 0,
                delta: "hi".to_owned(),
            }]
        );
    }

    #[test]
    fn test_reader_utf8_split_across_chunks() {
        let full = "data: {\"type\":\"text_delta\",\"contentIndex\":0,\"delta\":\"🙈\"}\n\n";
        let bytes = full.as_bytes();
        let cut = bytes.len() / 2;
        let mut reader = PiMessagesEventReader::default();
        assert!(reader.feed(&bytes[..cut]).expect("feed").is_empty());
        let events = reader.feed(&bytes[cut..]).expect("feed");
        assert_eq!(
            events,
            vec![PiMessagesEvent::TextDelta {
                content_index: 0,
                delta: "🙈".to_owned(),
            }]
        );
    }

    #[test]
    fn test_resolve_cache_retention() {
        // Explicit value wins; env "long" opt-in maps; unset stays unset.
        assert_eq!(
            resolve_cache_retention(Some(CacheRetention::Short), None),
            Some(CacheRetention::Short)
        );
        let env: ProviderEnv = [("RPI_CACHE_RETENTION".to_owned(), "long".to_owned())].into();
        assert_eq!(
            resolve_cache_retention(None, Some(&env)),
            Some(CacheRetention::Long)
        );
        let env: ProviderEnv = [("RPI_CACHE_RETENTION".to_owned(), "short".to_owned())].into();
        assert_eq!(resolve_cache_retention(None, Some(&env)), None);
    }

    #[test]
    fn test_truncate_diagnostic_string() {
        let long = "a".repeat(9000);
        let truncated = truncate_diagnostic_string(&long);
        assert_eq!(truncated.chars().count(), 8192 + 1);
        assert!(truncated.ends_with('…'));
        assert_eq!(truncate_diagnostic_string("short"), "short");
    }

    #[test]
    fn test_error_body_parsing_and_format() {
        let body = r#"{"error":{"message":"Token expired","code":"unauthorized"}}"#;
        let error_body = parse_pi_messages_error_body(body);
        assert!(error_body.is_some());
        assert_eq!(
            format_pi_messages_response_error(401, "Unauthorized", body, error_body.as_ref()),
            "401 Unauthorized: Token expired (unauthorized)"
        );
        // Non-JSON and non-object `error` bodies fall back to the raw body.
        assert!(parse_pi_messages_error_body("not json").is_none());
        assert!(parse_pi_messages_error_body(r#"{"error":"x"}"#).is_none());
        assert_eq!(
            format_pi_messages_response_error(500, "Server Error", "raw body", None),
            "500 Server Error: raw body"
        );
    }
}
