//! Port of `packages/ai/src/types.ts` @ pi 0.84.1+ (4181f66).
//!
//! Core wire types: messages, content blocks, usage, tools, models (with
//! compat matrices), stream options and the assistant stream event protocol.
//!
//! Intentional differences:
//! - TS `Api = KnownApi | (string & {})` becomes the [`ApiKind`] newtype with
//!   associated constants for the known APIs (wire-compatible, open to custom
//!   API strings like upstream).
//! - The per-API conditional `Model.compat` type becomes the single flat
//!   [`ModelCompat`] struct merging `OpenAICompletionsCompat`,
//!   `OpenAIResponsesCompat`, `AnthropicMessagesCompat` and `BedrockCompat`.
//!   All overlapping field names share the same type upstream, so the merged
//!   struct is wire-compatible in both directions; which fields are meaningful
//!   is governed by `Model.api` (enforced by the adapters, not the type).
//! - `AbortSignal` becomes `tokio_util::sync::CancellationToken`
//!   (coding-standards §6.4).
//! - `onPayload` / `onResponse` callbacks become `Arc<dyn Fn>` returning boxed
//!   futures; they are absent from serialized form (StreamOptions is internal,
//!   never on the wire).
//! - TS interface inheritance `StreamOptions extends ProviderRequestOptions`
//!   (v0.84, R2.8.1) becomes composition: [`StreamOptions`] owns a
//!   [`ProviderRequestOptions`] in its `request` field and implements
//!   `Deref`/`DerefMut` to it, so field access paths (`options.timeout_ms`)
//!   read exactly like upstream. Constructing the request-scoped fields uses
//!   the nested struct literal.
//! - `FetchFunction = typeof globalThis.fetch` becomes the neutral
//!   [`FetchFn`] channel over [`FetchRequest`]/[`FetchResponse`] value types
//!   (rpi has no global fetch to mirror; the shape is mock-friendly). Channel
//!   only: no adapter consumes it yet (R2.7.4 wiring lands in T20/T26).
//! - `TelemetryContext` (pi-telemetry) is an opaque placeholder unit struct:
//!   the telemetry pipeline is [DEFER] (v0.11 requirements §1.2); only the
//!   `ProviderRequestOptions.telemetry_context` field position is kept.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Api / Provider identifiers
// ---------------------------------------------------------------------------

/// API kind identifier. Mirrors `Api = KnownApi | (string & {})`: an open
/// string with associated constants for the ten known APIs (requirements §5.1).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ApiKind(pub String);

impl ApiKind {
    pub const OPENAI_COMPLETIONS: &'static str = "openai-completions";
    pub const MISTRAL_CONVERSATIONS: &'static str = "mistral-conversations";
    pub const OPENAI_RESPONSES: &'static str = "openai-responses";
    pub const AZURE_OPENAI_RESPONSES: &'static str = "azure-openai-responses";
    pub const OPENAI_CODEX_RESPONSES: &'static str = "openai-codex-responses";
    pub const ANTHROPIC_MESSAGES: &'static str = "anthropic-messages";
    pub const BEDROCK_CONVERSE_STREAM: &'static str = "bedrock-converse-stream";
    pub const GOOGLE_GENERATIVE_AI: &'static str = "google-generative-ai";
    pub const GOOGLE_VERTEX: &'static str = "google-vertex";
    /// `"rpi-messages"` (de-pi brand rename; upstream value was
    /// `"pi-messages"`, still accepted on input as a legacy alias —
    /// [`normalize_api_kind`]).
    pub const RPI_MESSAGES: &'static str = "rpi-messages";

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Legacy config-level alias: the pre-rename `"pi-messages"` spelling reads
/// back as `"rpi-messages"`. The kind string never appears on the wire (the
/// pi/rpi messages API is addressed via `{baseUrl}/messages`), so this is a
/// pure models.json/catalog compatibility shim. The virtual-model API id
/// (`pi-virtual` upstream, V16-12 §8-6) normalizes to its branded spelling
/// the same way.
fn normalize_api_kind(raw: String) -> String {
    match raw.as_str() {
        "pi-messages" => "rpi-messages".to_owned(),
        "pi-virtual" | "rpi-virtual" => "rpi-virtual".to_owned(),
        _ => raw,
    }
}

impl<'de> Deserialize<'de> for ApiKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(ApiKind(normalize_api_kind(raw)))
    }
}

impl fmt::Display for ApiKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ApiKind {
    fn from(s: &str) -> Self {
        Self(normalize_api_kind(s.to_owned()))
    }
}

impl From<String> for ApiKind {
    fn from(s: String) -> Self {
        Self(normalize_api_kind(s))
    }
}

// ---------------------------------------------------------------------------
// Thinking levels
// ---------------------------------------------------------------------------

/// `ThinkingLevel = "minimal" | "low" | "medium" | "high" | "xhigh" | "max"`.
///
/// Note: "xhigh" and "max" are only supported by selected model families; use
/// [`Model::thinking_level_map`] to detect support for a concrete model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ThinkingLevel {
    #[serde(rename = "minimal")]
    Minimal,
    #[serde(rename = "low")]
    Low,
    #[serde(rename = "medium")]
    Medium,
    #[serde(rename = "high")]
    High,
    #[serde(rename = "xhigh")]
    Xhigh,
    #[serde(rename = "max")]
    Max,
}

/// `ModelThinkingLevel = "off" | ThinkingLevel`.
///
/// This is also the agent-side `ThinkingLevel` of `packages/agent` (which
/// includes "off"); `rpi-agent` re-exports it under that name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ModelThinkingLevel {
    #[serde(rename = "off")]
    Off,
    #[serde(rename = "minimal")]
    Minimal,
    #[serde(rename = "low")]
    Low,
    #[serde(rename = "medium")]
    Medium,
    #[serde(rename = "high")]
    High,
    #[serde(rename = "xhigh")]
    Xhigh,
    #[serde(rename = "max")]
    Max,
}

/// `ThinkingLevelMap = Partial<Record<ModelThinkingLevel, string | null>>`.
///
/// Three-state per level: key absent = provider default, `None` (JSON null) =
/// level unsupported, `Some(value)` = provider/model-specific mapped value.
pub type ThinkingLevelMap = std::collections::BTreeMap<ModelThinkingLevel, Option<String>>;

/// `SamplingParams` (types.ts:92 @ abe508e1b): arbitrary sampling parameters
/// merged into the request body as-is.
pub type SamplingParams = serde_json::Map<String, Value>;

/// `SamplingParamsByThinkingLevel` (types.ts:93 @ abe508e1b; R2.3.1):
/// per-thinking-level sampling parameter overrides. Applies to the
/// `openai-completions` / `openai-responses` / `azure-openai-responses` APIs.
pub type SamplingParamsByThinkingLevel =
    std::collections::BTreeMap<ModelThinkingLevel, SamplingParams>;

impl ThinkingLevel {
    /// The corresponding [`ModelThinkingLevel`] variant.
    pub fn to_model_level(self) -> ModelThinkingLevel {
        match self {
            ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
            ThinkingLevel::Low => ModelThinkingLevel::Low,
            ThinkingLevel::Medium => ModelThinkingLevel::Medium,
            ThinkingLevel::High => ModelThinkingLevel::High,
            ThinkingLevel::Xhigh => ModelThinkingLevel::Xhigh,
            ThinkingLevel::Max => ModelThinkingLevel::Max,
        }
    }

    /// Inverse of [`Self::to_model_level`]: `"off"` maps to `None`
    /// (upstream `thinkingLevel === "off" ? undefined : thinkingLevel`),
    /// everything else back to its [`ThinkingLevel`] variant. Used where a
    /// [`ModelThinkingLevel`] (off-inclusive, e.g. `StreamOptions.reasoning`)
    /// is converted into [`SimpleStreamOptions`] reasoning at the
    /// `stream_simple` boundary.
    pub fn from_model_level(level: ModelThinkingLevel) -> Option<Self> {
        match level {
            ModelThinkingLevel::Off => None,
            ModelThinkingLevel::Minimal => Some(ThinkingLevel::Minimal),
            ModelThinkingLevel::Low => Some(ThinkingLevel::Low),
            ModelThinkingLevel::Medium => Some(ThinkingLevel::Medium),
            ModelThinkingLevel::High => Some(ThinkingLevel::High),
            ModelThinkingLevel::Xhigh => Some(ThinkingLevel::Xhigh),
            ModelThinkingLevel::Max => Some(ThinkingLevel::Max),
        }
    }
}

impl ModelThinkingLevel {
    /// The wire-format level name.
    pub fn as_str(self) -> &'static str {
        match self {
            ModelThinkingLevel::Off => "off",
            ModelThinkingLevel::Minimal => "minimal",
            ModelThinkingLevel::Low => "low",
            ModelThinkingLevel::Medium => "medium",
            ModelThinkingLevel::High => "high",
            ModelThinkingLevel::Xhigh => "xhigh",
            ModelThinkingLevel::Max => "max",
        }
    }
}

/// `ChatTemplateKwargValue`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChatTemplateKwargValue {
    /// `{ $var: "thinking.enabled" | "thinking.effort", omitWhenOff?: boolean }`
    Var(ChatTemplateKwargVar),
    /// Plain `string | number | boolean | null`.
    Scalar(Value),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTemplateKwargVar {
    #[serde(rename = "$var")]
    pub var: ChatTemplateKwargVarKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub omit_when_off: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatTemplateKwargVarKind {
    #[serde(rename = "thinking.enabled")]
    ThinkingEnabled,
    #[serde(rename = "thinking.effort")]
    ThinkingEffort,
    /// b23741269 (#8275): the clamped thinking budget (same value the
    /// top-level budget field would carry).
    #[serde(rename = "thinking.budget")]
    ThinkingBudget,
}

/// Token budgets for each thinking level (token-based providers only).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ThinkingBudgets {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimal: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub low: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub medium: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub high: Option<u32>,
}

// ---------------------------------------------------------------------------
// Stream options
// ---------------------------------------------------------------------------

/// `CacheRetention = "none" | "short" | "long"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CacheRetention {
    #[serde(rename = "none")]
    None,
    #[serde(rename = "short")]
    Short,
    #[serde(rename = "long")]
    Long,
}

/// `Transport = "sse" | "websocket" | "websocket-cached" | "auto"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transport {
    #[serde(rename = "sse")]
    Sse,
    #[serde(rename = "websocket")]
    Websocket,
    #[serde(rename = "websocket-cached")]
    WebsocketCached,
    #[serde(rename = "auto")]
    Auto,
}

/// `SessionAffinityFormat = "openai" | "openai-nosession" | "openrouter"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionAffinityFormat {
    #[serde(rename = "openai")]
    Openai,
    #[serde(rename = "openai-nosession")]
    OpenaiNosession,
    #[serde(rename = "openrouter")]
    Openrouter,
}

/// Provider-scoped environment overrides. Values take precedence over process.env.
pub type ProviderEnv = HashMap<String, String>;
/// Header map; a `None` value suppresses a provider/API default header of the
/// same name.
pub type ProviderHeaders = HashMap<String, Option<String>>;

/// `ProviderResponse` — passed to the `on_response` callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
}

/// `onPayload` callback: inspect or replace a provider payload before sending.
/// Returning `None` keeps the payload unchanged.
pub type OnPayloadCallback =
    Arc<dyn Fn(Value, &Model) -> Pin<Box<dyn Future<Output = Option<Value>> + Send>> + Send + Sync>;

/// `onResponse` callback: invoked after an HTTP response is received and
/// before its body stream is consumed.
pub type OnResponseCallback =
    Arc<dyn Fn(ProviderResponse, &Model) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// `TelemetryContext` placeholder (pi-telemetry is [DEFER], v0.11 requirements
/// §1.2). Opaque unit struct: keeps the
/// [`ProviderRequestOptions::telemetry_context`] field position and pass-through
/// channel without implementing any telemetry pipeline (G4 red line).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct TelemetryContext;

/// Wire-level HTTP request handed to a custom [`FetchFn`] (R2.7.4). Neutral
/// value type (no reqwest coupling in the public type layer); adapters
/// translate their built request into this shape when a custom fetch is set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchRequest {
    pub method: String,
    pub url: String,
    /// Header pairs in wire order; duplicate names are allowed.
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

/// Error returned by a custom [`FetchFn`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("custom fetch failed: {0}")]
pub struct FetchError(pub String);

/// Streaming HTTP response returned by a custom [`FetchFn`]. The body is a
/// byte-chunk stream so SSE adapters can parse incrementally.
pub struct FetchResponse {
    pub status: u16,
    /// Header pairs in wire order; duplicate names are allowed.
    pub headers: Vec<(String, String)>,
    pub body: Pin<Box<dyn Stream<Item = Result<Vec<u8>, FetchError>> + Send>>,
}

impl fmt::Debug for FetchResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FetchResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .field("body", &"<stream>")
            .finish()
    }
}

/// `FetchFunction` (`typeof globalThis.fetch`) — optional custom HTTP transport
/// for provider requests (R2.7.4). Does not affect WebSocket transports.
///
/// Adapters consume this through `utils::custom_fetch::send_provider_request`
/// (T20 Wave D): the built reqwest request is translated into [`FetchRequest`]
/// and the response bridged back, leaving the default reqwest path untouched.
/// The Google adapters reject any injected fetch instead (they have no fetch
/// indirection to thread it through — upstream `fetch !== globalThis.fetch`
/// check; rpi has no ambient global fetch, so any `Some` is non-default).
pub type FetchFn = Arc<
    dyn Fn(FetchRequest) -> Pin<Box<dyn Future<Output = Result<FetchResponse, FetchError>> + Send>>
        + Send
        + Sync,
>;

