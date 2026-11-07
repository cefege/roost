//! Port of `packages/agent/src/proxy.ts` @ pin `19451accd` (post-#9548:
//! `streamProxy` takes the normalized `TranscriptContext`; the wire body's
//! `context` field serializes as `{"messages": [...]}` — the brand symbol is
//! invisible to `JSON.stringify`). Earlier anchors: pi 0.82.1 (2efa728),
//! updated to 4181f66 for the v0.84 frame/protocol changes (`toolcall_end`
//! carries the server-authoritative `toolCall`, :46/:338-350;
//! `samplingParams` joins the options whitelist, :62/:105).
//!
//! SSE client that routes LLM calls through a server
//! (`POST {proxyUrl}/api/stream`). The server strips the `partial` field from
//! delta events to reduce bandwidth; the client reconstructs the partial
//! message and emits the standard [`StreamEvent`] protocol, terminating on
//! `done` / `error` (proxy.ts:20-31).
//!
//! Intentional differences:
//! - `ProxyMessageEventStream` (the `EventStream` subclass, :20-31) maps to
//!   [`AssistantMessageEventStream`] — the existing direct port of
//!   `packages/ai/src/utils/event-stream.ts`, which already implements
//!   `done`/`error` termination and `result()` resolution. The returned stream
//!   is `Stream<Item = StreamEvent>` + `Send` + `'static`, so it boxes
//!   directly into a `StreamFn` body (`Box::pin(stream_proxy(...))`).
//! - Line parsing is implemented here instead of reusing
//!   `rpi_ai::api::sse::SseDecoder`: the proxy treats every `data: ` line as
//!   one independent JSON event (:196-206) — no `event:` fields, no blank-line
//!   dispatch, and the final line without a trailing `\n` is never processed
//!   (the leftover buffer is discarded, :193/:207). Only the `TextDecoder`
//!   half (`{ stream: true }`) is mirrored — incremental UTF-8 decoding with
//!   U+FFFD substitution, the incomplete tail held across chunks and never
//!   flushed, exactly like the upstream loop.
//! - `AbortSignal` becomes `CancellationToken` (coding-standards §6.4). The
//!   abort handler's `reader.cancel()` (:141-149) is a `tokio::select!` branch
//!   that drops the pending body read. The abort error message is pinned to
//!   the proxy's own thrown strings ("Request aborted by user", :188/:210);
//!   upstream's mid-`fetch` abort message is engine-specific.
//! - The transient `partialJson` of a tool call (:316-339) lives in a side
//!   map (`ToolCall` has no such field in rpi); the JS quirk of
//!   `undefined += delta` for a delta without a preceding `toolcall_start` is
//!   not replicated.
//! - Every event carries a clone of the accumulated partial (Rust ownership);
//!   upstream events share the same object reference. Observable semantics
//!   (each event's `partial` shows the accumulated state) are identical.
//! - Malformed event JSON fails the stream with an `error` event (upstream
//!   `JSON.parse` throws inside the IIFE, :199/:214); a valid JSON event with
//!   an unknown `type` is warned and skipped (:361-365). Structurally invalid
//!   known events (missing fields) also fail with an `error` event, where
//!   upstream's JS would misbehave with `undefined` indexes.
//! - A `*_start` event whose `contentIndex` equals the content length appends
//!   the block (upstream array auto-extends); a `*_start` for a skipped index
//!   is a protocol error instead of the JS array hole upstream would create
//!   (`AssistantContent` has no hole representation).

use std::collections::HashMap;
use std::sync::Arc;

use futures::StreamExt;
use rpi_ai::types::{
    AssistantContent, AssistantMessage, AssistantRole, CacheRetention, DoneReason, ErrorReason,
    Model, ProviderHeaders, StopReason, StreamEvent, TextContent, ThinkingBudgets, ThinkingContent,
    ThinkingLevel, ToolCall, TranscriptContext, Transport, Usage,
};
use rpi_ai::utils::event_stream::AssistantMessageEventStream;
use rpi_ai::utils::json_parse::parse_streaming_json;
use rpi_ai::utils::transport_error::format_reqwest_error;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Proxy event types (:36-57)
// ---------------------------------------------------------------------------