/// `ProviderRequestOptions` — authentication, HTTP transport, and lifecycle
/// callbacks shared by provider requests (v0.84, R2.8.1).
///
/// Internal type (never serialized); field docs mirror upstream.
#[derive(Clone, Default)]
pub struct ProviderRequestOptions {
    pub signal: Option<CancellationToken>,
    /// Explicit parent context for telemetry produced by this logical request.
    /// Placeholder only — the telemetry pipeline is [DEFER] (v0.11
    /// requirements §1.2).
    pub telemetry_context: Option<TelemetryContext>,
    pub api_key: Option<String>,
    /// Optional fetch implementation for provider HTTP requests (R2.7.4).
    /// Defaults to the built-in reqwest transport — see [`FetchFn`].
    pub fetch: Option<FetchFn>,
    /// Provider-scoped environment values, taking precedence over process.env.
    pub env: Option<ProviderEnv>,
    pub on_payload: Option<OnPayloadCallback>,
    pub on_response: Option<OnResponseCallback>,
    /// Custom HTTP headers, merged over provider defaults; `None` suppresses a
    /// default header. On Bedrock, reserved headers (`x-amz-*`,
    /// `authorization`, `host`) are silently ignored to preserve SigV4.
    pub headers: Option<ProviderHeaders>,
    /// HTTP request timeout in milliseconds.
    pub timeout_ms: Option<u64>,
    /// Maximum client-side retry attempts.
    pub max_retries: Option<u32>,
    /// Maximum delay (ms) to wait for a server-requested long retry wait.
    /// Default: 60000; 0 disables the cap.
    pub max_retry_delay_ms: Option<u64>,
}
/// Provider-side deferred-response fetch options. A zero `wait` performs a
/// single status check; providers may long-poll up to the requested duration.
#[derive(Clone, Default)]
pub struct DeferredFetchOptions {
    pub request: ProviderRequestOptions,
    pub wait: Option<u64>,
}

/// Provider-side deferred-response cancellation options.
#[derive(Clone, Default)]
pub struct DeferredCancelOptions {
    pub request: ProviderRequestOptions,
}

// Manual Debug: the `on_payload` / `on_response` / `fetch` trait objects are
// not Debug. `api_key` and header values are redacted — credentials must never
// appear in Debug output (coding-standards §11.1/§11.2).
impl fmt::Debug for ProviderRequestOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderRequestOptions")
            .field("signal", &self.signal)
            .field("telemetry_context", &self.telemetry_context)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("fetch", &self.fetch.as_ref().map(|_| "<callback>"))
            .field("env", &self.env)
            .field(
                "on_payload",
                &self.on_payload.as_ref().map(|_| "<callback>"),
            )
            .field(
                "on_response",
                &self.on_response.as_ref().map(|_| "<callback>"),
            )
            .field(
                "headers",
                &self.headers.as_ref().map(|h| h.keys().collect::<Vec<_>>()),
            )
            .field("timeout_ms", &self.timeout_ms)
            .field("max_retries", &self.max_retries)
            .field("max_retry_delay_ms", &self.max_retry_delay_ms)
            .finish()
    }
}

/// Identifies the core operation that owns a model request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestOrigin {
    /// Operation kind.
    pub kind: RequestOriginKind,
    /// Stable invocation or task identifier.
    pub id: String,
}

/// Core-owned model request purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestOriginKind {
    /// User-visible conversation generation.
    Generation,
    /// Internal conversation summary generation.
    Compaction,
}

/// `StreamOptions extends ProviderRequestOptions` — base options all providers
/// share, plus the chat-sampling and transport knobs.
///
/// TS interface inheritance is expressed as composition (see file header):
/// the inherited fields live in [`StreamOptions::request`] and are reachable
/// directly via `Deref`/`DerefMut`, e.g. `options.timeout_ms`.
///
/// Internal type (never serialized); field docs mirror upstream.
#[derive(Clone, Default)]
pub struct StreamOptions {
    /// The inherited `ProviderRequestOptions` (`signal`, `api_key`, `fetch`,
    /// `env`, `on_payload`, `on_response`, `headers`, `timeout_ms`,
    /// `max_retries`, `max_retry_delay_ms`, `telemetry_context`).
    pub request: ProviderRequestOptions,
    pub temperature: Option<f64>,
    /// Arbitrary sampling parameters merged into the request body as-is, after
    /// the named request fields, so keys here override them (R2.1.4). Merged
    /// over `Model.sampling_params` per key. Only OpenAI-compatible adapters
    /// apply it (merge semantics land in T20; this is the type channel).
    pub sampling_params: Option<serde_json::Map<String, Value>>,
    pub max_tokens: Option<u32>,
    /// Reasoning level for reasoning-capable models. Upstream carries this on
    /// `SimpleStreamOptions`; the rpi-agent `StreamFn` shape (design §4.4)
    /// takes plain `StreamOptions`, so the channel lives here instead
    /// (compaction `createSummarizationOptions`, compaction.ts:539-553).
    /// `ModelThinkingLevel` (off-inclusive) because the compaction caller
    /// passes the agent-side thinking level.
    pub reasoning: Option<ModelThinkingLevel>,
    /// Preferred transport for providers that support multiple transports.
    pub transport: Option<Transport>,
    /// Prompt cache retention preference. Default: `Short`.
    pub cache_retention: Option<CacheRetention>,
    /// Optional session identifier for providers that support session-based caching.
    pub session_id: Option<String>,
    /// WebSocket connect (handshake) timeout in milliseconds.
    pub websocket_connect_timeout_ms: Option<u64>,
    /// Optional metadata; providers extract the fields they understand.
    pub metadata: Option<serde_json::Map<String, Value>>,
    /// Core-only request attribution; never serialized or sent to providers.
    pub origin: Option<RequestOrigin>,
}
impl std::ops::Deref for StreamOptions {
    type Target = ProviderRequestOptions;

    fn deref(&self) -> &Self::Target {
        &self.request
    }
}

impl std::ops::DerefMut for StreamOptions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.request
    }
}

impl From<ProviderRequestOptions> for StreamOptions {
    fn from(request: ProviderRequestOptions) -> Self {
        StreamOptions {
            request,
            ..StreamOptions::default()
        }
    }
}

// Manual Debug: delegates the inherited fields to `ProviderRequestOptions`'s
// redacting Debug impl (coding-standards §11.1/§11.2).
impl fmt::Debug for StreamOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamOptions")
            .field("request", &self.request)
            .field("temperature", &self.temperature)
            .field("sampling_params", &self.sampling_params)
            .field("max_tokens", &self.max_tokens)
            .field("reasoning", &self.reasoning)
            .field("transport", &self.transport)
            .field("cache_retention", &self.cache_retention)
            .field("session_id", &self.session_id)
            .field(
                "websocket_connect_timeout_ms",
                &self.websocket_connect_timeout_ms,
            )
            .field("metadata", &self.metadata)
            .field("origin", &self.origin)
            .finish()
    }
}

/// `SimpleStreamOptions extends StreamOptions` — unified options with
/// reasoning, passed to `stream_simple` / `complete_simple`.
#[derive(Debug, Clone, Default)]
pub struct SimpleStreamOptions {
    pub stream: StreamOptions,
    pub reasoning: Option<ThinkingLevel>,
    /// Custom token budgets for thinking levels (token-based providers only).
    pub thinking_budgets: Option<ThinkingBudgets>,
    /// Provider-neutral tool selection for simple requests
    /// (e5dde9a76/#8607, types.ts:316): `"auto" | "none"`. When omitted,
    /// adapters use provider-specific behavior.
    pub tool_choice: Option<SimpleToolChoice>,
}

/// `SimpleStreamOptions.toolChoice` literal union.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SimpleToolChoice {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "none")]
    None,
}

// ---------------------------------------------------------------------------
// Content blocks and messages
// ---------------------------------------------------------------------------

/// Role of a [`Message`]. The per-struct `role` fields use single-variant
/// marker enums ([`UserRole`] etc.) so each message struct serializes with its
/// literal tag standalone (upstream includes `role` in every serialized
/// message object); this type is the ergonomic sum used by accessors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Role {
    #[serde(rename = "user")]
    User,
    #[serde(rename = "assistant")]
    Assistant,
    #[serde(rename = "toolResult")]
    ToolResult,
    #[serde(rename = "system")]
    System,
}

/// Role marker for [`UserMessage`] (`role: "user"`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserRole {
    #[default]
    #[serde(rename = "user")]
    User,
}

/// Role marker for [`AssistantMessage`] (`role: "assistant"`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssistantRole {
    #[default]
    #[serde(rename = "assistant")]
    Assistant,
}

/// Role marker for [`ToolResultMessage`] (`role: "toolResult"`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolResultRole {
    #[default]
    #[serde(rename = "toolResult")]
    ToolResult,
}

/// Role marker for [`SystemMessage`] (`role: "system"`, #9548).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SystemRole {
    #[default]
    #[serde(rename = "system")]
    System,
}

/// `TextSignatureV1` — structured payload that may appear inside
/// [`TextContent::text_signature`] as a JSON string (OpenAI responses message
/// metadata; legacy values are plain id strings).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextSignatureV1 {
    pub v: u8,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<TextSignaturePhase>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextSignaturePhase {
    #[serde(rename = "commentary")]
    Commentary,
    #[serde(rename = "final_answer")]
    FinalAnswer,
}

/// `TextContent`. The `type: "text"` tag is supplied by the enclosing
/// content-block enum during serialization.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextContent {
    pub text: String,
    /// e.g. for OpenAI responses, message metadata (legacy id string or
    /// [`TextSignatureV1`] JSON).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_signature: Option<String>,
}

/// `ThinkingContent`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingContent {
    pub thinking: String,
    /// e.g. for OpenAI responses, the reasoning item ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_signature: Option<String>,
    /// When true, the thinking content was redacted by safety filters. The
    /// opaque encrypted payload is stored in `thinking_signature` so it can be
    /// passed back to the API for multi-turn continuity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redacted: Option<bool>,
}

/// `ImageContent`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageContent {
    /// Base64 encoded image data.
    pub data: String,
    /// e.g. "image/jpeg", "image/png".
    pub mime_type: String,
}

/// `ToolCall`. The `type: "toolCall"` tag is supplied by the enclosing
/// content-block enum; use [`tagged_tool_call`] when a standalone `ToolCall`
/// must serialize with its tag (e.g. inside [`StreamEvent::ToolCallEnd`]).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Map<String, Value>,
    /// Google-specific: opaque signature for reusing thought context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
    /// OpenAI Responses namespace for calls to dynamically loaded or
    /// namespaced tools (v0.84, R2.1.3). Preserved across streaming, proxy
    /// (`toolcall_end` frames) and replay.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

/// Serde `with` module that serializes a [`ToolCall`] with its upstream
/// `type: "toolCall"` tag (for positions where a bare ToolCall object is on
/// the wire, e.g. the `toolcall_end` stream event).
pub mod tagged_tool_call {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::ToolCall;

    #[derive(Serialize, Deserialize)]
    struct Tagged {
        #[serde(rename = "type")]
        kind: ToolCallTag,
        #[serde(flatten)]
        call: ToolCall,
    }

    #[derive(Serialize, Deserialize)]
    enum ToolCallTag {
        #[serde(rename = "toolCall")]
        ToolCall,
    }

    pub fn serialize<S: Serializer>(call: &ToolCall, serializer: S) -> Result<S::Ok, S::Error> {
        Tagged {
            kind: ToolCallTag::ToolCall,
            call: call.clone(),
        }
        .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<ToolCall, D::Error> {
        Ok(Tagged::deserialize(deserializer)?.call)
    }
}

/// `AssistantMessage["content"][number]` = Text | Thinking | ToolCall.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AssistantContent {
    Text(TextContent),
    Thinking(ThinkingContent),
    ToolCall(ToolCall),
}

/// `UserMessage` content block = Text | Image.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum UserContentBlock {
    Text(TextContent),
    Image(ImageContent),
}

/// `ToolResultMessage["content"][number]` = Text | Image.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ToolResultContent {
    Text(TextContent),
    Image(ImageContent),
}

/// `UserMessage["content"]` = `string | (TextContent | ImageContent)[]`.
/// Also used for `CustomMessage.content` and `CustomMessageEntry.content`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    Text(String),
    Blocks(Vec<UserContentBlock>),
}

impl Default for UserContent {
    /// Matches the upstream null-content normalization `content: []`
    /// (transform-messages.ts:71-73).
    fn default() -> Self {
        UserContent::Blocks(Vec::new())
    }
}

/// Deserializes `null` as `Default::default()`. Tolerates untyped upstream
/// inputs — custom tools, hand-built histories, old session files — that carry
/// `content: null`; upstream normalizes these in `transformMessages`
/// (transform-messages.ts:71-73), which in Rust must happen at the
/// deserialization boundary since the types cannot hold null.
fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// `Usage`. Token counts are integers upstream; costs are dollars (floats).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Subset of `cache_write` written with 1h retention. Only Anthropic
    /// reports this split.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write1h: Option<u64>,
    /// Reasoning/thinking tokens when the provider reports them; a subset of
    /// `output`. Absent when the provider exposes no reasoning breakdown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    pub total_tokens: u64,
    pub cost: UsageCost,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub total: f64,
}

impl Usage {
    /// `combineUsage` (usage-totals.ts:31 @ abe508e1b): field-wise sum;
    /// optional splits stay `None` when both sides are `None`.
    pub fn combined(&self, other: &Usage) -> Usage {
        Usage {
            input: self.input + other.input,
            output: self.output + other.output,
            cache_read: self.cache_read + other.cache_read,
            cache_write: self.cache_write + other.cache_write,
            cache_write1h: match (self.cache_write1h, other.cache_write1h) {
                (None, None) => None,
                (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
            },
            reasoning: match (self.reasoning, other.reasoning) {
                (None, None) => None,
                (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
            },
            total_tokens: self.total_tokens + other.total_tokens,
            cost: UsageCost {
                input: self.cost.input + other.cost.input,
                output: self.cost.output + other.cost.output,
                cache_read: self.cost.cache_read + other.cost.cache_read,
                cache_write: self.cost.cache_write + other.cost.cache_write,
                total: self.cost.total + other.cost.total,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Images (image generation subsystem: `images.ts` / `images-models.ts` /
// `image-models.ts` / `images-api-registry.ts` / `api/openrouter-images.ts`)
// ---------------------------------------------------------------------------

/// `ImagesApi = KnownImagesApi | (string & {})` — open string with the known
/// constant, mirroring [`ApiKind`] for the image-generation side.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ImagesApiKind(pub String);

impl ImagesApiKind {
    pub const OPENROUTER_IMAGES: &'static str = "openrouter-images";

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ImagesApiKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ImagesApiKind {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl From<String> for ImagesApiKind {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// `ImagesStopReason = "stop" | "error" | "aborted"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImagesStopReason {
    #[serde(rename = "stop")]
    Stop,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "aborted")]
    Aborted,
}

/// `ImagesInputContent` — `ImagesContext.input` block = Text | Image.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ImagesInputContent {
    Text(TextContent),
    Image(ImageContent),
}

/// `ImagesOutputContent` — `AssistantImages.output` block = Text | Image.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ImagesOutputContent {
    Text(TextContent),
    Image(ImageContent),
}

/// `ImagesContext`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImagesContext {
    pub input: Vec<ImagesInputContent>,
}

/// `AssistantImages` — the uniform image-generation result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantImages {
    pub api: ImagesApiKind,
    pub provider: String,
    pub model: String,
    pub output: Vec<ImagesOutputContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub stop_reason: ImagesStopReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Unix timestamp in milliseconds.
    pub timestamp: i64,
}

/// `ImagesModel.output` entry: `"text" | "image"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImagesOutputModality {
    #[serde(rename = "text")]
    Text,
    #[serde(rename = "image")]
    Image,
}

/// `ImagesModel<TApi>` — `Omit<Model, "api" | "provider" | "reasoning" |
/// "contextWindow" | "maxTokens" | "compat">` with `api`/`provider`
/// re-added and `output` (`"text" | "image"`). The `TApi` type parameter
/// collapses (all image models flow through the untyped registry, mirroring
/// the upstream `ImagesApi` open union).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImagesModel {
    pub id: String,
    pub name: String,
    pub api: ImagesApiKind,
    pub provider: String,
    pub base_url: String,
    pub input: Vec<InputModality>,
    pub output: Vec<ImagesOutputModality>,
    pub cost: ModelCost,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<std::collections::BTreeMap<String, String>>,
}

/// `ImagesOptions.onPayload` — inspect or replace the wire JSON payload
/// before sending. Returning `None` keeps the payload unchanged.
pub type ImagesOnPayloadCallback = Arc<
    dyn Fn(
            serde_json::Value,
            &ImagesModel,
        ) -> Pin<Box<dyn Future<Output = Option<serde_json::Value>> + Send>>
        + Send
        + Sync,
>;

/// `ImagesOptions.onResponse` — invoked after an HTTP response is received.
pub type ImagesOnResponseCallback = Arc<
    dyn Fn(ProviderResponse, &ImagesModel) -> Pin<Box<dyn Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// `ImagesOptions` — base options for image generation, mirroring
/// `StreamOptions` minus the chat-only fields.
#[derive(Clone, Default)]
pub struct ImagesOptions {
    pub signal: Option<CancellationToken>,
    pub api_key: Option<String>,
    /// Optional fetch implementation for provider HTTP requests (R2.7.4;
    /// upstream `ImagesOptions.fetch`, 027a58479). See [`FetchFn`].
    pub fetch: Option<FetchFn>,
    /// Provider-scoped environment values; precedence over process.env.
    pub env: Option<ProviderEnv>,
    pub on_payload: Option<ImagesOnPayloadCallback>,
    pub on_response: Option<ImagesOnResponseCallback>,
    /// Custom HTTP headers, merged over provider defaults; `None` suppresses
    /// a default header.
    pub headers: Option<ProviderHeaders>,
    /// HTTP request timeout in milliseconds.
    pub timeout_ms: Option<u64>,
    /// Maximum client-side retry attempts.
    pub max_retries: Option<u32>,
    /// Maximum delay (ms) to wait for a server-requested long retry wait.
    /// Default: 60000; 0 disables the cap.
    pub max_retry_delay_ms: Option<u64>,
    /// Optional metadata; providers extract the fields they understand.
    pub metadata: Option<serde_json::Map<String, Value>>,
}

// Manual Debug: the callback trait objects are not Debug; `api_key` and
// header values are redacted (coding-standards §11.1/§11.2).
impl fmt::Debug for ImagesOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImagesOptions")
            .field("signal", &self.signal)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("fetch", &self.fetch.as_ref().map(|_| "<callback>"))
            .field("env", &self.env)
            .field(
                "on_payload",
                &self.on_payload.as_ref().map(|_| "<callback>"),
            )
            .field(
                "on_response",
                &self.on_response.as_ref().map(|_| "<callback>"),
            )
            .field(
                "headers",
                &self.headers.as_ref().map(|h| h.keys().collect::<Vec<_>>()),
            )
            .field("timeout_ms", &self.timeout_ms)
            .field("max_retries", &self.max_retries)
            .field("max_retry_delay_ms", &self.max_retry_delay_ms)
            .field("metadata", &self.metadata)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Classifiers (classifier subsystem: `types.ts` Classifier*,
// `api/system-one-shared.ts`; V16-07 FR-F)
// ---------------------------------------------------------------------------

/// `ClassifierChoiceQuestion` (types.ts:633-637 @ abe508e1b).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifierChoiceQuestion {
    pub instructions: String,
    pub criteria: std::collections::BTreeMap<String, String>,
}

/// `ClassifierScoreQuestion` (types.ts:639-642).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifierScoreQuestion {
    pub instructions: String,
    pub criteria: Vec<String>,
}

/// `ClassifierBoolQuestion` (types.ts:644-647).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifierBoolQuestion {
    pub instructions: String,
    pub criteria: ClassifierBoolCriteria,
}

/// `{ true: string, false: string }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassifierBoolCriteria {
    #[serde(rename = "true")]
    pub yes: String,
    #[serde(rename = "false")]
    pub no: String,
}

/// `ClassifierQuestion` (types.ts:649): tagged by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ClassifierQuestion {
    Choice(ClassifierChoiceQuestion),
    Score(ClassifierScoreQuestion),
    Bool(ClassifierBoolQuestion),
}

/// `ClassifierContext` (types.ts:668-676 @ abe508e1b).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifierContext {
    pub state: serde_json::Map<String, Value>,
    /// Images judged together with `state`. Only models whose `input`
    /// includes `"image"` accept them; other models return an error result
    /// (ce8972a0e, R2.4.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageContent>>,
    pub questions: std::collections::BTreeMap<String, ClassifierQuestion>,
}

/// `ClassifierChoiceAnswer` (types.ts:656-661).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifierChoiceAnswer {
    pub choice: String,
    pub probabilities: std::collections::BTreeMap<String, f64>,
    pub confidence: f64,
}

/// `ClassifierScoreAnswer` (types.ts:663-667).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifierScoreAnswer {
    pub score: f64,
    pub confidence: f64,
}

/// `ClassifierBoolAnswer` (types.ts:669-672): the wire form is `noul`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifierBoolAnswer {
    pub probability: f64,
}

/// `ClassifierAnswer` (types.ts:674) — tagged by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ClassifierAnswer {
    Choice(ClassifierChoiceAnswer),
    Score(ClassifierScoreAnswer),
    Bool(ClassifierBoolAnswer),
}

/// `ClassifierStopReason = "stop" | "error" | "aborted"` (types.ts:675).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClassifierStopReason {
    Stop,
    Error,
    Aborted,
}

/// `ClassifierResult` (types.ts:677-690).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifierResult {
    pub api: ApiKind,
    pub provider: String,
    pub model: String,
    pub answers: std::collections::BTreeMap<String, ClassifierAnswer>,
    /// Token usage and its cost at the model's catalog price, when the
    /// service reports token counts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub stop_reason: ClassifierStopReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Unix timestamp in milliseconds.
    pub timestamp: i64,
}

/// `ClassifierOptions.onPayload` (types.ts:324-331) for classifier requests.
pub type ClassifierOnPayloadCallback = Arc<
    dyn Fn(Value, &ClassifierModel) -> Pin<Box<dyn Future<Output = Option<Value>> + Send>>
        + Send
        + Sync,
>;

/// `ClassifierOptions.onResponse`.
pub type ClassifierOnResponseCallback = Arc<
    dyn Fn(ProviderResponse, &ClassifierModel) -> Pin<Box<dyn Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// `ClassifierOptions` (types.ts:324-333): request options plus
/// `temperature`.
#[derive(Clone, Default)]
pub struct ClassifierOptions {
    pub signal: Option<CancellationToken>,
    pub api_key: Option<String>,
    pub fetch: Option<FetchFn>,
    pub env: Option<ProviderEnv>,
    pub on_payload: Option<ClassifierOnPayloadCallback>,
    pub on_response: Option<ClassifierOnResponseCallback>,
    pub headers: Option<ProviderHeaders>,
    pub timeout_ms: Option<u64>,
    pub max_retries: Option<u32>,
    pub max_retry_delay_ms: Option<u64>,
    /// Divides the answer logits by this value before they are normalized
    /// into probabilities. Must be positive when present.
    pub temperature: Option<f64>,
}

impl fmt::Debug for ClassifierOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClassifierOptions")
            .field("signal", &self.signal)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("fetch", &self.fetch.as_ref().map(|_| "<callback>"))
            .field("env", &self.env)
            .field(
                "on_payload",
                &self.on_payload.as_ref().map(|_| "<callback>"),
            )
            .field(
                "on_response",
                &self.on_response.as_ref().map(|_| "<callback>"),
            )
            .field(
                "headers",
                &self.headers.as_ref().map(|h| h.keys().collect::<Vec<_>>()),
            )
            .field("timeout_ms", &self.timeout_ms)
            .field("max_retries", &self.max_retries)
            .field("max_retry_delay_ms", &self.max_retry_delay_ms)
            .field("temperature", &self.temperature)
            .finish()
    }
}

/// `ProviderClassifier` (types.ts:316-322): the uniform contract of a
/// classifier API implementation.
pub trait ProviderClassifier: Send + Sync {
    fn classify(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
        options: Option<&ClassifierOptions>,
    ) -> Pin<Box<dyn Future<Output = ClassifierResult> + Send + 'static>>;
}

/// `Provider.generateImages` for the unified schema-v6 `ImageModel`
/// (models.ts:221-226 @ abe508e1b). The older image subsystem's
/// `images_models::ProviderImages` stays as is; this trait is the unified
/// runtime's contract.
pub trait ProviderImageGenerator: Send + Sync {
    fn generate_images(
        &self,
        model: &ImageModel,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
    ) -> Pin<Box<dyn Future<Output = AssistantImages> + Send + 'static>>;
}

/// `StopReason = "pending" | "stop" | "length" | "toolUse" | "error" |
/// "aborted" | "deferred"`.
///
/// `pending` exists in the type but is transient only: it is never persisted
/// to session JSONL (only `message_end` persists, and partials never produce
/// `message_end`) — requirements §4.1.
///
/// `deferred` marks a request handed off to a provider-side background
/// lifecycle. Providers implement deferred polling and cancellation through
/// [`crate::models::Provider::fetch_deferred`] and
/// [`crate::models::Provider::cancel_deferred`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "stop")]
    Stop,
    #[serde(rename = "length")]
    Length,
    #[serde(rename = "toolUse")]
    ToolUse,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "aborted")]
    Aborted,
    #[serde(rename = "deferred")]
    Deferred,
}

/// Durable provider-side deferred-response handle, returned with an assistant
/// message whose stop reason is [`StopReason::Deferred`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeferredHandle {
    pub provider: String,
    pub model_id: String,
    pub api: String,
    /// Provider token, such as a response id or batch id plus row id.
    pub id: String,
    /// Unix timestamp in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poll_after_ms: Option<u64>,
    /// Provider conversion data required to reconstruct the final assistant
    /// message. Arbitrary JSON (upstream `JsonValue`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// `SystemMessage` (#9548, 9e05370b2 — types.ts:480-514): system instructions
/// and tool declarations at one point in the transcript.
///
/// The leading system message is the system prompt. Later system messages
/// change it: `content` adds instructions from that point on, `sections`
/// replace or remove named prompt sections, and `toolsAdded`/`toolsRemoved`
/// change the tool set. Replaying every system message in order yields the
/// current prompt and tools. Providers that accept system messages
/// mid-conversation send each one in place; other providers rebuild the
/// leading system message from the replayed state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemMessage {
    pub role: SystemRole,
    /// Instruction text. On the leading message this is the base prompt;
    /// later, additional instructions. Both a missing key and an explicit
    /// `null` normalize to `""` (upstream `message.content == null` covers
    /// both; session-manager.ts:397 @ #9548).
    #[serde(default, deserialize_with = "null_default")]
    pub content: SystemContent,
    /// Named, ordered prompt sections rendered verbatim after `content`.
    /// The leading message declares them; later messages replace sections
    /// by name, and `null` removes one. Ordered via `serde_json`'s
    /// `preserve_order` (upstream TS object literal ordering).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sections: Option<serde_json::Map<String, Value>>,
    /// Complete definitions of tools that become available at this point.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools_added: Option<Vec<Tool>>,
    /// Tools that stop being available at this point.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools_removed: Option<Vec<ToolReference>>,
    /// Unix timestamp in milliseconds.
    pub timestamp: i64,
}

impl SystemMessage {
    /// Read one section's value: `None` when absent, `Some(None)` when the
    /// name maps to JSON `null` (the removal marker).
    pub fn section(&self, name: &str) -> Option<Option<&str>> {
        self.sections
            .as_ref()
            .and_then(|sections| sections.get(name))
            .map(|value| value.as_str())
    }

    /// Iterate `(name, value)` pairs in declaration order; `null` values
    /// surface as `None` (removal marker).
    pub fn iter_sections(&self) -> impl Iterator<Item = (&str, Option<&str>)> {
        self.sections
            .iter()
            .flatten()
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }
}

/// `SystemMessage.content: string | TextContent[]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemContent {
    Text(String),
    Blocks(Vec<TextContent>),
}

impl Default for SystemContent {
    fn default() -> Self {
        SystemContent::Text(String::new())
    }
}