/// `ProxyAssistantMessageEvent` — server events with the `partial` field
/// stripped to reduce bandwidth (:36-57).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ProxyAssistantMessageEvent {
    #[serde(rename = "start")]
    Start,
    TextStart {
        content_index: usize,
    },
    TextDelta {
        content_index: usize,
        delta: String,
    },
    TextEnd {
        content_index: usize,
        content_signature: Option<String>,
    },
    ThinkingStart {
        content_index: usize,
    },
    ThinkingDelta {
        content_index: usize,
        delta: String,
    },
    ThinkingEnd {
        content_index: usize,
        content_signature: Option<String>,
    },
    #[serde(rename = "toolcall_start")]
    ToolCallStart {
        content_index: usize,
        id: String,
        tool_name: String,
    },
    #[serde(rename = "toolcall_delta")]
    ToolCallDelta {
        content_index: usize,
        delta: String,
    },
    #[serde(rename = "toolcall_end")]
    ToolCallEnd {
        content_index: usize,
        /// The server-authoritative finished tool call (proxy.ts:46). The
        /// `type: "toolCall"` tag rides along via
        /// [`rpi_ai::types::tagged_tool_call`].
        #[serde(with = "rpi_ai::types::tagged_tool_call")]
        tool_call: ToolCall,
    },
    Done {
        reason: DoneReason,
        usage: Usage,
        /// Exact provider-native effort level for the response
        /// (proxy.ts:51, 4e69b0c28): written into `partial` so proxy-path
        /// messages persist it like direct-path ones.
        #[serde(
            rename = "providerThinkingLevel",
            skip_serializing_if = "Option::is_none"
        )]
        provider_thinking_level: Option<String>,
    },
    Error {
        reason: ErrorReason,
        error_message: Option<String>,
        usage: Usage,
        /// See [`ProxyAssistantMessageEvent::Done`]'s field
        /// (proxy.ts:58).
        #[serde(
            rename = "providerThinkingLevel",
            skip_serializing_if = "Option::is_none"
        )]
        provider_thinking_level: Option<String>,
    },
}

/// The twelve event type tags (:36-57).
const KNOWN_PROXY_EVENT_TYPES: &[&str] = &[
    "start",
    "text_start",
    "text_delta",
    "text_end",
    "thinking_start",
    "thinking_delta",
    "thinking_end",
    "toolcall_start",
    "toolcall_delta",
    "toolcall_end",
    "done",
    "error",
];

/// Parses one `data:` payload. Unknown event types are warned and skipped
/// (`Ok(None)`, :361-365); malformed events fail the stream (`Err`).
fn parse_proxy_event(
    value: Value,
) -> Result<Option<ProxyAssistantMessageEvent>, serde_json::Error> {
    // `${(proxyEvent as any).type}` coerces a missing/non-string type to a
    // template literal upstream; a JSON round-trip is the closest equivalent
    // (missing -> "undefined").
    let kind = match value.get("type") {
        Some(Value::String(kind)) => kind.clone(),
        Some(other) => other.to_string(),
        None => "undefined".to_owned(),
    };
    if KNOWN_PROXY_EVENT_TYPES.contains(&kind.as_str()) {
        Ok(Some(serde_json::from_value(value)?))
    } else {
        tracing::warn!("Unhandled proxy event type: {kind}");
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Stream options
// ---------------------------------------------------------------------------

/// `ProxySerializableStreamOptions = Pick<SimpleStreamOptions, ...>` (:59-72):
/// the eleven fields the proxy server is allowed to see
/// (`buildProxyRequestOptions`, :102-115). Serialized with absent fields
/// omitted (upstream `JSON.stringify` drops `undefined` properties).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProxySerializableStreamOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    /// Arbitrary sampling parameters, merged into the request body server-side
    /// (v0.84, R2.1.4; upstream whitelist entry `samplingParams`).
    #[serde(skip_serializing_if = "Option::is_none")]
    sampling_params: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ThinkingLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_retention: Option<CacheRetention>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    headers: Option<ProviderHeaders>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transport: Option<Transport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking_budgets: Option<ThinkingBudgets>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_retry_delay_ms: Option<u64>,
}

/// `ProxyStreamOptions extends ProxySerializableStreamOptions` (:74-81):
/// the whitelisted stream options plus the local-only `signal`, `authToken`
/// and `proxyUrl`. The whitelist is structural: fields outside the eleven
/// serializable ones cannot be sent to the server.
#[derive(Debug, Clone)]
pub struct ProxyStreamOptions {
    pub temperature: Option<f64>,
    /// `StreamOptions.sampling_params` (v0.84, R2.1.4).
    pub sampling_params: Option<Map<String, Value>>,
    pub max_tokens: Option<u32>,
    /// `SimpleStreamOptions.reasoning` — `ThinkingLevel` (off-exclusive).
    pub reasoning: Option<ThinkingLevel>,
    pub cache_retention: Option<CacheRetention>,
    pub session_id: Option<String>,
    pub headers: Option<ProviderHeaders>,
    pub metadata: Option<Map<String, Value>>,
    pub transport: Option<Transport>,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub max_retry_delay_ms: Option<u64>,
    /// Local abort signal for the proxy request (:75).
    pub signal: Option<CancellationToken>,
    /// Auth token for the proxy server (:77).
    pub auth_token: String,
    /// Proxy server URL, e.g. "https://genai.example.com" (:79).
    pub proxy_url: String,
}

/// Request body (:158-162): `{ model, context, options }`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProxyRequestBody<'a> {
    model: &'a Model,
    context: &'a TranscriptContext,
    options: ProxySerializableStreamOptions,
}