/// `ToolReference` — a tool named by reference (toolsRemoved entries).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolReference {
    pub name: String,
}

/// `UserMessage`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMessage {
    pub role: UserRole,
    #[serde(deserialize_with = "null_default")]
    pub content: UserContent,
    /// Unix timestamp in milliseconds.
    pub timestamp: i64,
}

/// `AssistantMessage`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    pub role: AssistantRole,
    #[serde(deserialize_with = "null_default")]
    pub content: Vec<AssistantContent>,
    pub api: ApiKind,
    pub provider: String,
    pub model: String,
    /// Concrete `chunk.model` when different from the requested `model`
    /// (e.g. OpenRouter `auto` -> `anthropic/...`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    /// Provider-specific response/message identifier when exposed upstream.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    /// Exact provider-native effort level used for this response. Absent
    /// for legacy or unmanaged responses (types.ts:435-436). Type face +
    /// proxy passthrough landed in V14-04; the Anthropic write/replay
    /// logic is V14-05 (4e69b0c28 split across tasks).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_thinking_level: Option<String>,
    /// Pi thinking level the agent loop requested for this response. Absent
    /// outside the agent loop and for legacy responses (R2.7.1,
    /// types.ts:553-554 @ abe508e1b). The agent loop records it on every
    /// finalized assistant message (agent-loop.ts:409).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ModelThinkingLevel>,
    /// Redacted provider/runtime diagnostics for failures and recoveries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<AssistantMessageDiagnostic>>,
    pub usage: Usage,
    pub stop_reason: StopReason,
    /// Handle to a provider-side deferred response when `stop_reason` is
    /// `Deferred`. Boxed to keep the `Message` enum small; the serialized
    /// handle carries the request identity needed to poll or cancel it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deferred: Option<Box<DeferredHandle>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// The provider's raw, unmapped stop reason (v0.84, R2.1.2). Populated by
    /// the adapters (Anthropic/Google/Bedrock/Mistral/OpenAI completions and
    /// responses) in T19/T20.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_stop_reason: Option<String>,
    /// Provider indication of whether the model explicitly ended its turn
    /// (v0.84, R2.1.2; Codex debugging). Preserved for debugging and does not
    /// affect agent control flow.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_turn: Option<bool>,
    /// Unix timestamp in milliseconds.
    pub timestamp: i64,
    /// Milliseconds from `timestamp` until the response ended, measured with
    /// a monotonic clock. Set by the assistant message event stream on the
    /// final message of a response it saw start; absent for legacy messages
    /// and for deferred results fetched later (R2.7.1, types.ts:571-575
    /// @ abe508e1b).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// A tool call that another tool made while it ran, for example from a
/// codemode script (types.ts:570-581 @ abe508e1b).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedToolCallRecord {
    pub id: String,
    pub name: String,
    /// Omitted when over the size limits; `argumentsBytes` then gives their size.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
    /// UTF-8 size of the arguments as JSON, set when `arguments` is omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments_bytes: Option<u64>,
    /// `unfinished`: the call was still running when the calling tool finished.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Error text, truncated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Bounded record of the nested calls a tool made. Results are not recorded
/// (types.ts:583-588 @ abe508e1b).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedToolCalls {
    pub calls: Vec<NestedToolCallRecord>,
    /// False when calls were dropped, arguments omitted, or calls had not
    /// finished.
    pub complete: bool,
}

/// `ToolResultMessage`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultMessage {
    pub role: ToolResultRole,
    pub tool_call_id: String,
    pub tool_name: String,
    /// Supports text and images.
    #[serde(deserialize_with = "null_default")]
    pub content: Vec<ToolResultContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    /// Usage from the tool execution itself, if available. Not part of main
    /// LLM context accounting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Calls this tool made to other tools. Kept for the session record; not
    /// sent to the model (R3.4.6 payload note; writer is V16-06).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nested_calls: Option<NestedToolCalls>,
    pub is_error: bool,
    /// Unix timestamp in milliseconds.
    pub timestamp: i64,
    /// Milliseconds the tool's execution took, measured with a monotonic
    /// clock. Absent for legacy results (R2.7.1, types.ts:595 @ abe508e1b).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// `Message = SystemMessage | UserMessage | AssistantMessage | ToolResultMessage`
/// (post-#9548 order; system rides first-class for mid-conversation
/// semantics).
///
/// Untagged: the per-struct role markers disambiguate the variants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Message {
    System(SystemMessage),
    User(UserMessage),
    Assistant(AssistantMessage),
    ToolResult(ToolResultMessage),
}

impl Message {
    pub fn role(&self) -> Role {
        match self {
            Message::System(_) => Role::System,
            Message::User(_) => Role::User,
            Message::Assistant(_) => Role::Assistant,
            Message::ToolResult(_) => Role::ToolResult,
        }
    }
}

// ---------------------------------------------------------------------------
// Diagnostics (packages/ai/src/utils/diagnostics.ts)
// ---------------------------------------------------------------------------

/// `AssistantMessageDiagnostic` (see `utils/diagnostics.ts`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantMessageDiagnostic {
    #[serde(rename = "type")]
    pub kind: String,
    pub timestamp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<DiagnosticErrorInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Map<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticErrorInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<NumberOrString>,
}

/// JSON `number | string` union (used by diagnostic error codes and OpenRouter
/// routing prices).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NumberOrString {
    Number(f64),
    String(String),
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

/// OpenAI grammar variants for constrained sampling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GrammarFormat {
    #[serde(rename = "openai_lark")]
    OpenaiLark,
    #[serde(rename = "openai_regex")]
    OpenaiRegex,
}

/// `GrammarVariants = Partial<Record<GrammarFormat, string>>`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrammarVariants {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openai_lark: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openai_regex: Option<String>,
}

/// `ConstrainedSamplingConfig`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConstrainedSamplingConfig {
    /// Roughly maps to the concept of `strict` in APIs which is implemented as
    /// json-schema constrained sampling.
    JsonSchema { strict: ConstrainedSamplingStrict },
    /// Provider-specific encodings of the same intended language.
    Grammar { variants: GrammarVariants },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConstrainedSamplingStrict {
    #[serde(rename = "prefer")]
    Prefer,
    #[serde(rename = "require")]
    Require,
}

/// `Tool.constrainedSampling?: false | ConstrainedSamplingConfig`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConstrainedSampling {
    Config(ConstrainedSamplingConfig),
    Disabled(bool),
}

/// `Tool`. `parameters` is a JSON Schema object (TypeBox upstream).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constrained_sampling: Option<ConstrainedSampling>,
}

/// `Context`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
}

/// Zero-sized brand type: its privacy keeps [`TranscriptContext`]
/// construction inside `rpi-ai` (upstream `transcriptContextBrand`,
/// types.ts:627-635).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TranscriptContextBrand;

/// Crate-internal full constructor (brand + messages); the only sanctioned
/// production path outside `normalize_context` is `collapse_system_messages`
/// / `resolve_transcript` in `utils/transcript.rs`.
pub(crate) fn make_transcript_context(messages: Vec<Message>) -> TranscriptContext {
    TranscriptContext {
        messages,
        _brand: TranscriptContextBrand,
    }
}

/// `TranscriptContext` (#9548): normalized request context passed to
/// providers and API implementations. The prompt and tool declarations are
/// carried by the transcript's system messages. Only
/// [`crate::utils::transcript::normalize_context`] produces this type, so a
/// raw [`Context`] cannot reach provider code by accident.
///
/// Serde note: the JSON form is exactly `{"messages": [...]}` (the TS brand
/// symbol is invisible to `JSON.stringify`, so the proxy wire shape carries
/// only `messages`).
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptContext {
    pub messages: Vec<Message>,
    /// Upstream brands via a unique symbol; the private field plays the same
    /// role in Rust (external crates cannot construct the struct literally).
    _brand: TranscriptContextBrand,
}

impl Serialize for TranscriptContext {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("TranscriptContext", 1)?;
        state.serialize_field("messages", &self.messages)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for TranscriptContext {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            messages: Vec<Message>,
        }
        Ok(TranscriptContext {
            messages: Raw::deserialize(deserializer)?.messages,
            _brand: TranscriptContextBrand,
        })
    }
}

// ---------------------------------------------------------------------------
// Stream events (AssistantMessageEvent)
// ---------------------------------------------------------------------------

/// `Extract<StopReason, "stop" | "length" | "toolUse" | "deferred">` — reason
/// of a `done` event.
///
/// Note: the proxy frame protocol (`packages/agent/src/proxy.ts`) narrows this
/// to `"stop" | "length" | "toolUse"`. The shared Rust enum accepts `deferred`
/// in proxy frames too, which only widens parse tolerance — upstream does no
/// runtime validation either (TS types are erased), so behavior is identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DoneReason {
    #[serde(rename = "stop")]
    Stop,
    #[serde(rename = "length")]
    Length,
    #[serde(rename = "toolUse")]
    ToolUse,
    #[serde(rename = "deferred")]
    Deferred,
}

/// `Extract<StopReason, "aborted" | "error">` — reason of an `error` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorReason {
    #[serde(rename = "aborted")]
    Aborted,
    #[serde(rename = "error")]
    Error,
}

/// `AssistantMessageEvent` — event protocol for the assistant message event
/// stream. Called `StreamEvent` in rpi (design §3.2, coding-standards §4.1).
///
/// Successful streams emit `start` before partial updates and terminate with
/// `done`. A stream may terminate directly with `error` when request setup
/// fails before generation starts; after `start`, failures also terminate
/// with `error`. Direct `stream_simple` calls fail synchronously (the Rust
/// `Err` equivalent of upstream's throw) when request auth is missing
/// (types.ts:325-330 @ 9841914, 8b5899dce). Updates and `done` must never
/// appear before `start`.
///
/// `partial` is the shared live response-so-far helper, not an event-time
/// snapshot. Since V15-13 (rpi#53) it is shared behind an `Arc` — the
/// same accumulated message flows provider -> agent loop -> events ->
/// UI queue without deep copies (serde shape unchanged). Text and thinking blocks are empty when their `*_start` event
/// is emitted and grow only through their corresponding `*_delta` events
/// until the authoritative `*_end`. Redacted thinking may be complete at
/// start and emit no deltas. Tool-call arguments at `toolcall_start` are
/// provider-specific (empty for classic streaming JSON tools;
/// `{[input_property]: ""}` / `{[input_property]: item.input}` for
/// grammar/custom tools — 8b5899dce); `toolcall_delta` carries subsequent
/// JSON updates.
///
/// Contract: events for different content blocks may interleave; consumers
/// correlate by `content_index` (requirements §4.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum StreamEvent {
    Start {
        partial: Arc<AssistantMessage>,
    },
    TextStart {
        content_index: usize,
        partial: Arc<AssistantMessage>,
    },
    TextDelta {
        content_index: usize,
        delta: String,
        partial: Arc<AssistantMessage>,
    },
    TextEnd {
        content_index: usize,
        content: String,
        partial: Arc<AssistantMessage>,
    },
    ThinkingStart {
        content_index: usize,
        partial: Arc<AssistantMessage>,
    },
    ThinkingDelta {
        content_index: usize,
        delta: String,
        partial: Arc<AssistantMessage>,
    },
    ThinkingEnd {
        content_index: usize,
        content: String,
        partial: Arc<AssistantMessage>,
    },
    #[serde(rename = "toolcall_start")]
    ToolCallStart {
        content_index: usize,
        partial: Arc<AssistantMessage>,
    },
    #[serde(rename = "toolcall_delta")]
    ToolCallDelta {
        content_index: usize,
        delta: String,
        partial: Arc<AssistantMessage>,
    },
    #[serde(rename = "toolcall_end")]
    ToolCallEnd {
        content_index: usize,
        #[serde(with = "tagged_tool_call")]
        tool_call: ToolCall,
        partial: Arc<AssistantMessage>,
    },
    Done {
        reason: DoneReason,
        message: AssistantMessage,
    },
    Error {
        reason: ErrorReason,
        error: AssistantMessage,
    },
}

// ---------------------------------------------------------------------------
// Model and compat matrices
// ---------------------------------------------------------------------------

/// `ModelCostRates` — $/million tokens.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCostRates {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

/// `ModelCostTier extends ModelCostRates`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCostTier {
    #[serde(flatten)]
    pub rates: ModelCostRates,
    /// Use this tier for requests whose total input usage exceeds this token count.
    pub input_tokens_above: u64,
}

/// `ModelCost extends ModelCostRates`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCost {
    #[serde(flatten)]
    pub rates: ModelCostRates,
    /// Request-wide pricing tiers. The highest matching input threshold
    /// applies to the full request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tiers: Option<Vec<ModelCostTier>>,
}

/// `Model.input` entry: `"text" | "image"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputModality {
    #[serde(rename = "text")]
    Text,
    #[serde(rename = "image")]
    Image,
}

/// `ModelPromptCache` (types.ts:110-112, #9668): best-effort prompt cache
/// lifetime in seconds per retention tier a request can ask for. A missing
/// tier means the lifetime is unknown; the warmer (V15-05 scope) does not
/// warm such caches. Only direct Anthropic carries entries from the catalog
/// generator (`applyPromptCacheMetadata`, generate-models.ts:939-946 —
/// proxies are intentionally not annotated).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModelPromptCache {
    /// `"short"` tier lifetime (Anthropic ephemeral default: 300s).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short: Option<u32>,
    /// `"long"` tier lifetime (`ttl: "1h"`: 3600s).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub long: Option<u32>,
}

/// `ModelImageResizeOptions` (types.ts:958-964, #9631 / f5c946480):
/// cache-safe resize profile applied before a new image enters conversation
/// history. Every key is optional — unset keys keep the caller's defaults
/// (rpi's historical global limits in `rpi::tools::image_process`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelImageResizeOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_height: Option<u32>,
    /// Maximum base64-encoded payload size in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jpeg_quality: Option<u32>,
}

/// `ModelImageInputLimits` (types.ts:966-972).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelImageInputLimits {
    /// Cache-safe resize profile applied before a new image enters history.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resize: Option<ModelImageResizeOptions>,
    /// Maximum images accepted in one provider message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_per_message: Option<u32>,
    /// Maximum images accepted across one provider request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_per_request: Option<u32>,
}

/// `ModelInputLimits` (types.ts:974-978): provider input limits and
/// cache-safe preprocessing metadata (#9631). Parsing is additive — unknown
/// fields are ignored and a missing `inputLimits` yields `None` (models keep
/// their historical behavior; zero-regression red line).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInputLimits {
    /// Maximum serialized provider request size in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_request_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<ModelImageInputLimits>,
}

/// `Model` — unified model system entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    pub name: String,
    pub api: ApiKind,
    pub provider: String,
    pub base_url: String,
    pub reasoning: bool,
    /// Maps pi thinking levels to provider/model-specific values.
    /// Missing keys use provider defaults; null marks a level as unsupported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<ThinkingLevelMap>,
    pub input: Vec<InputModality>,
    /// Provider input limits and cache-safe preprocessing metadata
    /// (types.ts:994-995, #9631). Unset when the provider is unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_limits: Option<ModelInputLimits>,
    pub cost: ModelCost,
    /// Prompt cache lifetimes per retention tier (#9668, c596d09d9).
    /// Unset when the provider's cache behavior is unknown. Consumed by
    /// cache warming (V15-05); carried here so catalog data is not
    /// silently dropped at parse time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache: Option<ModelPromptCache>,
    pub context_window: u32,
    pub max_tokens: u32,
    /// Default sampling parameters for this model (v0.84, R2.1.4). See
    /// [`StreamOptions::sampling_params`]; per-request keys override these.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling_params: Option<serde_json::Map<String, Value>>,
    /// Per-thinking-level sampling parameter overrides (v1.0.2, R2.3.1):
    /// merged over `sampling_params` for the effective (clamped) thinking
    /// level, below the request's own keys. Unset keeps the request shape
    /// unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling_params_by_thinking_level: Option<SamplingParamsByThinkingLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<std::collections::BTreeMap<String, String>>,
    /// Compatibility overrides; which fields apply is governed by `api`
    /// (upstream types this conditionally per API; see file header note).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat: Option<ModelCompat>,
}

/// `ModelType` (types.ts:1158-1165 @ abe508e1b, schema v6): what a catalog
/// entry is for. Chat is the default and omits `type` in catalog data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ModelType {
    /// Chat model — usable with `stream()` and friends.
    #[default]
    Chat,
    /// Image-generation model — usable with `generateImages()` only.
    Image,
    /// Structured classifier model — usable with `classify()` only.
    Classifier,
}

impl ModelType {
    pub fn as_str(self) -> &'static str {
        match self {
            ModelType::Chat => "chat",
            ModelType::Image => "image",
            ModelType::Classifier => "classifier",
        }
    }
}

/// `ImageModel` (types.ts:1143-1156 @ abe508e1b): image-generation catalog
/// entry. Parsed through [`AnyModel`]; `AnyModel` dispatch tries this shape
/// before the classifier and chat shapes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageModel {
    #[serde(rename = "type", default = "image_model_type")]
    pub model_type: ModelType,
    pub id: String,
    pub name: String,
    pub api: ApiKind,
    pub provider: String,
    pub base_url: String,
    pub input: Vec<InputModality>,
    /// Provider input limits and cache-safe preprocessing metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_limits: Option<ModelInputLimits>,
    pub cost: ModelCost,
    /// Output modalities. Always includes `"image"`; `"text"` means the
    /// model can also return text blocks.
    pub output: Vec<InputModality>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<std::collections::BTreeMap<String, String>>,
}

/// `ClassifierModel` (types.ts:1150-1155 @ abe508e1b): structured classifier
/// catalog entry. Parsed through [`AnyModel`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifierModel {
    #[serde(rename = "type", default = "classifier_model_type")]
    pub model_type: ModelType,
    pub id: String,
    pub name: String,
    pub api: ApiKind,
    pub provider: String,
    pub base_url: String,
    pub input: Vec<InputModality>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_limits: Option<ModelInputLimits>,
    pub cost: ModelCost,
    pub context_window: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<std::collections::BTreeMap<String, String>>,
}

/// `AnyModel` (types.ts:1167-1168 @ abe508e1b): anything a provider can
/// list. Deserialization dispatches on the raw `type` field (missing =
/// chat), so each variant is unambiguous; unknown types are rejected and
/// skipped by callers (catalog/remote parse).
///
/// The chat variant is intentionally much larger than the image/classifier
/// variants (upstream `AnyModel` is a plain TS union); boxing would force
/// allocations through the hot chat path, so the size difference is
/// accepted here.
fn image_model_type() -> ModelType {
    ModelType::Image
}

fn classifier_model_type() -> ModelType {
    ModelType::Classifier
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum AnyModel {
    Image(ImageModel),
    Classifier(ClassifierModel),
    Chat(Model),
}

impl<'de> Deserialize<'de> for AnyModel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        match value.get("type").and_then(Value::as_str) {
            None | Some("chat") => serde_json::from_value(value)
                .map(AnyModel::Chat)
                .map_err(serde::de::Error::custom),
            Some("image") => serde_json::from_value(value)
                .map(AnyModel::Image)
                .map_err(serde::de::Error::custom),
            Some("classifier") => serde_json::from_value(value)
                .map(AnyModel::Classifier)
                .map_err(serde::de::Error::custom),
            Some(other) => Err(serde::de::Error::custom(format!(
                "unknown model type {other:?}"
            ))),
        }
    }
}

impl AnyModel {
    /// `getModelType` (utils/model-operations.ts @ abe508e1b).
    pub fn model_type(&self) -> ModelType {
        match self {
            AnyModel::Image(_) => ModelType::Image,
            AnyModel::Classifier(_) => ModelType::Classifier,
            AnyModel::Chat(_) => ModelType::Chat,
        }
    }

    pub fn id(&self) -> &str {
        match self {
            AnyModel::Image(model) => &model.id,
            AnyModel::Classifier(model) => &model.id,
            AnyModel::Chat(model) => &model.id,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            AnyModel::Image(model) => &model.name,
            AnyModel::Classifier(model) => &model.name,
            AnyModel::Chat(model) => &model.name,
        }
    }

    pub fn provider(&self) -> &str {
        match self {
            AnyModel::Image(model) => &model.provider,
            AnyModel::Classifier(model) => &model.provider,
            AnyModel::Chat(model) => &model.provider,
        }
    }

    pub fn api(&self) -> &ApiKind {
        match self {
            AnyModel::Image(model) => &model.api,
            AnyModel::Classifier(model) => &model.api,
            AnyModel::Chat(model) => &model.api,
        }
    }

    /// Merge key `type\0id` (remote-catalog-provider.ts:26-30 @ abe508e1b):
    /// the same upstream id may appear once per type.
    pub fn merge_key(&self) -> String {
        format!("{}\0{}", self.model_type().as_str(), self.id())
    }

    pub fn as_chat(&self) -> Option<&Model> {
        match self {
            AnyModel::Chat(model) => Some(model),
            _ => None,
        }
    }

    pub fn into_chat(self) -> Option<Model> {
        match self {
            AnyModel::Chat(model) => Some(model),
            _ => None,
        }
    }

    /// Stamp the owning provider id (remote catalog parse / hydration).
    pub fn set_provider(&mut self, provider: &str) {
        match self {
            AnyModel::Image(model) => model.provider = provider.to_owned(),
            AnyModel::Classifier(model) => model.provider = provider.to_owned(),
            AnyModel::Chat(model) => model.provider = provider.to_owned(),
        }
    }
}

impl From<Model> for AnyModel {
    fn from(model: Model) -> Self {
        AnyModel::Chat(model)
    }
}

/// `isModelType` (utils/model-operations.ts @ abe508e1b).
pub fn is_model_type(model: &AnyModel, model_type: ModelType) -> bool {
    model.model_type() == model_type
}

/// `getModelType` free-function form (utils/model-operations.ts).
pub fn get_model_type(model: &AnyModel) -> ModelType {
    model.model_type()
}

/// `OpenAICompletionsCompat.maxTokensField`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MaxTokensField {
    #[serde(rename = "max_completion_tokens")]
    MaxCompletionTokens,
    #[serde(rename = "max_tokens")]
    MaxTokens,
}

/// `OpenAICompletionsCompat.thinkingFormat` (11 values).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThinkingFormat {
    /// Uses `reasoning_effort`. Default.
    #[serde(rename = "openai")]
    Openai,
    /// Uses `reasoning: { effort }`.
    #[serde(rename = "openrouter")]
    Openrouter,
    /// Uses `thinking: { type }` plus `reasoning_effort` when supported.
    #[serde(rename = "deepseek")]
    Deepseek,
    /// Uses `reasoning: { enabled }` plus `reasoning_effort` when supported.
    #[serde(rename = "together")]
    Together,
    /// Uses `thinking: { type }`.
    #[serde(rename = "zai")]
    Zai,
    /// Uses top-level `enable_thinking: boolean`.
    #[serde(rename = "qwen")]
    Qwen,
    /// Uses configurable `chat_template_kwargs`.
    #[serde(rename = "chat-template")]
    ChatTemplate,
    /// Uses `chat_template_kwargs.enable_thinking` and `preserve_thinking`.
    #[serde(rename = "qwen-chat-template")]
    QwenChatTemplate,
    /// Uses top-level `thinking: string`.
    #[serde(rename = "string-thinking")]
    StringThinking,
    /// Uses `chat_template_args` (Baseten-specific) plus optional
    /// `reasoning_effort` when the model supports it.
    /// Port of `openai-completions.ts:779` @ `c1019d920` / `4181f66`.
    #[serde(rename = "baseten")]
    Baseten,
    /// Uses `reasoning: { effort }` only when the mapped effort is non-null.
    #[serde(rename = "ant-ling")]
    AntLing,
}

/// `OpenAICompletionsCompat.cacheControlFormat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CacheControlFormat {
    /// Anthropic-style `cache_control` markers on the system prompt, last tool
    /// definition, and last user/assistant/tool-result text content.
    #[serde(rename = "anthropic")]
    Anthropic,
}

/// `OpenAICompletionsCompat.thinkingTokenBudgetField` (b23741269/#8275):
/// the top-level request field carrying the thinking budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThinkingTokenBudgetField {
    #[serde(rename = "thinking_token_budget")]
    ThinkingTokenBudget,
    #[serde(rename = "thinking_budget")]
    ThinkingBudget,
    #[serde(rename = "thinking_budget_tokens")]
    ThinkingBudgetTokens,
}

impl ThinkingTokenBudgetField {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ThinkingTokenBudget => "thinking_token_budget",
            Self::ThinkingBudget => "thinking_budget",
            Self::ThinkingBudgetTokens => "thinking_budget_tokens",
        }
    }
}