/// `buildProxyRequestOptions` (:102-115).
fn build_proxy_request_options(options: &ProxyStreamOptions) -> ProxySerializableStreamOptions {
    ProxySerializableStreamOptions {
        temperature: options.temperature,
        sampling_params: options.sampling_params.clone(),
        max_tokens: options.max_tokens,
        reasoning: options.reasoning,
        cache_retention: options.cache_retention,
        session_id: options.session_id.clone(),
        headers: options.headers.clone(),
        metadata: options.metadata.clone(),
        transport: options.transport,
        thinking_budgets: options.thinking_budgets.clone(),
        max_retry_delay_ms: options.max_retry_delay_ms,
    }
}

// ---------------------------------------------------------------------------
// Stream function (:82-233)
// ---------------------------------------------------------------------------

/// Stream function that proxies through a server instead of calling LLM
/// providers directly (:82-100). The server strips the `partial` field from
/// delta events to reduce bandwidth; we reconstruct the partial message
/// client-side (:121-137).
///
/// The returned stream is `Stream<Item = StreamEvent>` + `Send` + `'static`
/// and terminates on `done` / `error` (:20-31), so it boxes directly into a
/// `StreamFn` body.
pub fn stream_proxy(
    model: &Model,
    context: &TranscriptContext,
    options: ProxyStreamOptions,
) -> AssistantMessageEventStream {
    let event_stream = AssistantMessageEventStream::new();
    let task_stream = event_stream.clone();
    let model = model.clone();
    let context = context.clone();
    let signal = options.signal.clone();
    tokio::spawn(async move {
        // :121-137 — the partial message reconstructed from delta events.
        let mut partial = initial_partial(&model);
        // Transient `partialJson` per content index (:316-339); see the file
        // header notes for why it lives outside `ToolCall`.
        let mut partial_jsons: HashMap<usize, String> = HashMap::new();
        match run(
            &model,
            &context,
            &options,
            &mut partial,
            &mut partial_jsons,
            &task_stream,
        )
        .await
        {
            Ok(()) => task_stream.end(None),
            Err(message) => {
                // :214-224 — failures become an `error` event carrying the
                // accumulated partial; `aborted` when the signal fired.
                let aborted = signal.as_ref().is_some_and(|token| token.is_cancelled());
                partial.stop_reason = if aborted {
                    StopReason::Aborted
                } else {
                    StopReason::Error
                };
                partial.error_message = Some(message);
                task_stream.push(StreamEvent::Error {
                    reason: if aborted {
                        ErrorReason::Aborted
                    } else {
                        ErrorReason::Error
                    },
                    error: partial,
                });
                task_stream.end(None);
            }
        }
    });
    event_stream
}