/// Merged compat settings for OpenAI-completions / OpenAI-responses /
/// Anthropic-messages / Bedrock APIs (see file header note for why this is one
/// flat struct). All fields optional; absent fields are omitted on the wire,
/// matching upstream objects.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCompat {
    // --- OpenAICompletionsCompat ---
    /// Whether the provider supports the `store` field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_store: Option<bool>,
    /// Whether the provider supports the `developer` role (vs `system`).
    /// Also in OpenAIResponsesCompat.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_developer_role: Option<bool>,
    /// Whether the provider supports `reasoning_effort`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_reasoning_effort: Option<bool>,
    /// Whether the provider supports `stream_options: { include_usage: true }`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_usage_in_streaming: Option<bool>,
    /// Whether streamed responses include `finish_reason`. When false, pi
    /// infers `stop` or `toolUse` when the stream ends. Default: true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_finish_reason: Option<bool>,
    /// Which field to use for max tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens_field: Option<MaxTokensField>,
    /// Whether tool results require the `name` field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_tool_result_name: Option<bool>,
    /// Whether a user message after tool results requires an assistant message
    /// in between.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_assistant_after_tool_result: Option<bool>,
    /// Whether thinking blocks must be converted to text blocks with
    /// `<thinking>` delimiters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_thinking_as_text: Option<bool>,
    /// Whether all replayed assistant messages must include an empty
    /// `reasoning_content` field when reasoning is enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_reasoning_content_on_assistant_messages: Option<bool>,
    /// Format for the reasoning/thinking parameter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_format: Option<ThinkingFormat>,
    /// Kwargs sent as `chat_template_kwargs` when `thinking_format` is
    /// `chat-template`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_template_kwargs: Option<std::collections::BTreeMap<String, ChatTemplateKwargValue>>,
    /// Arguments sent as `chat_template_args` when `thinking_format` is
    /// `baseten` (the Baseten thinking branch itself is T26 scope; the field
    /// is threaded through here so models.json entries round-trip).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_template_args: Option<std::collections::BTreeMap<String, ChatTemplateKwargValue>>,
    /// OpenRouter routing preferences, sent as the `provider` request field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_router_routing: Option<OpenRouterRouting>,
    /// Vercel AI Gateway routing preferences.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vercel_gateway_routing: Option<VercelGatewayRouting>,
    /// Whether z.ai supports top-level `tool_stream: true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zai_tool_stream: Option<bool>,
    /// Whether the provider supports top-level `thinking_token_budget` to cap
    /// reasoning tokens (vLLM). Reasoning and the answer share `max_tokens`
    /// on these endpoints, so without a budget a reasoning-heavy turn can
    /// consume the whole response and emit no answer. Default: false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_thinking_token_budget: Option<bool>,
    /// b23741269 (#8275): the top-level request field carrying the thinking
    /// budget — `thinking_token_budget` (vLLM) / `thinking_budget`
    /// (Qwen-SGLang) / `thinking_budget_tokens` (llama.cpp).
    /// `supports_thinking_token_budget` is the `thinking_token_budget`
    /// alias (field wins when both are set).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_token_budget_field: Option<ThinkingTokenBudgetField>,
    /// 256f63024 (#9004): vLLM scheduler priority, sent as the top-level
    /// `priority` request field when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vllm_priority: Option<f64>,
    /// b8b873b98 (#8941): whether the Responses `max_output_tokens` field is
    /// accepted. Default: true; some Codex-protocol gateways reject it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_max_output_tokens: Option<bool>,
    /// Whether the provider supports OpenAI custom tools with Lark/regex
    /// grammar formats. Also in OpenAIResponsesCompat.
    #[serde(rename = "supportsOpenAIGrammarTools")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_open_ai_grammar_tools: Option<bool>,
    /// Whether the provider supports the `strict` field in tool definitions.
    /// Also in OpenAIResponsesCompat (strict JSON-schema function tools) and
    /// BedrockCompat (Bedrock strict tool schemas).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_strict_mode: Option<bool>,
    /// Cache control convention for prompt caching.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control_format: Option<CacheControlFormat>,
    /// Whether to send session-affinity data from `options.session_id`.
    /// Default: true for OpenRouter endpoints, false otherwise (#9102).
    /// Also in AnthropicMessagesCompat.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub send_session_affinity_headers: Option<bool>,
    /// Session-affinity header format. Also in OpenAIResponsesCompat; the
    /// Anthropic Messages adapter consumes it too (#9102: `openrouter`
    /// selects `x-session-id`, unset keeps `x-session-affinity`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_affinity_format: Option<SessionAffinityFormat>,
    /// Whether the provider supports long prompt cache retention. Also in
    /// OpenAIResponsesCompat and AnthropicMessagesCompat.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_long_cache_retention: Option<bool>,

    // --- OpenAIResponsesCompat only ---
    /// Whether the model supports message-anchored `additional_tools` input
    /// items. Default: false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_additional_tools: Option<bool>,
    /// Whether the model supports client-executed tool search for deferred tools.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_tool_search: Option<bool>,
    /// Whether the model accepts `prompt_cache_options` (OpenAI GPT-5.6+
    /// explicit prompt caching).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_explicit_prompt_cache_mode: Option<bool>,

    // --- AnthropicMessagesCompat only ---
    /// Whether the provider accepts per-tool `eager_input_streaming`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_eager_tool_input_streaming: Option<bool>,
    /// Whether the provider supports Anthropic-style `cache_control` markers
    /// on tool definitions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_cache_control_on_tools: Option<bool>,
    /// Whether the model accepts the Anthropic `temperature` request field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_temperature: Option<bool>,
    /// Whether to force adaptive thinking (`thinking.type: "adaptive"` plus
    /// `output_config.effort`) regardless of the model id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub force_adaptive_thinking: Option<bool>,
    /// Whether to replay empty thinking signatures as `signature: ""` instead
    /// of converting thinking to text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_empty_signature: Option<bool>,
    /// Whether the provider supports Anthropic strict tool schemas.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_strict_tools: Option<bool>,
    /// Whether the exact model transport supports effort-only system messages
    /// and thinking binding controls (types.ts:716, 4e69b0c28). Default:
    /// false; catalog gating rules are V14-09 scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_effort: Option<bool>,
    /// Models Anthropic accepts in `fallbacks` for server-side refusal
    /// fallback, with local pricing metadata for returned fallback
    /// responses (types.ts:718-723, eb1f87fa9). When absent or empty the
    /// `fallbacks` request field must be omitted — Anthropic rejects it for
    /// models with no permitted fallback targets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_fallback_models: Option<Vec<AnthropicAllowedFallbackModel>>,
    /// Whether the model accepts system messages / Kimi-style tool-bearing
    /// system messages after the conversation started (#9548, 9e05370b2 —
    /// replaces the earlier `supportsToolReferences` catalog face, which
    /// left the type surface with the deferred-tool mechanism). Declared
    /// in OpenAI-completions/responses and Anthropic-messages compat;
    /// catalog bake rules are V15-03 data sync, wire semantics V15-06.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_system_messages: Option<bool>,
    /// Whether tool additions can ride system messages mid-conversation
    /// (Kimi K3 family: Moonshot, Fireworks, OpenCode; not GitHub Copilot).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_tool_additions: Option<bool>,
    /// Whether the transport accepts `tool_addition`/`tool_removal` blocks
    /// (first-party Anthropic only; proxies reject them).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_tool_changes: Option<bool>,
}

/// `AnthropicAllowedFallbackModel` (types.ts:307-311): one server-side
/// refusal-fallback target with its local pricing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnthropicAllowedFallbackModel {
    pub provider: String,
    pub model: String,
    pub cost: ModelCost,
}

// ---------------------------------------------------------------------------
// Routing preferences
// ---------------------------------------------------------------------------

/// `OpenRouterRouting.data_collection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DataCollection {
    #[serde(rename = "deny")]
    Deny,
    #[serde(rename = "allow")]
    Allow,
}

/// `OpenRouterRouting.sort` — string or object form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OpenRouterSort {
    Name(String),
    Spec(OpenRouterSortSpec),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenRouterSortSpec {
    /// The sorting metric: "price", "throughput", "latency".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// Partitioning strategy: "model" (default) or "none".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partition: Option<String>,
}

/// `OpenRouterRouting.max_price` — prices per million tokens (USD).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenRouterMaxPrice {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<NumberOrString>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion: Option<NumberOrString>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<NumberOrString>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<NumberOrString>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<NumberOrString>,
}

/// `OpenRouterRouting.preferred_min_throughput` — number or percentile object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OpenRouterThroughput {
    Value(f64),
    Percentiles(OpenRouterPercentiles),
}

/// `OpenRouterRouting.preferred_max_latency` — number or percentile object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OpenRouterLatency {
    Value(f64),
    Percentiles(OpenRouterPercentiles),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenRouterPercentiles {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p50: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p75: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p90: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p99: Option<f64>,
}

/// `OpenRouterRouting` — controls which upstream providers OpenRouter routes
/// requests to. Sent as the `provider` field in the OpenRouter API request
/// body. Field names are snake_case upstream (unlike the rest of the model
/// catalog), so no rename rule is applied here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenRouterRouting {
    /// Whether to allow backup providers to serve requests. Default: true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_fallbacks: Option<bool>,
    /// Filter to providers supporting all parameters in the request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require_parameters: Option<bool>,
    /// Data collection setting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_collection: Option<DataCollection>,
    /// Restrict to ZDR (Zero Data Retention) endpoints.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zdr: Option<bool>,
    /// Restrict to models that allow text distillation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enforce_distillable_text: Option<bool>,
    /// Ordered list of provider names/slugs to try in sequence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
    /// Provider names/slugs to exclusively allow.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<String>>,
    /// Provider names/slugs to skip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore: Option<Vec<String>>,
    /// Quantization levels to filter by.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quantizations: Option<Vec<String>>,
    /// Sorting strategy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<OpenRouterSort>,
    /// Maximum price per million tokens (USD).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_price: Option<OpenRouterMaxPrice>,
    /// Preferred minimum throughput (tokens/second).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_min_throughput: Option<OpenRouterThroughput>,
    /// Preferred maximum latency (seconds).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_max_latency: Option<OpenRouterLatency>,
}

/// `VercelGatewayRouting` — routing preferences for Vercel AI Gateway.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VercelGatewayRouting {
    /// Provider slugs to exclusively use (e.g. ["bedrock", "anthropic"]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<String>>,
    /// Provider slugs to try in order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Model type guards and error results (`utils/model-operations.ts` @
// abe508e1b; V16-06/V16-07 R2.5.2/R2.5.3)
// ---------------------------------------------------------------------------
fn assert_model_type(
    model: &AnyModel,
    expected: ModelType,
) -> Result<&AnyModel, crate::auth::ModelsError> {
    if model.model_type() != expected {
        return Err(crate::auth::ModelsError::new(
            crate::auth::ModelsErrorCode::Provider,
            format!(
                "Model {}/{} is not a {} model",
                model.provider(),
                model.id(),
                expected.as_str()
            ),
        ));
    }
    Ok(model)
}

/// `assertImageModel` (utils/model-operations.ts:20-22): a non-image model
/// reaching image generation is a caller error.
pub fn assert_image_model(model: &AnyModel) -> Result<&ImageModel, crate::auth::ModelsError> {
    assert_model_type(model, ModelType::Image)?;
    match model {
        AnyModel::Image(image) => Ok(image),
        _ => Err(crate::auth::ModelsError::new(
            crate::auth::ModelsErrorCode::Provider,
            "Model is not an image model".to_owned(),
        )),
    }
}

/// `assertClassifierModel` (utils/model-operations.ts:24-26).
pub fn assert_classifier_model(
    model: &AnyModel,
) -> Result<&ClassifierModel, crate::auth::ModelsError> {
    assert_model_type(model, ModelType::Classifier)?;
    match model {
        AnyModel::Classifier(classifier) => Ok(classifier),
        _ => Err(crate::auth::ModelsError::new(
            crate::auth::ModelsErrorCode::Provider,
            "Model is not a classifier model".to_owned(),
        )),
    }
}

/// `assertClassifierInputSupported` (utils/model-operations.ts:44-51):
/// rejects classifier images for models whose catalog entry does not accept
/// image input. APIs that accept the model but cannot send images fail in
/// the transport with their own message.
pub fn assert_classifier_input_supported(
    model: &ClassifierModel,
    context: &ClassifierContext,
) -> Result<(), crate::auth::ModelsError> {
    if context
        .images
        .as_ref()
        .is_some_and(|images| !images.is_empty())
        && !model.input.contains(&InputModality::Image)
    {
        return Err(crate::auth::ModelsError::new(
            crate::auth::ModelsErrorCode::Provider,
            format!(
                "Model {}/{} does not accept image input",
                model.provider, model.id
            ),
        ));
    }
    Ok(())
}