/// Everything inside upstream's async IIFE (:119-230). `Err(message)` carries
/// the upstream thrown `Error.message`; the accumulated `partial` stays valid
/// either way.
async fn run(
    model: &Model,
    context: &TranscriptContext,
    options: &ProxyStreamOptions,
    partial: &mut AssistantMessage,
    partial_jsons: &mut HashMap<usize, String>,
    stream: &AssistantMessageEventStream,
) -> Result<(), String> {
    let signal = options.signal.as_ref();

    let body = serde_json::to_string(&ProxyRequestBody {
        model,
        context,
        options: build_proxy_request_options(options),
    })
    .map_err(|error| error.to_string())?;

    // :152-164 — `fetch(proxyUrl + "/api/stream", ...)` (no URL normalization
    // upstream; a trailing slash would double).
    let url = format!("{}/api/stream", options.proxy_url);
    let client = reqwest::Client::new();
    let request = client
        .post(&url)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", options.auth_token),
        )
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body);
    let response = match signal {
        // `fetch(..., { signal })` (:163): an abort cancels the request itself.
        Some(token) => tokio::select! {
            result = request.send() => result,
            () = token.cancelled() => return Err("Request aborted by user".to_owned()),
        },
        None => request.send().await,
    }
    .map_err(|error| format_reqwest_error(&error))?;

    // :166-177
    let status = response.status();
    if !status.is_success() {
        return Err(proxy_error_message(status, response).await);
    }

    // :179-207 — the read loop. The `TextDecoder` half is
    // [`IncrementalUtf8Decoder`]; `sawTerminalEvent` tracks whether a
    // `done`/`error` event arrived (:186, :195).
    let mut decoder = IncrementalUtf8Decoder::new();
    let mut buffer = String::new();
    let mut saw_terminal_event = false;
    let mut byte_stream = response.bytes_stream();
    loop {
        let chunk = match signal {
            Some(token) => tokio::select! {
                chunk = byte_stream.next() => chunk,
                // :141-149 — abort cancels the reader: the pending body read
                // is dropped with the branch future.
                () = token.cancelled() => return Err("Request aborted by user".to_owned()),
            },
            None => byte_stream.next().await,
        };
        let Some(chunk) = chunk else { break };
        let bytes = chunk.map_err(|error| format_reqwest_error(&error))?;
        // :187-189
        if signal.is_some_and(|token| token.is_cancelled()) {
            return Err("Request aborted by user".to_owned());
        }
        buffer.push_str(&decoder.decode(&bytes));
        // :192-194 — `lines = buffer.split("\n"); buffer = lines.pop() || ""`.
        let mut lines: Vec<String> = buffer.split('\n').map(str::to_owned).collect();
        buffer = lines.pop().unwrap_or_default();
        for line in lines {
            process_data_line(
                &line,
                partial,
                partial_jsons,
                stream,
                &mut saw_terminal_event,
            )?;
        }
    }

    // :209-211
    if signal.is_some_and(|token| token.is_cancelled()) {
        return Err("Request aborted by user".to_owned());
    }
    // :213-217 — the final event may not be newline-terminated: flush the
    // decoder and process whatever is left in the buffer (ebc374490, #8997 —
    // the residual line used to be dropped, losing the last event).
    buffer.push_str(&decoder.flush());
    if !buffer.is_empty() {
        process_data_line(
            &buffer,
            partial,
            partial_jsons,
            stream,
            &mut saw_terminal_event,
        )?;
    }
    // :219-233 — a clean EOF without a done/error event means the server
    // dropped the response mid-stream; surface it as an error instead of
    // leaving consumers waiting on a result that never arrives
    // (ebc374490, #8997).
    if !saw_terminal_event {
        partial.stop_reason = StopReason::Error;
        partial.error_message =
            Some("Connection closed by proxy server before the response completed".to_owned());
        stream.push(StreamEvent::Error {
            reason: ErrorReason::Error,
            error: partial.clone(),
        });
    }
    // :235 — `stream.end()`; a terminal `done`/`error` event is expected to
    // have arrived on the wire.
    Ok(())
}

/// `processLine` (:194-206): one SSE line → zero or one stream event.
/// Sets `saw_terminal_event` when the produced event terminates the stream
/// (`done`/`error`, :195).
fn process_data_line(
    line: &str,
    partial: &mut AssistantMessage,
    partial_jsons: &mut HashMap<usize, String>,
    stream: &AssistantMessageEventStream,
    saw_terminal_event: &mut bool,
) -> Result<(), String> {
    // :196 — only `data: `-prefixed lines are events.
    let Some(data) = line.strip_prefix("data: ") else {
        return Ok(());
    };
    // :197-198 — `line.slice(6).trim()`.
    let data = data.trim();
    if data.is_empty() {
        return Ok(());
    }
    // :199-203 — `JSON.parse` failures abort the stream.
    let value: Value = serde_json::from_str(data).map_err(|error| error.to_string())?;
    // Unknown event types parse to `None` (already warned).
    if let Some(proxy_event) = parse_proxy_event(value).map_err(|error| error.to_string())?
        && let Some(event) = process_proxy_event(proxy_event, partial, partial_jsons)?
    {
        if matches!(event, StreamEvent::Done { .. } | StreamEvent::Error { .. }) {
            *saw_terminal_event = true;
        }
        stream.push(event);
    }
    Ok(())
}

/// :166-177 — non-2xx responses become `Proxy error: ...` messages.
async fn proxy_error_message(status: reqwest::StatusCode, response: reqwest::Response) -> String {
    // :167 — `Proxy error: ${response.status} ${response.statusText}`.
    let mut message = format!(
        "Proxy error: {} {}",
        status.as_u16(),
        status.canonical_reason().unwrap_or("")
    );
    // :168-175 — a JSON body with a truthy string `error` field overrides the
    // fallback (:171). Non-string / empty values are ignored, matching the
    // upstream truthiness check; unparseable bodies keep the fallback
    // (:173-175).
    let body = response.text().await.unwrap_or_default();
    let error_field = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|data| data.get("error").and_then(Value::as_str).map(str::to_owned))
        .filter(|error| !error.is_empty());
    if let Some(error) = error_field {
        message = format!("Proxy error: {error}");
    }
    message
}