/// `imageErrorResult` (utils/model-operations.ts:28-39): the never-reject
/// failure shape.
pub fn image_error_result(model: &ImageModel, error: &str, aborted: bool) -> AssistantImages {
    AssistantImages {
        // The unified runtime resolves models as `ApiKind`; the legacy
        // `AssistantImages` carries `ImagesApiKind` (same open-string shape).
        api: model.api.as_str().into(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        output: Vec::new(),
        response_id: None,
        usage: None,
        stop_reason: if aborted {
            ImagesStopReason::Aborted
        } else {
            ImagesStopReason::Error
        },
        error_message: Some(error.to_owned()),
        timestamp: now_millis(),
    }
}

/// `classifierErrorResult` (utils/model-operations.ts:41-52).
pub fn classifier_error_result(
    model: &ClassifierModel,
    error: &str,
    aborted: bool,
) -> ClassifierResult {
    ClassifierResult {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        answers: std::collections::BTreeMap::new(),
        usage: None,
        stop_reason: if aborted {
            ClassifierStopReason::Aborted
        } else {
            ClassifierStopReason::Error
        },
        error_message: Some(error.to_owned()),
        timestamp: now_millis(),
    }
}

/// Unix timestamp in milliseconds (`Date.now()` upstream).
fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Tests: serialization shape snapshots (T01 gate G3 substitute — locked type
// contracts verified against hand-checked upstream JSON shapes)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value, json};

    use super::*;

    fn partial() -> AssistantMessage {
        AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![],
            api: ApiKind::from("anthropic-messages"),
            provider: "anthropic".to_owned(),
            model: "claude-x".to_owned(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 0,
            duration_ms: None,
        }
    }

    /// Canonical JSON of `partial()` — pinned literal, mirrors the upstream
    /// AssistantMessage property order (types.ts).
    const PARTIAL_JSON: &str = r#"{"role":"assistant","content":[],"api":"anthropic-messages","provider":"anthropic","model":"claude-x","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0,"total":0.0}},"stopReason":"stop","timestamp":0}"#;

    fn to_json<T: Serialize>(v: &T) -> String {
        serde_json::to_string(v).expect("serialization must succeed")
    }

    #[test]
    fn message_content_null_deserializes_as_empty() {
        // Upstream tolerates `content: null` from untyped callers and old
        // session files (transform-messages.ts:71-73, normalizes to `[]`).
        let assistant: AssistantMessage = serde_json::from_value(json!({
            "role": "assistant", "content": null, "api": "anthropic-messages",
            "provider": "p", "model": "m",
            "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0,
                      "totalTokens": 0,
                      "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0,
                               "cacheWrite": 0.0, "total": 0.0}},
            "stopReason": "stop", "timestamp": 0
        }))
        .expect("assistant with null content");
        assert_eq!(assistant.content, vec![]);

        let tool_result: ToolResultMessage = serde_json::from_value(json!({
            "role": "toolResult", "toolCallId": "c1", "toolName": "bash",
            "content": null, "isError": false, "timestamp": 0
        }))
        .expect("tool result with null content");
        assert_eq!(tool_result.content, vec![]);

        let user: UserMessage = serde_json::from_value(json!({
            "role": "user", "content": null, "timestamp": 0
        }))
        .expect("user with null content");
        assert_eq!(user.content, UserContent::Blocks(vec![]));
    }

    #[test]
    fn stream_event_start_shape() {
        let ev = StreamEvent::Start {
            partial: Arc::new(partial()),
        };
        assert_eq!(
            to_json(&ev),
            format!(r#"{{"type":"start","partial":{PARTIAL_JSON}}}"#)
        );
        let back: StreamEvent = serde_json::from_str(&to_json(&ev)).expect("roundtrip");
        assert_eq!(back, ev);
    }

    #[test]
    fn stream_event_text_delta_shape() {
        let ev = StreamEvent::TextDelta {
            content_index: 1,
            delta: "hi".to_owned(),
            partial: Arc::new(partial()),
        };
        assert_eq!(
            to_json(&ev),
            format!(
                r#"{{"type":"text_delta","contentIndex":1,"delta":"hi","partial":{PARTIAL_JSON}}}"#
            )
        );
    }

    #[test]
    fn stream_event_toolcall_end_carries_tagged_tool_call() {
        let mut arguments = Map::new();
        arguments.insert("cmd".to_owned(), Value::from("ls"));
        let tool_call = ToolCall {
            id: "c1".to_owned(),
            name: "bash".to_owned(),
            arguments,
            thought_signature: Some("sig".to_owned()),
            namespace: None,
        };
        let ev = StreamEvent::ToolCallEnd {
            content_index: 2,
            tool_call,
            partial: Arc::new(partial()),
        };
        // Upstream ToolCall objects always carry their `type: "toolCall"` tag,
        // also standalone inside toolcall_end.
        assert_eq!(
            to_json(&ev),
            format!(
                r#"{{"type":"toolcall_end","contentIndex":2,"toolCall":{{"type":"toolCall","id":"c1","name":"bash","arguments":{{"cmd":"ls"}},"thoughtSignature":"sig"}},"partial":{PARTIAL_JSON}}}"#
            )
        );
        let back: StreamEvent = serde_json::from_str(&to_json(&ev)).expect("roundtrip");
        assert_eq!(back, ev);
    }

    #[test]
    fn provider_thinking_level_round_trip_and_backward_compatibility() {
        // V14-05 FR-A R1 (4e69b0c28): camelCase, absent when None; old
        // records without the field deserialize to None (JSONL backward
        // compatibility).
        let mut message = partial();
        let wire = serde_json::to_value(&message).expect("serialize");
        assert!(
            !wire
                .as_object()
                .expect("object")
                .contains_key("providerThinkingLevel"),
            "None must be omitted"
        );

        message.provider_thinking_level = Some("xhigh".to_owned());
        let wire = serde_json::to_value(&message).expect("serialize");
        assert_eq!(wire["providerThinkingLevel"], json!("xhigh"));

        let legacy = json!({
            "role": "assistant", "content": [], "api": "anthropic-messages",
            "provider": "anthropic", "model": "m",
            "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                      "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0}},
            "stopReason": "stop", "timestamp": 1
        });
        let parsed: AssistantMessage = serde_json::from_value(legacy).expect("deserialize");
        assert_eq!(parsed.provider_thinking_level, None);
    }

    #[test]
    fn tool_call_namespace_round_trip() {
        // v0.84 (R2.1.3): `namespace` is preserved across streaming, proxy and
        // replay; absent namespaces are omitted on the wire (no stray null).
        let call = ToolCall {
            id: "c1".to_owned(),
            name: "search".to_owned(),
            arguments: Map::new(),
            thought_signature: None,
            namespace: Some("dynamic_tools".to_owned()),
        };
        let v: Value = serde_json::from_str(&to_json(&call)).expect("parse");
        assert_eq!(v["namespace"], json!("dynamic_tools"));

        let bare = ToolCall {
            namespace: None,
            ..call.clone()
        };
        let v: Value = serde_json::from_str(&to_json(&bare)).expect("parse");
        assert!(v.get("namespace").is_none(), "absent namespace is omitted");

        let ev = StreamEvent::ToolCallEnd {
            content_index: 0,
            tool_call: call,
            partial: Arc::new(partial()),
        };
        let v: Value = serde_json::from_str(&to_json(&ev)).expect("parse");
        assert_eq!(v["toolCall"]["namespace"], json!("dynamic_tools"));
        let back: StreamEvent = serde_json::from_str(&to_json(&ev)).expect("roundtrip");
        assert_eq!(back, ev);
    }

    #[test]
    fn stream_event_done_and_error_shapes() {
        let done = StreamEvent::Done {
            reason: DoneReason::ToolUse,
            message: partial(),
        };
        assert_eq!(
            to_json(&done),
            format!(r#"{{"type":"done","reason":"toolUse","message":{PARTIAL_JSON}}}"#)
        );

        let mut err_msg = partial();
        err_msg.stop_reason = StopReason::Aborted;
        err_msg.error_message = Some("Operation aborted".to_owned());
        let err = StreamEvent::Error {
            reason: ErrorReason::Aborted,
            error: err_msg,
        };
        let v: Value = serde_json::from_str(&to_json(&err)).expect("parse");
        assert_eq!(v["type"], json!("error"));
        assert_eq!(v["reason"], json!("aborted"));
        assert_eq!(v["error"]["stopReason"], json!("aborted"));
        assert_eq!(v["error"]["errorMessage"], json!("Operation aborted"));
    }

    #[test]
    fn stream_event_variant_type_literals() {
        // Every variant's `type` tag, checked against upstream types.ts.
        let p = || partial();
        let cases: Vec<(StreamEvent, &str)> = vec![
            (
                StreamEvent::Start {
                    partial: Arc::new(p()),
                },
                "start",
            ),
            (
                StreamEvent::TextStart {
                    content_index: 0,
                    partial: Arc::new(p()),
                },
                "text_start",
            ),
            (
                StreamEvent::TextDelta {
                    content_index: 0,
                    delta: String::new(),
                    partial: Arc::new(p()),
                },
                "text_delta",
            ),
            (
                StreamEvent::TextEnd {
                    content_index: 0,
                    content: String::new(),
                    partial: Arc::new(p()),
                },
                "text_end",
            ),
            (
                StreamEvent::ThinkingStart {
                    content_index: 0,
                    partial: Arc::new(p()),
                },
                "thinking_start",
            ),
            (
                StreamEvent::ThinkingDelta {
                    content_index: 0,
                    delta: String::new(),
                    partial: Arc::new(p()),
                },
                "thinking_delta",
            ),
            (
                StreamEvent::ThinkingEnd {
                    content_index: 0,
                    content: String::new(),
                    partial: Arc::new(p()),
                },
                "thinking_end",
            ),
            (
                StreamEvent::ToolCallStart {
                    content_index: 0,
                    partial: Arc::new(p()),
                },
                "toolcall_start",
            ),
            (
                StreamEvent::ToolCallDelta {
                    content_index: 0,
                    delta: String::new(),
                    partial: Arc::new(p()),
                },
                "toolcall_delta",
            ),
            (
                StreamEvent::ToolCallEnd {
                    content_index: 0,
                    tool_call: ToolCall::default(),
                    partial: Arc::new(p()),
                },
                "toolcall_end",
            ),
            (
                StreamEvent::Done {
                    reason: DoneReason::Stop,
                    message: p(),
                },
                "done",
            ),
            (
                StreamEvent::Error {
                    reason: ErrorReason::Error,
                    error: p(),
                },
                "error",
            ),
        ];
        for (ev, expected_type) in cases {
            let v: Value = serde_json::from_str(&to_json(&ev)).expect("parse");
            assert_eq!(v["type"], json!(expected_type));
        }
    }

    #[test]
    fn assistant_message_signature_fields_shape() {
        let mut msg = partial();
        msg.content = vec![
            AssistantContent::Text(TextContent {
                text: "t".to_owned(),
                text_signature: Some("ts".to_owned()),
            }),
            AssistantContent::Thinking(ThinkingContent {
                thinking: "th".to_owned(),
                thinking_signature: Some("ths".to_owned()),
                redacted: Some(true),
            }),
            AssistantContent::ToolCall(ToolCall {
                id: "1".to_owned(),
                name: "n".to_owned(),
                arguments: Map::new(),
                thought_signature: Some("s".to_owned()),
                namespace: None,
            }),
        ];
        msg.usage.cache_write1h = Some(5);
        msg.usage.reasoning = Some(7);
        msg.response_model = Some("resolved-model".to_owned());
        msg.response_id = Some("resp-1".to_owned());

        let v: Value = serde_json::from_str(&to_json(&msg)).expect("parse");
        assert_eq!(
            v["content"],
            json!([
                {"type": "text", "text": "t", "textSignature": "ts"},
                {"type": "thinking", "thinking": "th", "thinkingSignature": "ths", "redacted": true},
                {"type": "toolCall", "id": "1", "name": "n", "arguments": {}, "thoughtSignature": "s"},
            ])
        );
        assert_eq!(v["usage"]["cacheWrite1h"], json!(5));
        assert_eq!(v["usage"]["reasoning"], json!(7));
        assert_eq!(v["responseModel"], json!("resolved-model"));
        assert_eq!(v["responseId"], json!("resp-1"));

        let back: AssistantMessage = serde_json::from_str(&to_json(&msg)).expect("roundtrip");
        assert_eq!(back, msg);
    }

    #[test]
    fn stop_reason_literals() {
        let cases = [
            (StopReason::Pending, "pending"),
            (StopReason::Stop, "stop"),
            (StopReason::Length, "length"),
            (StopReason::ToolUse, "toolUse"),
            (StopReason::Error, "error"),
            (StopReason::Aborted, "aborted"),
            (StopReason::Deferred, "deferred"),
        ];
        for (reason, expected) in cases {
            assert_eq!(to_json(&reason), format!("\"{expected}\""));
            let back: StopReason =
                serde_json::from_str(&format!("\"{expected}\"")).expect("roundtrip");
            assert_eq!(back, reason);
        }
    }

    #[test]
    fn done_reason_literals() {
        // Upstream: Extract<StopReason, "stop" | "length" | "toolUse" |
        // "deferred"> (types.ts, v0.84).
        let cases = [
            (DoneReason::Stop, "stop"),
            (DoneReason::Length, "length"),
            (DoneReason::ToolUse, "toolUse"),
            (DoneReason::Deferred, "deferred"),
        ];
        for (reason, expected) in cases {
            assert_eq!(to_json(&reason), format!("\"{expected}\""));
            let back: DoneReason =
                serde_json::from_str(&format!("\"{expected}\"")).expect("roundtrip");
            assert_eq!(back, reason);
        }
    }

    #[test]
    fn deferred_handle_shape() {
        // Field-for-field against upstream `DeferredHandle` (types.ts, v0.84):
        // provider/modelId/api/id required; expiresAt/pollAfterMs/data optional
        // and omitted when absent; `data` carries arbitrary JSON.
        let full: DeferredHandle = serde_json::from_value(json!({
            "provider": "openai",
            "modelId": "gpt-5.6",
            "api": "openai-responses",
            "id": "resp_123",
            "expiresAt": 1760000000000i64,
            "pollAfterMs": 5000,
            "data": {"rowId": 7, "tags": ["a", null, true]}
        }))
        .expect("deserialize full handle");
        assert_eq!(full.expires_at, Some(1_760_000_000_000));
        assert_eq!(full.poll_after_ms, Some(5_000));
        assert_eq!(
            full.data,
            Some(json!({"rowId": 7, "tags": ["a", null, true]}))
        );
        let v: Value = serde_json::from_str(&to_json(&full)).expect("re-parse");
        assert_eq!(v["modelId"], json!("gpt-5.6"));
        assert_eq!(v["pollAfterMs"], json!(5000));

        let bare = DeferredHandle {
            provider: "openai".to_owned(),
            model_id: "gpt-5.6".to_owned(),
            api: "openai-responses".to_owned(),
            id: "resp_123".to_owned(),
            expires_at: None,
            poll_after_ms: None,
            data: None,
        };
        assert_eq!(
            to_json(&bare),
            r#"{"provider":"openai","modelId":"gpt-5.6","api":"openai-responses","id":"resp_123"}"#
        );
    }

    #[test]
    fn assistant_message_v084_fields_three_state() {
        // v0.84 additions (R2.1.1/R2.1.2): `deferred` sits between `stopReason`
        // and `errorMessage`, `rawStopReason`/`endTurn` after `errorMessage`
        // (upstream property order, types.ts).
        let mut msg = partial();
        msg.stop_reason = StopReason::Deferred;
        msg.deferred = Some(Box::new(DeferredHandle {
            provider: "openai".to_owned(),
            model_id: "gpt-5.6".to_owned(),
            api: "openai-responses".to_owned(),
            id: "resp_123".to_owned(),
            expires_at: None,
            poll_after_ms: None,
            data: None,
        }));
        msg.raw_stop_reason = Some("background".to_owned());
        msg.end_turn = Some(false);

        let v: Value = serde_json::from_str(&to_json(&msg)).expect("parse");
        assert_eq!(v["stopReason"], json!("deferred"));
        assert_eq!(v["deferred"]["id"], json!("resp_123"));
        assert_eq!(v["rawStopReason"], json!("background"));
        assert_eq!(v["endTurn"], json!(false));

        let back: AssistantMessage = serde_json::from_str(&to_json(&msg)).expect("roundtrip");
        assert_eq!(back, msg);

        // Absent state: no stray nulls (already pinned by PARTIAL_JSON, which
        // `partial()` — all-None new fields — serializes to unchanged).
        let v: Value = serde_json::from_str(&to_json(&partial())).expect("parse");
        for key in ["deferred", "rawStopReason", "endTurn"] {
            assert!(v.get(key).is_none(), "absent {key} is omitted");
        }
    }

    #[test]
    fn model_sampling_params_shape() {
        // v0.84 (R2.1.4): `Model.samplingParams` sits between `maxTokens` and
        // `headers`; absent when unset.
        let model_json = json!({
            "id": "m1",
            "name": "M One",
            "api": "openai-completions",
            "provider": "openai",
            "baseUrl": "https://api.example.com",
            "reasoning": false,
            "input": ["text"],
            "cost": {"input": 1.0, "output": 2.0, "cacheRead": 0.5, "cacheWrite": 1.5},
            "contextWindow": 1000000,
            "maxTokens": 64000,
            "samplingParams": {"top_p": 0.9, "min_p": 0.05}
        });
        let model: Model = serde_json::from_value(model_json.clone()).expect("deserialize");
        assert_eq!(
            model.sampling_params.as_ref().expect("present")["top_p"],
            json!(0.9)
        );
        let serialized: Value = serde_json::from_str(&to_json(&model)).expect("re-parse");
        assert_eq!(serialized, model_json);
    }

    #[test]
    fn stream_options_deref_reaches_request_fields() {
        // TS `StreamOptions extends ProviderRequestOptions` is composition +
        // Deref here: inherited fields read/write through `StreamOptions`
        // directly, matching upstream access paths.
        let mut options = StreamOptions {
            temperature: Some(0.5),
            ..StreamOptions::default()
        };
        assert!(options.api_key.is_none());
        options.api_key = Some("sk-test".to_owned());
        options.timeout_ms = Some(30_000);
        assert_eq!(options.request.api_key.as_deref(), Some("sk-test"));
        assert_eq!(options.request.timeout_ms, Some(30_000));

        let request = ProviderRequestOptions {
            max_retries: Some(3),
            ..ProviderRequestOptions::default()
        };
        let options = StreamOptions::from(request);
        assert_eq!(options.max_retries, Some(3));
        assert!(options.temperature.is_none());
    }

    #[test]
    fn stream_options_debug_redacts_credentials() {
        // coding-standards §11.1/§11.2: api_key and header values never appear
        // in Debug output, through both the direct and the delegated impl.
        let mut headers = ProviderHeaders::new();
        headers.insert("authorization".to_owned(), Some("Bearer sekrit".to_owned()));
        let options = StreamOptions {
            request: ProviderRequestOptions {
                api_key: Some("sk-sekrit".to_owned()),
                headers: Some(headers),
                ..ProviderRequestOptions::default()
            },
            ..StreamOptions::default()
        };
        let debug = format!("{options:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("sk-sekrit"));
        assert!(!debug.contains("Bearer sekrit"));
        assert!(debug.contains("authorization"));
    }

    #[test]
    fn user_message_content_string_or_blocks() {
        let text_msg = UserMessage {
            role: UserRole::User,
            content: UserContent::Text("hello".to_owned()),
            timestamp: 1,
        };
        assert_eq!(
            to_json(&text_msg),
            r#"{"role":"user","content":"hello","timestamp":1}"#
        );

        let blocks_msg = UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![
                UserContentBlock::Text(TextContent {
                    text: "a".to_owned(),
                    text_signature: None,
                }),
                UserContentBlock::Image(ImageContent {
                    data: "AAAA".to_owned(),
                    mime_type: "image/png".to_owned(),
                }),
            ]),
            timestamp: 2,
        };
        assert_eq!(
            to_json(&blocks_msg),
            r#"{"role":"user","content":[{"type":"text","text":"a"},{"type":"image","data":"AAAA","mimeType":"image/png"}],"timestamp":2}"#
        );

        // Both forms deserialize through the Message union (role marker
        // disambiguates the untagged variants).
        let m1: Message = serde_json::from_str(&to_json(&text_msg)).expect("text roundtrip");
        assert_eq!(m1.role(), Role::User);
        let m2: Message = serde_json::from_str(&to_json(&blocks_msg)).expect("blocks roundtrip");
        assert_eq!(m2, Message::User(blocks_msg));
    }

    #[test]
    fn tool_result_message_full_shape() {
        let msg = ToolResultMessage {
            role: ToolResultRole::ToolResult,
            tool_call_id: "c1".to_owned(),
            tool_name: "read".to_owned(),
            content: vec![ToolResultContent::Text(TextContent {
                text: "out".to_owned(),
                text_signature: None,
            })],
            details: Some(json!({"truncated": false})),
            usage: None,
            nested_calls: None,
            is_error: false,
            timestamp: 3,
            duration_ms: None,
        };
        let v: Value = serde_json::from_str(&to_json(&msg)).expect("parse");
        assert_eq!(v["role"], json!("toolResult"));
        assert_eq!(v["toolCallId"], json!("c1"));
        assert_eq!(v["toolName"], json!("read"));
        assert_eq!(v["details"], json!({"truncated": false}));
        // addedToolNames removed with the deferred-tool mechanism (#9548).
        assert!(v.get("addedToolNames").is_none());
        assert_eq!(v["isError"], json!(false));
        assert!(
            v.get("usage").is_none(),
            "absent optional fields are omitted"
        );
        let back: ToolResultMessage = serde_json::from_str(&to_json(&msg)).expect("roundtrip");
        assert_eq!(back, msg);
    }

    #[test]
    fn system_message_content_tolerates_null_and_missing() {
        // #9548 (session-manager.ts:397): `message.content == null` covers
        // both explicit null and a missing key; both normalize to "".
        let null_content: SystemMessage = serde_json::from_value(json!({
            "role": "system", "content": null, "timestamp": 1
        }))
        .expect("null content parses");
        assert_eq!(null_content.content, SystemContent::Text(String::new()));
        let missing_content: SystemMessage = serde_json::from_value(json!({
            "role": "system", "timestamp": 1
        }))
        .expect("missing content parses");
        assert_eq!(missing_content.content, SystemContent::Text(String::new()));
        assert_eq!(null_content, missing_content);
    }

    #[test]
    fn tool_constrained_sampling_shapes() {
        let base = |cs: Option<ConstrainedSampling>| Tool {
            name: "n".to_owned(),
            description: "d".to_owned(),
            parameters: json!({"type": "object"}),
            constrained_sampling: cs,
        };

        let json_schema = base(Some(ConstrainedSampling::Config(
            ConstrainedSamplingConfig::JsonSchema {
                strict: ConstrainedSamplingStrict::Require,
            },
        )));
        assert_eq!(
            to_json(&json_schema),
            r#"{"name":"n","description":"d","parameters":{"type":"object"},"constrainedSampling":{"type":"json_schema","strict":"require"}}"#
        );

        let grammar = base(Some(ConstrainedSampling::Config(
            ConstrainedSamplingConfig::Grammar {
                variants: GrammarVariants {
                    openai_lark: Some("x".to_owned()),
                    openai_regex: None,
                },
            },
        )));
        assert_eq!(
            to_json(&grammar),
            r#"{"name":"n","description":"d","parameters":{"type":"object"},"constrainedSampling":{"type":"grammar","variants":{"openai_lark":"x"}}}"#
        );

        let disabled = base(Some(ConstrainedSampling::Disabled(false)));
        assert_eq!(
            to_json(&disabled),
            r#"{"name":"n","description":"d","parameters":{"type":"object"},"constrainedSampling":false}"#
        );

        for tool in [json_schema, grammar, disabled] {
            let back: Tool = serde_json::from_str(&to_json(&tool)).expect("roundtrip");
            assert_eq!(back, tool);
        }
    }

    #[test]
    fn model_full_shape_roundtrip() {
        // Covers: thinkingLevelMap three-state, cost.tiers, headers, compat
        // (incl. the supportsOpenAIGrammarTools acronym rename and
        // chat_template_kwargs $var form).
        let model_json = json!({
            "id": "m1",
            "name": "M One",
            "api": "openai-completions",
            "provider": "openai",
            "baseUrl": "https://api.example.com",
            "reasoning": true,
            "thinkingLevelMap": {"off": null, "high": "high"},
            "input": ["text", "image"],
            "cost": {
                "input": 1.0, "output": 2.0, "cacheRead": 0.5, "cacheWrite": 1.5,
                "tiers": [
                    {"input": 3.0, "output": 4.0, "cacheRead": 0.1, "cacheWrite": 0.2,
                     "inputTokensAbove": 200000}
                ]
            },
            "contextWindow": 1000000,
            "maxTokens": 64000,
            "headers": {"x-a": "b"},
            "compat": {
                "supportsStore": true,
                "supportsOpenAIGrammarTools": true,
                "thinkingFormat": "chat-template",
                "chatTemplateKwargs": {"k": {"$var": "thinking.enabled", "omitWhenOff": true}},
                "openRouterRouting": {
                    "allow_fallbacks": true,
                    "data_collection": "deny",
                    "sort": {"by": "price", "partition": "model"},
                    "max_price": {"prompt": 1.5, "completion": "2"},
                    "preferred_min_throughput": {"p50": 10.0},
                    "preferred_max_latency": 5.0
                },
                "sessionAffinityFormat": "openai-nosession"
            }
        });

        let model: Model = serde_json::from_value(model_json.clone()).expect("deserialize");

        // thinkingLevelMap three-state: absent key (provider default),
        // null (unsupported), string (mapped).
        let map = model.thinking_level_map.as_ref().expect("map present");
        assert_eq!(map.get(&ModelThinkingLevel::Off), Some(&None));
        assert_eq!(
            map.get(&ModelThinkingLevel::High),
            Some(&Some("high".to_owned()))
        );
        assert_eq!(map.get(&ModelThinkingLevel::Low), None);

        let cost = &model.cost;
        assert_eq!(cost.rates.input, 1.0);
        let tiers = cost.tiers.as_ref().expect("tiers present");
        assert_eq!(tiers[0].input_tokens_above, 200_000);
        assert_eq!(tiers[0].rates.output, 4.0);

        let compat = model.compat.as_ref().expect("compat present");
        assert_eq!(compat.supports_open_ai_grammar_tools, Some(true));
        assert_eq!(compat.thinking_format, Some(ThinkingFormat::ChatTemplate));
        assert_eq!(
            compat.session_affinity_format,
            Some(SessionAffinityFormat::OpenaiNosession)
        );
        let kwargs = compat
            .chat_template_kwargs
            .as_ref()
            .expect("kwargs present");
        assert_eq!(
            kwargs.get("k"),
            Some(&ChatTemplateKwargValue::Var(ChatTemplateKwargVar {
                var: ChatTemplateKwargVarKind::ThinkingEnabled,
                omit_when_off: Some(true),
            }))
        );

        // Roundtrip is semantically identical (serde_json::Value map equality
        // is key-order independent).
        let serialized: Value = serde_json::from_str(&to_json(&model)).expect("re-parse");
        assert_eq!(serialized, model_json);
    }

    #[test]
    fn api_kind_known_constants_match_upstream() {
        // KnownApi literal values (requirements §5.1), order as upstream.
        let known = [
            ApiKind::OPENAI_COMPLETIONS,
            ApiKind::MISTRAL_CONVERSATIONS,
            ApiKind::OPENAI_RESPONSES,
            ApiKind::AZURE_OPENAI_RESPONSES,
            ApiKind::OPENAI_CODEX_RESPONSES,
            ApiKind::ANTHROPIC_MESSAGES,
            ApiKind::BEDROCK_CONVERSE_STREAM,
            ApiKind::GOOGLE_GENERATIVE_AI,
            ApiKind::GOOGLE_VERTEX,
            ApiKind::RPI_MESSAGES,
        ];
        assert_eq!(
            known,
            [
                "openai-completions",
                "mistral-conversations",
                "openai-responses",
                "azure-openai-responses",
                "openai-codex-responses",
                "anthropic-messages",
                "bedrock-converse-stream",
                "google-generative-ai",
                "google-vertex",
                "rpi-messages",
            ]
        );
        // Legacy alias: the pre-rename "pi-messages" spelling normalizes to
        // "rpi-messages" on every input path (From and JSON deserialize).
        assert_eq!(ApiKind::from("pi-messages").as_str(), "rpi-messages");
        assert_eq!(to_json(&ApiKind::from("pi-messages")), "\"rpi-messages\"");
        let from_json: ApiKind = serde_json::from_str("\"pi-messages\"").unwrap();
        assert_eq!(from_json.as_str(), "rpi-messages");
        // V16-12 §8-6: the upstream virtual-model API id normalizes to its
        // branded spelling on every input path too.
        assert_eq!(ApiKind::from("pi-virtual").as_str(), "rpi-virtual");
        assert_eq!(to_json(&ApiKind::from("pi-virtual")), "\"rpi-virtual\"");
        let from_json: ApiKind = serde_json::from_str("\"pi-virtual\"").unwrap();
        assert_eq!(from_json.as_str(), "rpi-virtual");
        // Custom API strings stay possible (Api = KnownApi | (string & {})).
        assert_eq!(
            to_json(&ApiKind::from("my-custom-api")),
            "\"my-custom-api\""
        );
    }

    #[test]
    fn diagnostic_shape() {
        let mut details = Map::new();
        details.insert("a".to_owned(), json!(1));
        let diag = AssistantMessageDiagnostic {
            kind: "rewrite".to_owned(),
            timestamp: 1,
            error: Some(DiagnosticErrorInfo {
                name: Some("Error".to_owned()),
                message: "m".to_owned(),
                stack: None,
                code: Some(NumberOrString::String("E".to_owned())),
            }),
            details: Some(details),
        };
        assert_eq!(
            to_json(&diag),
            r#"{"type":"rewrite","timestamp":1,"error":{"name":"Error","message":"m","code":"E"},"details":{"a":1}}"#
        );
    }

    /// #9631 (f5c946480, types.ts:957-996): `inputLimits` parses additively —
    /// camelCase wire names, unknown fields ignored, absent → `None`, and
    /// `None` never serializes (wire format unchanged for models without a
    /// profile).
    #[test]
    fn test_model_input_limits_parse_additive() {
        let model: Model = serde_json::from_value(json!({
            "id": "m", "name": "m", "api": "openai-completions", "provider": "p",
            "baseUrl": "https://example.com", "reasoning": false, "input": ["text", "image"],
            "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
            "contextWindow": 1000, "maxTokens": 100,
            "inputLimits": {
                "maxRequestBytes": 33554432,
                "futureField": true,
                "images": {
                    "maxPerMessage": 20,
                    "futureNested": 1,
                    "resize": {
                        "maxWidth": 2000,
                        "maxHeight": 1568,
                        "maxBytes": 4718592,
                        "jpegQuality": 80
                    }
                }
            }
        }))
        .expect("model");
        assert_eq!(
            model.input_limits,
            Some(ModelInputLimits {
                max_request_bytes: Some(32 * 1024 * 1024),
                images: Some(ModelImageInputLimits {
                    resize: Some(ModelImageResizeOptions {
                        max_width: Some(2000),
                        max_height: Some(1568),
                        max_bytes: Some(4_718_592),
                        jpeg_quality: Some(80),
                    }),
                    max_per_message: Some(20),
                    max_per_request: None,
                }),
            })
        );

        // Absent field → None; None does not serialize (additive wire).
        let plain: Model = serde_json::from_value(json!({
            "id": "m", "name": "m", "api": "openai-completions", "provider": "p",
            "baseUrl": "https://example.com", "reasoning": false, "input": ["text"],
            "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
            "contextWindow": 1000, "maxTokens": 100
        }))
        .expect("model");
        assert_eq!(plain.input_limits, None);
        let wire = serde_json::to_value(&plain).expect("json");
        assert!(wire.get("inputLimits").is_none());

        // Round-trip keeps the profile and the camelCase names.
        let wire = serde_json::to_value(&model).expect("json");
        assert_eq!(
            wire["inputLimits"]["images"]["resize"]["maxWidth"],
            json!(2000)
        );
        let back: Model = serde_json::from_value(wire).expect("model");
        assert_eq!(back.input_limits, model.input_limits);
    }
}