/// `TextDecoder` with `{ stream: true }` (:180, :191): incremental UTF-8
/// decoding with U+FFFD substitution; an incomplete trailing sequence is held
/// across chunks. [`IncrementalUtf8Decoder::flush`] is the end-of-stream
/// `decoder.decode()` (:218, ebc374490). Unlike
/// `rpi_ai::api::sse::SseDecoder` there is no event framing here — this type
/// only decodes bytes into the line `String`.
struct IncrementalUtf8Decoder {
    tail: Vec<u8>,
}

impl IncrementalUtf8Decoder {
    fn new() -> Self {
        Self { tail: Vec::new() }
    }

    fn decode(&mut self, bytes: &[u8]) -> String {
        let mut combined = std::mem::take(&mut self.tail);
        combined.extend_from_slice(bytes);
        let mut decoded = String::new();
        let mut offset = 0;
        while offset < combined.len() {
            match std::str::from_utf8(&combined[offset..]) {
                Ok(valid) => {
                    decoded.push_str(valid);
                    offset = combined.len();
                }
                Err(error) => {
                    // invariant: `valid_up_to` is a valid UTF-8 boundary
                    decoded.push_str(
                        std::str::from_utf8(&combined[offset..offset + error.valid_up_to()])
                            .unwrap_or(""),
                    );
                    offset += error.valid_up_to();
                    match error.error_len() {
                        Some(invalid_len) => {
                            decoded.push('\u{FFFD}');
                            offset += invalid_len;
                        }
                        None => break, // incomplete trailing sequence: hold it
                    }
                }
            }
        }
        self.tail = combined[offset..].to_vec();
        decoded
    }

    /// `decoder.decode()` without `{ stream: true }` (:218): end-of-stream
    /// flush — an incomplete trailing sequence becomes a single U+FFFD
    /// instead of being held (ebc374490, #8997).
    fn flush(&mut self) -> String {
        let tail = std::mem::take(&mut self.tail);
        if tail.is_empty() {
            return String::new();
        }
        match std::str::from_utf8(&tail) {
            Ok(valid) => valid.to_owned(),
            Err(error) => {
                let mut decoded = std::str::from_utf8(&tail[..error.valid_up_to()])
                    .unwrap_or("")
                    .to_owned();
                decoded.push('\u{FFFD}');
                decoded
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Event processing (:238-367)
// ---------------------------------------------------------------------------

/// `processProxyEvent` (:238-367): applies a proxy event to the partial
/// message and produces the downstream [`StreamEvent`]. `Err(message)` carries
/// the upstream thrown `Error.message`; `Ok(None)` skips the event.
fn process_proxy_event(
    proxy_event: ProxyAssistantMessageEvent,
    partial: &mut AssistantMessage,
    partial_jsons: &mut HashMap<usize, String>,
) -> Result<Option<StreamEvent>, String> {
    match proxy_event {
        ProxyAssistantMessageEvent::Start => Ok(Some(StreamEvent::Start {
            partial: Arc::new(partial.clone()),
        })),

        ProxyAssistantMessageEvent::TextStart { content_index } => {
            set_content_block(
                partial,
                content_index,
                AssistantContent::Text(TextContent::default()),
            )?;
            Ok(Some(StreamEvent::TextStart {
                content_index,
                partial: Arc::new(partial.clone()),
            }))
        }

        ProxyAssistantMessageEvent::TextDelta {
            content_index,
            delta,
        } => {
            // :250-262 — delta accumulates in place (:253); a delta for
            // non-text content is a protocol error (:261).
            let Some(AssistantContent::Text(text)) = partial.content.get_mut(content_index) else {
                return Err("Received text_delta for non-text content".to_owned());
            };
            text.text.push_str(&delta);
            Ok(Some(StreamEvent::TextDelta {
                content_index,
                delta,
                partial: Arc::new(partial.clone()),
            }))
        }

        ProxyAssistantMessageEvent::TextEnd {
            content_index,
            content_signature,
        } => {
            let Some(AssistantContent::Text(text)) = partial.content.get_mut(content_index) else {
                return Err("Received text_end for non-text content".to_owned());
            };
            let content = text.text.clone();
            text.text_signature = content_signature;
            Ok(Some(StreamEvent::TextEnd {
                content_index,
                content,
                partial: Arc::new(partial.clone()),
            }))
        }

        ProxyAssistantMessageEvent::ThinkingStart { content_index } => {
            set_content_block(
                partial,
                content_index,
                AssistantContent::Thinking(ThinkingContent::default()),
            )?;
            Ok(Some(StreamEvent::ThinkingStart {
                content_index,
                partial: Arc::new(partial.clone()),
            }))
        }

        ProxyAssistantMessageEvent::ThinkingDelta {
            content_index,
            delta,
        } => {
            let Some(AssistantContent::Thinking(thinking)) = partial.content.get_mut(content_index)
            else {
                return Err("Received thinking_delta for non-thinking content".to_owned());
            };
            thinking.thinking.push_str(&delta);
            Ok(Some(StreamEvent::ThinkingDelta {
                content_index,
                delta,
                partial: Arc::new(partial.clone()),
            }))
        }

        ProxyAssistantMessageEvent::ThinkingEnd {
            content_index,
            content_signature,
        } => {
            let Some(AssistantContent::Thinking(thinking)) = partial.content.get_mut(content_index)
            else {
                return Err("Received thinking_end for non-thinking content".to_owned());
            };
            let content = thinking.thinking.clone();
            thinking.thinking_signature = content_signature;
            Ok(Some(StreamEvent::ThinkingEnd {
                content_index,
                content,
                partial: Arc::new(partial.clone()),
            }))
        }

        ProxyAssistantMessageEvent::ToolCallStart {
            content_index,
            id,
            tool_name,
        } => {
            // :310-318 — `{ type: "toolCall", id, name: toolName, arguments: {},
            // partialJson: "" }` (:317); `partialJson` tracked separately.
            set_content_block(
                partial,
                content_index,
                AssistantContent::ToolCall(ToolCall {
                    id,
                    name: tool_name,
                    arguments: Map::new(),
                    thought_signature: None,
                    namespace: None,
                }),
            )?;
            partial_jsons.insert(content_index, String::new());
            Ok(Some(StreamEvent::ToolCallStart {
                content_index,
                partial: Arc::new(partial.clone()),
            }))
        }

        ProxyAssistantMessageEvent::ToolCallDelta {
            content_index,
            delta,
        } => {
            // :320-334 — accumulate `partialJson` and incrementally parse the
            // arguments (:323-324); a delta for non-toolCall content is a
            // protocol error (:333).
            let Some(AssistantContent::ToolCall(call)) = partial.content.get_mut(content_index)
            else {
                return Err("Received toolcall_delta for non-toolCall content".to_owned());
            };
            let accumulated = partial_jsons.entry(content_index).or_default();
            accumulated.push_str(&delta);
            call.arguments = parse_streaming_json(Some(accumulated.as_str()));
            Ok(Some(StreamEvent::ToolCallDelta {
                content_index,
                delta,
                partial: Arc::new(partial.clone()),
            }))
        }

        ProxyAssistantMessageEvent::ToolCallEnd {
            content_index,
            tool_call,
        } => {
            // :338-350 — the frame's server-authoritative `toolCall` replaces
            // the accumulated block (`Object.assign(content,
            // proxyEvent.toolCall)`, :341; full replacement is equivalent
            // because `ToolCall` covers every assignable field) and the
            // transient `partialJson` is dropped (`delete`, :342). A
            // toolcall_end for non-toolCall content is silently skipped
            // (:347).
            partial_jsons.remove(&content_index);
            match partial.content.get_mut(content_index) {
                Some(AssistantContent::ToolCall(call)) => {
                    *call = tool_call;
                    Ok(Some(StreamEvent::ToolCallEnd {
                        content_index,
                        tool_call: call.clone(),
                        partial: Arc::new(partial.clone()),
                    }))
                }
                _ => Ok(None),
            }
        }

        ProxyAssistantMessageEvent::Done {
            reason,
            usage,
            provider_thinking_level,
        } => {
            // :350-353
            partial.stop_reason = match reason {
                DoneReason::Stop => StopReason::Stop,
                DoneReason::Length => StopReason::Length,
                DoneReason::ToolUse => StopReason::ToolUse,
                // Placeholder variant (R2.1.1); the shared enum accepts it on
                // the wire (see `DoneReason` docs), so map it explicitly
                // instead of rejecting the frame.
                DoneReason::Deferred => StopReason::Deferred,
            };
            partial.usage = usage;
            // :382-384 — absent on the frame leaves any earlier value
            // untouched (4e69b0c28).
            if let Some(level) = provider_thinking_level {
                partial.provider_thinking_level = Some(level);
            }
            Ok(Some(StreamEvent::Done {
                reason,
                message: partial.clone(),
            }))
        }

        ProxyAssistantMessageEvent::Error {
            reason,
            error_message,
            usage,
            provider_thinking_level,
        } => {
            // :355-359
            partial.stop_reason = match reason {
                ErrorReason::Aborted => StopReason::Aborted,
                ErrorReason::Error => StopReason::Error,
            };
            partial.error_message = error_message;
            partial.usage = usage;
            // :390-392 (4e69b0c28)
            if let Some(level) = provider_thinking_level {
                partial.provider_thinking_level = Some(level);
            }
            Ok(Some(StreamEvent::Error {
                reason,
                error: partial.clone(),
            }))
        }
    }
}

/// Applies a `*_start` proxy event's content block to the partial message.
/// Upstream assigns `partial.content[contentIndex] = ...`, which in JS
/// auto-extends the array when `contentIndex == content.length` and creates
/// holes for larger indexes. `AssistantContent` has no hole representation and
/// well-behaved servers always start indexes in order, so the hole case is a
/// protocol error (documented difference from upstream).
fn set_content_block(
    partial: &mut AssistantMessage,
    content_index: usize,
    block: AssistantContent,
) -> Result<(), String> {
    if content_index < partial.content.len() {
        partial.content[content_index] = block;
        Ok(())
    } else if content_index == partial.content.len() {
        partial.content.push(block);
        Ok(())
    } else {
        Err(format!(
            "Received content start at index {content_index}, before previous content indexes"
        ))
    }
}

/// :121-137 — the initial partial message (`stopReason: "pending"`, zeroed
/// usage).
fn initial_partial(model: &Model) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        stop_reason: StopReason::Pending,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        error_message: None,
        timestamp: now_ms(),
        deferred: None,
        end_turn: None,
        raw_stop_reason: None,
        duration_ms: None,
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use rpi_ai::types::{ApiKind, InputModality, ModelCost};
    use serde_json::json;

    use super::*;

    fn parse(json: &str) -> Result<Option<ProxyAssistantMessageEvent>, serde_json::Error> {
        parse_proxy_event(serde_json::from_str(json).expect("valid JSON"))
    }

    #[test]
    fn test_parse_all_known_event_types() {
        let cases: Vec<(&str, ProxyAssistantMessageEvent)> = vec![
            (r#"{"type":"start"}"#, ProxyAssistantMessageEvent::Start),
            (
                r#"{"type":"text_start","contentIndex":0}"#,
                ProxyAssistantMessageEvent::TextStart { content_index: 0 },
            ),
            (
                r#"{"type":"text_delta","contentIndex":1,"delta":"hi"}"#,
                ProxyAssistantMessageEvent::TextDelta {
                    content_index: 1,
                    delta: "hi".to_owned(),
                },
            ),
            (
                r#"{"type":"text_end","contentIndex":0}"#,
                ProxyAssistantMessageEvent::TextEnd {
                    content_index: 0,
                    content_signature: None,
                },
            ),
            (
                r#"{"type":"text_end","contentIndex":0,"contentSignature":"sig"}"#,
                ProxyAssistantMessageEvent::TextEnd {
                    content_index: 0,
                    content_signature: Some("sig".to_owned()),
                },
            ),
            (
                r#"{"type":"thinking_start","contentIndex":2}"#,
                ProxyAssistantMessageEvent::ThinkingStart { content_index: 2 },
            ),
            (
                r#"{"type":"thinking_delta","contentIndex":2,"delta":"t"}"#,
                ProxyAssistantMessageEvent::ThinkingDelta {
                    content_index: 2,
                    delta: "t".to_owned(),
                },
            ),
            (
                r#"{"type":"thinking_end","contentIndex":2}"#,
                ProxyAssistantMessageEvent::ThinkingEnd {
                    content_index: 2,
                    content_signature: None,
                },
            ),
            (
                r#"{"type":"toolcall_start","contentIndex":3,"id":"c1","toolName":"bash"}"#,
                ProxyAssistantMessageEvent::ToolCallStart {
                    content_index: 3,
                    id: "c1".to_owned(),
                    tool_name: "bash".to_owned(),
                },
            ),
            (
                r#"{"type":"toolcall_delta","contentIndex":3,"delta":"{}"}"#,
                ProxyAssistantMessageEvent::ToolCallDelta {
                    content_index: 3,
                    delta: "{}".to_owned(),
                },
            ),
            (
                r#"{"type":"toolcall_end","contentIndex":3,"toolCall":{"type":"toolCall","id":"c1","name":"bash","arguments":{}}}"#,
                ProxyAssistantMessageEvent::ToolCallEnd {
                    content_index: 3,
                    tool_call: ToolCall {
                        id: "c1".to_owned(),
                        name: "bash".to_owned(),
                        arguments: Map::new(),
                        thought_signature: None,
                        namespace: None,
                    },
                },
            ),
            (
                r#"{"type":"done","reason":"stop","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}}"#,
                ProxyAssistantMessageEvent::Done {
                    reason: DoneReason::Stop,
                    usage: Usage::default(),
                    provider_thinking_level: None,
                },
            ),
            (
                r#"{"type":"done","reason":"stop","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"providerThinkingLevel":"high"}"#,
                ProxyAssistantMessageEvent::Done {
                    reason: DoneReason::Stop,
                    usage: Usage::default(),
                    provider_thinking_level: Some("high".to_owned()),
                },
            ),
            (
                r#"{"type":"error","reason":"aborted","errorMessage":"oops","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}}"#,
                ProxyAssistantMessageEvent::Error {
                    reason: ErrorReason::Aborted,
                    error_message: Some("oops".to_owned()),
                    usage: Usage::default(),
                    provider_thinking_level: None,
                },
            ),
            (
                r#"{"type":"error","reason":"error","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"providerThinkingLevel":"low"}"#,
                ProxyAssistantMessageEvent::Error {
                    reason: ErrorReason::Error,
                    error_message: None,
                    usage: Usage::default(),
                    provider_thinking_level: Some("low".to_owned()),
                },
            ),
        ];
        for (json, expected) in cases {
            assert_eq!(parse(json).expect("parses"), Some(expected), "for {json}");
        }
    }

    #[test]
    fn test_parse_unknown_missing_and_non_string_types_are_skipped() {
        assert!(
            parse(r#"{"type":"bogus","x":1}"#)
                .expect("parses")
                .is_none()
        );
        assert!(parse(r#"{"x":1}"#).expect("parses").is_none());
        assert!(parse(r#"{"type":5}"#).expect("parses").is_none());
        assert!(parse(r#"{"type":null}"#).expect("parses").is_none());
    }

    #[test]
    fn test_parse_malformed_known_event_errors() {
        // Structurally invalid known events fail the stream (upstream would
        // misbehave with `undefined` indexes).
        assert!(parse(r#"{"type":"text_delta"}"#).is_err());
        assert!(parse(r#"{"type":"done"}"#).is_err());
        assert!(parse(r#"{"type":"toolcall_start","contentIndex":0}"#).is_err());
    }

    #[test]
    fn test_process_toolcall_delta_parses_arguments_incrementally() {
        let mut partial = initial_partial(&test_model());
        let mut partial_jsons = HashMap::new();

        let start = process_proxy_event(
            ProxyAssistantMessageEvent::ToolCallStart {
                content_index: 0,
                id: "c1".to_owned(),
                tool_name: "bash".to_owned(),
            },
            &mut partial,
            &mut partial_jsons,
        )
        .expect("start processes");
        assert!(matches!(start, Some(StreamEvent::ToolCallStart { .. })));

        let delta = process_proxy_event(
            ProxyAssistantMessageEvent::ToolCallDelta {
                content_index: 0,
                delta: "{\"cmd\":\"ls".to_owned(),
            },
            &mut partial,
            &mut partial_jsons,
        )
        .expect("delta processes");
        let StreamEvent::ToolCallDelta {
            partial: delta_partial,
            ..
        } = delta.expect("delta event")
        else {
            panic!("expected ToolCallDelta");
        };
        assert_eq!(
            delta_partial.content[0],
            AssistantContent::ToolCall(ToolCall {
                id: "c1".to_owned(),
                name: "bash".to_owned(),
                arguments: json!({"cmd": "ls"}).as_object().expect("object").clone(),
                thought_signature: None,
                namespace: None,
            })
        );

        process_proxy_event(
            ProxyAssistantMessageEvent::ToolCallDelta {
                content_index: 0,
                delta: " -la\"}".to_owned(),
            },
            &mut partial,
            &mut partial_jsons,
        )
        .expect("delta processes");
        // The frame carries the server-authoritative finished call
        // (proxy.ts:46); it replaces the accumulated block (:341).
        let finished = ToolCall {
            id: "c1".to_owned(),
            name: "bash".to_owned(),
            arguments: json!({"cmd": "ls -la"})
                .as_object()
                .expect("object")
                .clone(),
            thought_signature: None,
            namespace: None,
        };
        let end = process_proxy_event(
            ProxyAssistantMessageEvent::ToolCallEnd {
                content_index: 0,
                tool_call: finished.clone(),
            },
            &mut partial,
            &mut partial_jsons,
        )
        .expect("end processes");
        let StreamEvent::ToolCallEnd { tool_call, .. } = end.expect("end event") else {
            panic!("expected ToolCallEnd");
        };
        assert_eq!(tool_call, finished);
        assert!(
            partial_jsons.is_empty(),
            "partialJson dropped at toolcall_end"
        );
    }

    fn test_model() -> Model {
        Model {
            id: "test-model".to_owned(),
            name: "Test Model".to_owned(),
            api: ApiKind::from("anthropic-messages"),
            provider: "test-provider".to_owned(),
            base_url: "http://test".to_owned(),
            reasoning: false,
            thinking_level_map: None,
            input: vec![InputModality::Text],
            input_limits: None,
            cost: ModelCost::default(),
            prompt_cache: None,
            context_window: 8192,
            max_tokens: 4096,
            headers: None,
            compat: None,
            sampling_params: None,
            sampling_params_by_thinking_level: None,
        }
    }
}
