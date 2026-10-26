//! Port of `packages/ai/src/api/google-vertex.ts` (and the intent of
//! `google-vertex.lazy.ts`, which only defers the module import — rpi has no
//! lazy-loading boundary) @ pi 0.82.1 (2efa728); stream-termination semantics
//! (rawStopReason, provider-stopped error text) updated to 4181f66
//! (23cb385b6, 5a2539a7b); initial-request retry wiring updated to 4181f66
//! (b9d360a2c, `retryGoogleRequest`).
//!
//! Google Vertex AI adapter: API-key (Vertex Express) or ADC authentication,
//! project/location resolution, regional endpoint selection, request
//! construction (system instruction, tools, function-calling mode, thinking
//! config — Gemini 3 `thinkingLevel` vs `thinkingBudget` elsewhere), SSE
//! stream decoding into [`StreamEvent`]s, thought-signature retention, usage
//! mapping, and `stream_simple` reasoning mapping.
//!
//! The upstream adapter delegates transport to the `@google/genai` SDK in
//! `vertexai` mode; like W1's `google_generative_ai.rs` (D-023) this port
//! drives reqwest directly with the wire shape reverse-engineered from the
//! pinned SDK (`external/pi/node_modules/@google/genai` 1.52.0) — see D-024:
//!
//! - URL (`ApiClient` constructor + `getRequestUrlInternal` +
//!   `shouldPrependVertexProjectPath` + `tModel` + `generateContentStreamInternal`):
//!   `POST {base}[/{apiVersion}][/projects/{project}/locations/{location}]/
//!   {tModel}:streamGenerateContent?alt=sse` where `tModel` maps a plain id
//!   to `publishers/google/models/{id}` (ids already starting with
//!   `publishers/`/`projects/`/`models/` pass through; `owner/model` becomes
//!   `publishers/{owner}/models/{model}`). The base is:
//!   - a custom `model.baseUrl` when [`resolve_custom_base_url`] accepts it,
//!     with `apiVersion` appended unless the URL already contains a `vN` /
//!     `vNbetaM` path segment (`baseUrlIncludesApiVersion`), and NO
//!     project/location prefix (`baseUrlResourceScope: COLLECTION`);
//!   - `https://aiplatform.googleapis.com` for API-key mode (Vertex Express)
//!     or ADC with `location == "global"`;
//!   - `https://aiplatform.{location}.rep.googleapis.com` for the
//!     multi-regional locations `us` / `eu` (`MULTI_REGIONAL_LOCATIONS`);
//!   - `https://{location}-aiplatform.googleapis.com` otherwise, with the
//!     project/location prefix. `apiVersion` is pi's pinned `v1`.
//! - A `model.baseUrl` containing `{location}` is DISCARDED WHOLE (no
//!   template substitution) and falls back to the SDK default endpoint —
//!   counter-intuitive but faithful to `resolveCustomBaseUrl`
//!   (google-vertex.ts:391-397).
//! - Auth headers (`NodeAuth`): `x-goog-api-key` in API-key mode,
//!   `authorization: Bearer <ADC access token>` otherwise (see
//!   [`crate::api::google_adc`]); both sit at the base of the header merge
//!   chain so user headers win (SDK skip-if-present semantics).
//! - Body (`generateContentParametersToVertex`): `{contents, systemInstruction?,
//!   tools?, toolConfig?, generationConfig?}`; `temperature` /
//!   `maxOutputTokens` / `thinkingConfig` land in `generationConfig` (omitted
//!   when empty, same as D-023), `systemInstruction` (a string on pi's side)
//!   becomes a `Content` with `role: "user"`. For every field pi sets,
//!   `partToVertex` / `toolToVertex` / `functionDeclarationToVertex` pass
//!   values through verbatim.
//! - The SDK's `user-agent` / `x-goog-api-client` telemetry headers are not
//!   sent. Retries go through
//!   [`crate::api::google_shared::retry_google_request`] (b9d360a2c): opt-in
//!   via `StreamOptions::max_retries`, unset means no retries.
//!
//! Other intentional differences:
//! - `on_payload` still receives the SDK-level params shape
//!   `{model, contents, config}` (as upstream); the wire conversion happens
//!   after the hook, mirroring the SDK pipeline.
//! - SSE events are decoded with the shared [`crate::api::sse::SseDecoder`]
//!   and strict `serde_json` (SDK: delimiter splitter + `JSON.parse`);
//!   parse-failure wording differs.
//! - `stream_simple` performs NO API-key preflight: a missing key is the
//!   legitimate ADC path (unlike the Gemini API adapter, which errors
//!   eagerly).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::api::google_adc::{AdcEndpoints, resolve_access_token};
use crate::api::google_generative_ai::{GoogleThinking, GoogleToolChoice};
use crate::api::google_shared::{
    convert_messages, convert_tools, get_disabled_google_thinking_config, is_thinking_part,
    map_stop_reason, resolve_google_function_calling_mode, resolve_google_thinking_level,
    retain_thought_signature, retry_google_request, supports_google_strict_tool_sampling,
    to_google_thinking_level, uses_google_thinking_level,
};
use crate::api::simple_options::build_base_options;
use crate::api::sse::{ServerSentEvent, SseDecoder};
use crate::api::stream_cancel::{StreamNext, next_chunk_or_cancelled};
use crate::models::{ProviderStreams, clamp_thinking_level};
use crate::types::{
    AssistantContent, AssistantMessage, DoneReason, ErrorReason, Model, ProviderEnv,
    ProviderResponse, SimpleStreamOptions, StopReason, StreamEvent, StreamOptions, ThinkingBudgets,
    ThinkingLevel, Tool, ToolCall, TranscriptContext, Usage,
};
use crate::utils::cost::calculate_cost;
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::headers::{
    headers_to_record, merge_headers_chain, model_headers, provider_headers_to_header_map,
    rpi_user_agent_headers,
};
use crate::utils::provider_retry::ProviderErrorInfo;
use crate::utils::sanitize_unicode::sanitize_surrogates;

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// `GoogleVertexOptions` — `StreamOptions` plus Vertex-specific extras.
#[derive(Debug, Clone, Default)]
pub struct GoogleVertexOptions {
    pub stream: StreamOptions,
    pub tool_choice: Option<GoogleToolChoice>,
    pub thinking: Option<GoogleThinking>,
    pub project: Option<String>,
    pub location: Option<String>,
    /// ADC endpoint overrides (test seam; production uses
    /// [`AdcEndpoints::default`]).
    #[doc(hidden)]
    pub adc_endpoints: Option<AdcEndpoints>,
}

// ---------------------------------------------------------------------------
// Auth / project / location / base URL resolution
// ---------------------------------------------------------------------------

/// `API_VERSION` (google-vertex.ts): pi pins `v1` for the SDK client.
const API_VERSION: &str = "v1";

/// `GCP_VERTEX_CREDENTIALS_MARKER` (google-vertex.ts): the auth-store marker
/// meaning "use ADC".
pub const GCP_VERTEX_CREDENTIALS_MARKER: &str = "gcp-vertex-credentials";

/// `isPlaceholderApiKey` (`/^<[^>]+>$/`): `<...>` placeholders are not real keys.
pub fn is_placeholder_api_key(api_key: &str) -> bool {
    let Some(inner) = api_key.strip_prefix('<').and_then(|s| s.strip_suffix('>')) else {
        return false;
    };
    !inner.is_empty() && !inner.contains('>')
}

/// `resolveApiKey`: trimmed; empty, the ADC marker, and `<...>` placeholders
/// all fall back to ADC.
pub fn resolve_api_key(api_key: Option<&str>) -> Option<String> {
    let api_key = api_key?.trim();
    if api_key.is_empty()
        || api_key == GCP_VERTEX_CREDENTIALS_MARKER
        || is_placeholder_api_key(api_key)
    {
        return None;
    }
    Some(api_key.to_owned())
}

/// `resolveProject`: `options.project` → `GOOGLE_CLOUD_PROJECT` →
/// `GCLOUD_PROJECT` (empty strings are falsy upstream and fall through).
pub fn resolve_project(project: Option<&str>, env: Option<&ProviderEnv>) -> Result<String, String> {
    let project = project
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| crate::utils::provider_env::get_provider_env_value("GOOGLE_CLOUD_PROJECT", env))
        .or_else(|| crate::utils::provider_env::get_provider_env_value("GCLOUD_PROJECT", env));
    project.ok_or_else(|| {
        "Vertex AI requires a project ID. Set GOOGLE_CLOUD_PROJECT/GCLOUD_PROJECT or pass project in options.".to_owned()
    })
}

/// `resolveLocation`: `options.location` → `GOOGLE_CLOUD_LOCATION`.
pub fn resolve_location(
    location: Option<&str>,
    env: Option<&ProviderEnv>,
) -> Result<String, String> {
    let location = location
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            crate::utils::provider_env::get_provider_env_value("GOOGLE_CLOUD_LOCATION", env)
        });
    location.ok_or_else(|| {
        "Vertex AI requires a location. Set GOOGLE_CLOUD_LOCATION or pass location in options."
            .to_owned()
    })
}

/// `resolveCustomBaseUrl`: trimmed; empty and `{location}`-templated base
/// URLs are DISCARDED WHOLE (no template substitution) so the SDK default
/// endpoint applies (google-vertex.ts:391-397).
pub fn resolve_custom_base_url(base_url: &str) -> Option<String> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() || trimmed.contains("{location}") {
        return None;
    }
    Some(trimmed.to_owned())
}

/// `baseUrlIncludesApiVersion`: any path segment matching `v\d+(beta\d*)?`;
/// on unparseable URLs a raw-string scan with the same shape
/// (`/(?:^|\/)v\d+(?:beta\d*)?(?:\/|$)/`).
pub fn base_url_includes_api_version(base_url: &str) -> bool {
    fn is_version_segment(segment: &str) -> bool {
        let digits = segment.strip_prefix('v').unwrap_or(segment);
        if digits.is_empty() || segment.len() == digits.len() {
            return false;
        }
        let (num, beta) = match digits.find("beta") {
            Some(pos) => (&digits[..pos], &digits[pos + 4..]),
            None => (digits, ""),
        };
        !num.is_empty()
            && num.chars().all(|c| c.is_ascii_digit())
            && beta.chars().all(|c| c.is_ascii_digit())
    }
    if let Ok(url) = url::Url::parse(base_url) {
        return url
            .path_segments()
            .is_some_and(|mut segments| segments.any(is_version_segment));
    }
    base_url.split('/').any(is_version_segment)
}

// ---------------------------------------------------------------------------
// Thinking configuration (#9455: the vertex-local disabled config and the
// per-family collapse table moved to google-shared / map metadata; only the
// budget table remains adapter-local)
// ---------------------------------------------------------------------------

/// `getGoogleBudget` (vertex): `options.thinkingBudgets` wins per level;
/// otherwise the model-family table — unlike the Gemini API adapter there is
/// no `2.5-flash-lite` row upstream; -1 (dynamic) for everything else.
fn get_google_budget(
    model: &Model,
    effort: ThinkingLevel,
    custom_budgets: Option<&ThinkingBudgets>,
) -> i64 {
    if let Some(budgets) = custom_budgets {
        let custom = match effort {
            ThinkingLevel::Minimal => budgets.minimal,
            ThinkingLevel::Low => budgets.low,
            ThinkingLevel::Medium => budgets.medium,
            ThinkingLevel::High | ThinkingLevel::Xhigh | ThinkingLevel::Max => budgets.high,
        };
        if let Some(custom) = custom {
            return i64::from(custom);
        }
    }

    let (minimal, low, medium, high) = if model.id.contains("2.5-pro") {
        (128, 2048, 8192, 32768)
    } else if model.id.contains("2.5-flash") {
        (128, 2048, 8192, 24576)
    } else {
        return -1;
    };
    match effort {
        ThinkingLevel::Minimal => minimal,
        ThinkingLevel::Low => low,
        ThinkingLevel::Medium => medium,
        ThinkingLevel::High | ThinkingLevel::Xhigh | ThinkingLevel::Max => high,
    }
}

// ---------------------------------------------------------------------------
// Request construction
// ---------------------------------------------------------------------------

/// `buildParams`: the SDK-level `GenerateContentParameters` shape
/// (`{model, contents, config}`); the wire conversion happens in
/// [`params_to_wire`] after the `on_payload` hook, mirroring the SDK
/// pipeline. Identical to the Gemini API adapter's params construction
/// (google-vertex.ts `buildParams` === google-generative-ai.ts's).
fn build_params(
    model: &Model,
    context: &TranscriptContext,
    options: &GoogleVertexOptions,
) -> Result<Value, String> {
    // Collapse at entry (#9548, google-vertex.ts:30-31: same as the Gemini
    // adapter).
    let context = &crate::utils::transcript::collapse_system_messages(context);
    let contents = convert_messages(model, context);

    let mut config = Map::new();
    if let Some(temperature) = options.stream.temperature {
        config.insert("temperature".to_owned(), json!(temperature));
    }
    if let Some(max_tokens) = options.stream.max_tokens {
        config.insert("maxOutputTokens".to_owned(), json!(max_tokens));
    }
    let initial_system_text =
        crate::utils::transcript::get_initial_system_message(&context.messages)
            .map(crate::utils::text::get_system_message_text)
            .unwrap_or_default();
    if !initial_system_text.is_empty() {
        config.insert(
            "systemInstruction".to_owned(),
            json!(sanitize_surrogates(&initial_system_text)),
        );
    }
    let current_tools = crate::utils::transcript::get_current_tools(&context.messages);
    let tools: &[Tool] = &current_tools;
    if !tools.is_empty() {
        if let Some(converted) = convert_tools(
            tools,
            false,
            supports_google_strict_tool_sampling(&model.id),
        )? {
            config.insert("tools".to_owned(), converted);
        }
        let function_calling_mode = resolve_google_function_calling_mode(
            tools,
            options.tool_choice.map(GoogleToolChoice::as_str),
            supports_google_strict_tool_sampling(&model.id),
        )?;
        if let Some(mode) = function_calling_mode {
            config.insert(
                "toolConfig".to_owned(),
                json!({"functionCallingConfig": {"mode": mode}}),
            );
        }
    }

    if let Some(thinking) = &options.thinking {
        if thinking.enabled && model.reasoning {
            let mut thinking_config = json!({"includeThoughts": true});
            if let Some(level) = thinking.level {
                thinking_config["thinkingLevel"] = json!(level.as_str());
            } else if let Some(budget_tokens) = thinking.budget_tokens {
                thinking_config["thinkingBudget"] = json!(budget_tokens);
            }
            config.insert("thinkingConfig".to_owned(), thinking_config);
        } else if model.reasoning && !thinking.enabled {
            config.insert(
                "thinkingConfig".to_owned(),
                get_disabled_google_thinking_config(model),
            );
        }
    }

    if options
        .stream
        .signal
        .as_ref()
        .is_some_and(|signal| signal.is_cancelled())
    {
        return Err("Request aborted".to_owned());
    }

    Ok(json!({
        "model": model.id,
        "contents": contents,
        "config": Value::Object(config),
    }))
}

/// `tModel` (Vertex path): ids already starting with `publishers/` /
/// `projects/` / `models/` pass through; `owner/model` becomes
/// `publishers/{owner}/models/{model}`; plain ids get the
/// `publishers/google/models/` prefix.
fn t_model_vertex(model: &str) -> String {
    if model.starts_with("publishers/")
        || model.starts_with("projects/")
        || model.starts_with("models/")
    {
        model.to_owned()
    } else if let Some((owner, name)) = model.split_once('/') {
        // JS `split('/', 2)`: keeps only the first two segments.
        let name = name.split('/').next().unwrap_or(name);
        format!("publishers/{owner}/models/{name}")
    } else {
        format!("publishers/google/models/{model}")
    }
}

/// `generateContentParametersToVertex` (for the fields pi sets): splits the
/// SDK-level params into the URL model path and the wire body. `temperature`
/// / `maxOutputTokens` / `thinkingConfig` land in `generationConfig` (omitted
/// when empty, matching the D-023 line); `systemInstruction` becomes a
/// `Content` with `role: "user"` (`tContent(string)`); `tools` / `toolConfig`
/// pass through at the top level.
fn params_to_wire(params: &Value) -> (String, Value) {
    let model = params
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let config = params.get("config").cloned().unwrap_or(Value::Null);

    let mut body = Map::new();
    body.insert(
        "contents".to_owned(),
        params.get("contents").cloned().unwrap_or_else(|| json!([])),
    );

    let mut generation_config = Map::new();
    for key in ["temperature", "maxOutputTokens", "thinkingConfig"] {
        if let Some(value) = config.get(key) {
            generation_config.insert(key.to_owned(), value.clone());
        }
    }
    if !generation_config.is_empty() {
        body.insert(
            "generationConfig".to_owned(),
            Value::Object(generation_config),
        );
    }
    if let Some(system_instruction) = config.get("systemInstruction") {
        body.insert(
            "systemInstruction".to_owned(),
            json!({
                "parts": [{"text": system_instruction}],
                "role": "user",
            }),
        );
    }
    if let Some(tools) = config.get("tools") {
        body.insert("tools".to_owned(), tools.clone());
    }
    if let Some(tool_config) = config.get("toolConfig") {
        body.insert("toolConfig".to_owned(), tool_config.clone());
    }

    (t_model_vertex(model), Value::Object(body))
}

/// The resolved Vertex endpoint (`ApiClient` constructor +
/// `getRequestUrlInternal` + `shouldPrependVertexProjectPath`).
struct VertexEndpoint {
    base_url: String,
    api_version: String,
    prepend_project_location: bool,
}

/// `MULTI_REGIONAL_LOCATIONS` (SDK): these locations use the
/// `aiplatform.{location}.rep.googleapis.com` host shape.
const MULTI_REGIONAL_LOCATIONS: [&str; 2] = ["us", "eu"];

/// Endpoint selection, mirroring the SDK `ApiClient` constructor's vertexai
/// branch combined with pi's `buildHttpOptions`:
/// - an accepted custom base URL wins, drops the project/location prefix
///   (`baseUrlResourceScope: COLLECTION`) and suppresses the API version when
///   the URL already carries one (`httpOptions.apiVersion = ""`);
/// - API-key mode (Vertex Express) and ADC with `location == "global"` use
///   the global host without prefix;
/// - multi-regional locations use the `aiplatform.{location}.rep.` host;
/// - everything else uses `{location}-aiplatform.googleapis.com` with the
///   project/location prefix.
fn resolve_endpoint(
    model: &Model,
    api_key_mode: bool,
    project: Option<&str>,
    location: Option<&str>,
) -> VertexEndpoint {
    if let Some(base_url) = resolve_custom_base_url(&model.base_url) {
        return VertexEndpoint {
            api_version: if base_url_includes_api_version(&base_url) {
                String::new()
            } else {
                API_VERSION.to_owned()
            },
            base_url,
            prepend_project_location: false,
        };
    }
    if api_key_mode {
        return VertexEndpoint {
            base_url: "https://aiplatform.googleapis.com".to_owned(),
            api_version: API_VERSION.to_owned(),
            prepend_project_location: false,
        };
    }
    // invariant: ADC mode resolves project/location before endpoint
    // resolution (upstream createClient argument evaluation).
    let location = location.unwrap_or_default();
    let base_url = if location == "global" {
        "https://aiplatform.googleapis.com".to_owned()
    } else if MULTI_REGIONAL_LOCATIONS.contains(&location) {
        format!("https://aiplatform.{location}.rep.googleapis.com")
    } else {
        format!("https://{location}-aiplatform.googleapis.com")
    };
    VertexEndpoint {
        base_url,
        api_version: API_VERSION.to_owned(),
        prepend_project_location: project.is_some(),
    }
}

/// Full request URL assembly (`constructUrl` + the `requestStream` path
/// template): `{base}[/{apiVersion}][/projects/{project}/locations/{location}]
/// /{modelPath}:streamGenerateContent?alt=sse`. `#[doc(hidden)]`: exposed for
/// contract tests covering the endpoint-selection matrix.
#[doc(hidden)]
pub fn resolve_request_url(
    model: &Model,
    api_key_mode: bool,
    project: Option<&str>,
    location: Option<&str>,
    model_path: &str,
) -> String {
    let endpoint = resolve_endpoint(model, api_key_mode, project, location);
    let mut url = endpoint.base_url.trim_end_matches('/').to_owned();
    if !endpoint.api_version.is_empty() {
        url.push('/');
        url.push_str(&endpoint.api_version);
    }
    if endpoint.prepend_project_location {
        // invariant: prepend is only selected in ADC mode, where
        // project/location were resolved before URL construction
        url.push_str(&format!(
            "/projects/{}/locations/{}",
            project.unwrap_or_default(),
            location.unwrap_or_default()
        ));
    }
    url.push_str(&format!("/{model_path}:streamGenerateContent?alt=sse"));
    url
}

/// Request headers: the auth header (`x-goog-api-key` or `authorization`)
/// sits in the base position so user headers (model/option) win, mirroring
/// the SDK's skip-if-present semantics (`NodeAuth.addKeyHeader` /
/// `addGoogleAuthHeaders`).
fn build_request_headers(
    model: &Model,
    auth_header: (&str, String),
    options_headers: Option<&crate::types::ProviderHeaders>,
) -> crate::types::ProviderHeaders {
    let mut base = crate::types::ProviderHeaders::new();
    base.insert(auth_header.0.to_owned(), Some(auth_header.1));
    merge_headers_chain(&[
        rpi_user_agent_headers(),
        Some(base),
        model_headers(model),
        options_headers.cloned(),
    ])
}

// ---------------------------------------------------------------------------
// Stream processing (same chunk protocol as the Gemini API adapter;
// upstream duplicates this loop across the two files)
// ---------------------------------------------------------------------------

/// Counter for generating unique tool call IDs (upstream module-level
/// `toolCallCounter`).
static TOOL_CALL_COUNTER: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn json_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_f64().map(|float| float as u64))
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

/// The currently open streamed block (text or thinking). Tool calls complete
/// in one shot upstream (no function call streaming), so they need no state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CurrentBlock {
    Text(usize),
    Thinking(usize),
}

/// Consumes `GenerateContentResponse` chunks and drives the [`StreamEvent`]
/// protocol, accumulating the final assistant message. Mirrors the
/// `for await (const chunk of googleStream)` loop in google-vertex.ts.
struct StreamProcessor<'a> {
    output: &'a mut AssistantMessage,
    model: &'a Model,
    current_block: Option<CurrentBlock>,
}

impl<'a> StreamProcessor<'a> {
    fn new(output: &'a mut AssistantMessage, model: &'a Model) -> Self {
        Self {
            output,
            model,
            current_block: None,
        }
    }

    fn block_index(&self) -> usize {
        self.output.content.len() - 1
    }

    /// Ends the current text/thinking block (upstream's shared tail used both
    /// on block-type switches and at end of stream).
    fn end_current_block(&mut self, events: &AssistantMessageEventStream) {
        match self.current_block.take() {
            Some(CurrentBlock::Text(content_index)) => {
                let content = match self.output.content.get(content_index) {
                    Some(AssistantContent::Text(text)) => text.text.clone(),
                    _ => String::new(),
                };
                events.push(StreamEvent::TextEnd {
                    content_index,
                    content,
                    partial: Arc::new(self.output.clone()),
                });
            }
            Some(CurrentBlock::Thinking(content_index)) => {
                let content = match self.output.content.get(content_index) {
                    Some(AssistantContent::Thinking(thinking)) => thinking.thinking.clone(),
                    _ => String::new(),
                };
                events.push(StreamEvent::ThinkingEnd {
                    content_index,
                    content,
                    partial: Arc::new(self.output.clone()),
                });
            }
            None => {}
        }
    }

    fn handle_text_part(&mut self, part: &Value, events: &AssistantMessageEventStream) {
        let is_thinking = is_thinking_part(part.get("thought").and_then(Value::as_bool));
        let kind_matches = matches!(
            (is_thinking, self.current_block),
            (true, Some(CurrentBlock::Thinking(_))) | (false, Some(CurrentBlock::Text(_)))
        );
        if !kind_matches {
            self.end_current_block(events);
            if is_thinking {
                self.output.content.push(AssistantContent::Thinking(
                    crate::types::ThinkingContent {
                        thinking: String::new(),
                        thinking_signature: None,
                        redacted: None,
                    },
                ));
                self.current_block = Some(CurrentBlock::Thinking(self.block_index()));
                events.push(StreamEvent::ThinkingStart {
                    content_index: self.block_index(),
                    partial: Arc::new(self.output.clone()),
                });
            } else {
                self.output
                    .content
                    .push(AssistantContent::Text(crate::types::TextContent {
                        text: String::new(),
                        text_signature: None,
                    }));
                self.current_block = Some(CurrentBlock::Text(self.block_index()));
                events.push(StreamEvent::TextStart {
                    content_index: self.block_index(),
                    partial: Arc::new(self.output.clone()),
                });
            }
        }

        let text = part.get("text").and_then(Value::as_str).unwrap_or("");
        let incoming_signature = part.get("thoughtSignature").and_then(Value::as_str);
        let content_index = self.block_index();
        match self.output.content.get_mut(content_index) {
            Some(AssistantContent::Thinking(thinking)) => {
                thinking.thinking.push_str(text);
                thinking.thinking_signature = retain_thought_signature(
                    thinking.thinking_signature.as_deref(),
                    incoming_signature,
                );
                events.push(StreamEvent::ThinkingDelta {
                    content_index,
                    delta: text.to_owned(),
                    partial: Arc::new(self.output.clone()),
                });
            }
            Some(AssistantContent::Text(block)) => {
                block.text.push_str(text);
                block.text_signature =
                    retain_thought_signature(block.text_signature.as_deref(), incoming_signature);
                events.push(StreamEvent::TextDelta {
                    content_index,
                    delta: text.to_owned(),
                    partial: Arc::new(self.output.clone()),
                });
            }
            _ => {}
        }
    }

    fn handle_function_call_part(
        &mut self,
        part: &Value,
        function_call: &Value,
        events: &AssistantMessageEventStream,
    ) {
        self.end_current_block(events);

        // Generate a unique ID if not provided or if it's a duplicate.
        let provided_id = function_call.get("id").and_then(Value::as_str);
        let needs_new_id = provided_id.is_none_or(|id| {
            self.output
                .content
                .iter()
                .any(|block| matches!(block, AssistantContent::ToolCall(call) if call.id == id))
        });
        let name = function_call
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("");
        let tool_call_id = if needs_new_id {
            let counter = TOOL_CALL_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
            format!("{name}_{}_{counter}", now_ms())
        } else {
            // invariant: needs_new_id is false implies a provided id
            provided_id.unwrap_or_default().to_owned()
        };

        let arguments = function_call
            .get("args")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let tool_call = ToolCall {
            id: tool_call_id,
            name: name.to_owned(),
            arguments: arguments.clone(),
            thought_signature: part
                .get("thoughtSignature")
                .and_then(Value::as_str)
                .filter(|signature| !signature.is_empty())
                .map(str::to_owned),
            namespace: None,
        };

        self.output
            .content
            .push(AssistantContent::ToolCall(tool_call.clone()));
        let content_index = self.block_index();
        events.push(StreamEvent::ToolCallStart {
            content_index,
            partial: Arc::new(self.output.clone()),
        });
        events.push(StreamEvent::ToolCallDelta {
            content_index,
            delta: serde_json::to_string(&arguments).unwrap_or_else(|_| "{}".to_owned()),
            partial: Arc::new(self.output.clone()),
        });
        events.push(StreamEvent::ToolCallEnd {
            content_index,
            tool_call,
            partial: Arc::new(self.output.clone()),
        });
    }

    /// Per-chunk body (upstream's `for await (const chunk of googleStream)`).
    fn handle_chunk(
        &mut self,
        chunk: &Value,
        events: &AssistantMessageEventStream,
    ) -> Result<(), String> {
        // GenerateContentResponse.responseId is output-only; keep the first
        // one from the stream (upstream `output.responseId ||=`).
        if self.output.response_id.is_none()
            && let Some(response_id) = chunk.get("responseId").and_then(Value::as_str)
        {
            self.output.response_id = Some(response_id.to_owned());
        }

        let candidate = chunk
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|candidates| candidates.first());
        if let Some(parts) = candidate
            .and_then(|candidate| candidate.get("content"))
            .and_then(|content| content.get("parts"))
            .and_then(Value::as_array)
        {
            for part in parts {
                if part.get("text").is_some() {
                    self.handle_text_part(part, events);
                }
                if let Some(function_call) = part.get("functionCall") {
                    self.handle_function_call_part(part, function_call, events);
                }
            }
        }

        if let Some(finish_reason) = candidate
            .and_then(|candidate| candidate.get("finishReason"))
            .and_then(Value::as_str)
        {
            // 23cb385b6: preserve the raw provider reason before mapping.
            self.output.raw_stop_reason = Some(finish_reason.to_owned());
            self.output.stop_reason = map_stop_reason(finish_reason)?;
            if self
                .output
                .content
                .iter()
                .any(|block| matches!(block, AssistantContent::ToolCall(_)))
            {
                self.output.stop_reason = StopReason::ToolUse;
            }
        }

        if let Some(usage_metadata) = chunk.get("usageMetadata") {
            let prompt = json_u64(&usage_metadata["promptTokenCount"]).unwrap_or(0);
            let cached = json_u64(&usage_metadata["cachedContentTokenCount"]).unwrap_or(0);
            let candidates = json_u64(&usage_metadata["candidatesTokenCount"]).unwrap_or(0);
            let thoughts = json_u64(&usage_metadata["thoughtsTokenCount"]).unwrap_or(0);
            // JS `prompt - cached` can go negative on anomalous counts; Usage
            // is u64 in rpi, so saturate.
            self.output.usage.input = prompt.saturating_sub(cached);
            self.output.usage.output = candidates + thoughts;
            self.output.usage.cache_read = cached;
            self.output.usage.cache_write = 0;
            self.output.usage.reasoning = Some(thoughts);
            self.output.usage.total_tokens =
                json_u64(&usage_metadata["totalTokenCount"]).unwrap_or(0);
            calculate_cost(self.model, &mut self.output.usage);
        }

        Ok(())
    }

    fn handle_sse(
        &mut self,
        sse: &ServerSentEvent,
        events: &AssistantMessageEventStream,
    ) -> Result<(), String> {
        // P2-4: tolerate keep-alive/dispatch events with no `data` payload
        // (empty string can never decode to a valid chunk — skip instead of
        // hard-failing the stream).
        if sse.data.trim().is_empty() {
            return Ok(());
        }
        let chunk: Value = serde_json::from_str(&sse.data).map_err(|error| {
            format!(
                "Could not parse Google SSE event: {error}; data={}; raw={}",
                sse.data,
                sse.raw.join("\\n")
            )
        })?;
        self.handle_chunk(&chunk, events)
    }
}

/// The SDK's in-stream error check (`processStreamResponse`): each raw
/// network chunk is probed as a JSON error object; a 4xx/5xx `code` aborts
/// the stream with `got status: {status}. {json}`.
fn check_raw_chunk_error(bytes: &[u8]) -> Result<(), String> {
    let Ok(chunk_json) = serde_json::from_slice::<Value>(bytes) else {
        return Ok(());
    };
    let Some(error) = chunk_json.get("error") else {
        return Ok(());
    };
    let Some(code) = error.get("code").and_then(Value::as_i64) else {
        return Ok(());
    };
    if !(400..600).contains(&code) {
        return Ok(());
    }
    let status = error
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("undefined");
    let serialized = serde_json::to_string(&chunk_json).unwrap_or_else(|_| "{}".to_owned());
    Err(format!("got status: {status}. {serialized}"))
}

/// The streaming body: everything that runs inside upstream's async IIFE.
/// Errors return the upstream `Error.message`; `output` carries the partial
/// message either way.
async fn run(
    model: &Model,
    context: &TranscriptContext,
    options: &GoogleVertexOptions,
    output: &mut AssistantMessage,
    events: &AssistantMessageEventStream,
) -> Result<DoneReason, String> {
    // 027a58479: reject a custom fetch instead of silently bypassing it
    // (upstream `options.fetch !== globalThis.fetch`; rpi has no ambient
    // global fetch — the default reqwest transport is its analogue — so any
    // injected fetch is non-default and rejected). Checked before auth
    // resolution, matching the top of upstream's try block.
    if options.stream.fetch.is_some() {
        return Err("Custom fetch is not supported by the Google Vertex adapter".to_owned());
    }
    // Upstream resolves auth at client-construction time, before buildParams:
    // a real API key selects the Vertex Express client; everything else falls
    // back to ADC, which requires project and location up front.
    let api_key = resolve_api_key(options.stream.api_key.as_deref());
    let (project, location) = if api_key.is_none() {
        (
            Some(resolve_project(
                options.project.as_deref(),
                options.stream.env.as_ref(),
            )?),
            Some(resolve_location(
                options.location.as_deref(),
                options.stream.env.as_ref(),
            )?),
        )
    } else {
        (None, None)
    };

    let mut params = build_params(model, context, options)?;
    if let Some(on_payload) = &options.stream.on_payload
        && let Some(next_params) = on_payload(params.clone(), model).await
    {
        params = next_params;
    }
    let (model_path, body) = params_to_wire(&params);

    // The ADC token is fetched lazily at request time in the SDK
    // (`NodeAuth.addAuthHeaders` inside `requestStream`), i.e. after
    // buildParams and the onPayload hook.
    let auth_header = match &api_key {
        Some(api_key) => ("x-goog-api-key", api_key.clone()),
        None => {
            let endpoints = options.adc_endpoints.clone().unwrap_or_default();
            let token = resolve_access_token(options.stream.env.as_ref(), &endpoints).await?;
            ("authorization", format!("Bearer {token}"))
        }
    };

    let url = resolve_request_url(
        model,
        api_key.is_some(),
        project.as_deref(),
        location.as_deref(),
        &model_path,
    );

    let headers = build_request_headers(model, auth_header, options.stream.headers.as_ref());
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

    // b9d360a2c: the initial request runs under the shared provider retry
    // policy via `retryGoogleRequest` (opt-in through `maxRetries`; default
    // unchanged).
    let response = retry_google_request(
        || {
            let request = client.post(&url).headers(header_map.clone()).json(&body);
            let signal = options.stream.signal.clone();
            async move {
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
                            let status_code = status.as_u16();
                            let reason = status.canonical_reason().unwrap_or_default().to_owned();
                            let is_json = response
                                .headers()
                                .get(reqwest::header::CONTENT_TYPE)
                                .and_then(|value| value.to_str().ok())
                                .is_some_and(|content_type| {
                                    content_type.contains("application/json")
                                });
                            let response_headers = headers_to_record(response.headers());
                            let body_text = response.text().await.unwrap_or_default();
                            // SDK `throwErrorIfNotOK`: the message is the
                            // stringified error body (JSON bodies verbatim;
                            // non-JSON wrapped in an error object).
                            let message = if is_json {
                                body_text
                            } else {
                                serde_json::to_string(&json!({
                                    "error": {
                                        "message": body_text,
                                        "code": status_code,
                                        "status": reason,
                                    }
                                }))
                                .unwrap_or_else(|_| "{}".to_owned())
                            };
                            Err(ProviderErrorInfo {
                                status: Some(status_code),
                                headers: Some(response_headers),
                                message,
                            })
                        }
                    }
                    crate::api::stream_timeouts::SendOutcome::Transport(error) => {
                        Err(ProviderErrorInfo {
                            status: error.status().map(|status| status.as_u16()),
                            headers: None,
                            message: crate::utils::transport_error::format_reqwest_error(&error),
                        })
                    }
                    crate::api::stream_timeouts::SendOutcome::HeadersTimeout(message) => {
                        Err(ProviderErrorInfo {
                            status: None,
                            headers: None,
                            message,
                        })
                    }
                }
            }
        },
        &options.stream,
    )
    .await
    .map_err(|error| error.message())?;

    let status = response.status();

    if let Some(on_response) = &options.stream.on_response {
        on_response(
            ProviderResponse {
                status: status.as_u16(),
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
        let chunk =
            match next_chunk_or_cancelled(&mut byte_stream, options.stream.signal.as_ref()).await {
                StreamNext::Item(chunk) => chunk,
                StreamNext::Cancelled => return Err("Request was aborted".to_owned()),
            };
        let Some(chunk) = chunk else { break };
        let bytes = chunk.map_err(|error| error.message())?;
        check_raw_chunk_error(&bytes)?;
        for sse in decoder.feed(&bytes)? {
            processor.handle_sse(&sse, events)?;
        }
    }
    for sse in decoder.finish()? {
        processor.handle_sse(&sse, events)?;
    }
    // End-of-stream: close the trailing text/thinking block before validation.
    processor.end_current_block(events);
    finish_processor(processor, options.stream.signal.as_ref())
}

fn finish_processor(
    processor: StreamProcessor,
    signal: Option<&CancellationToken>,
) -> Result<DoneReason, String> {
    if signal.is_some_and(|signal| signal.is_cancelled()) {
        return Err("Request was aborted".to_owned());
    }
    match processor.output.stop_reason {
        // `Deferred` shares the `Pending` arm: no rpi provider produces it
        // (lifecycle is [DEFER], R2.2.1), so it is unreachable here and
        // treated as "stream ended without a usable finish reason".
        StopReason::Pending | StopReason::Deferred => {
            Err("Google Vertex stream ended without a finish reason".to_owned())
        }
        // 23cb385b6 + 5a2539a7b: the raw provider reason becomes the error
        // text when one was seen.
        StopReason::Aborted | StopReason::Error => Err(match &processor.output.raw_stop_reason {
            Some(raw) => format!("Provider stopped with: {raw}"),
            None => "An unknown error occurred".to_owned(),
        }),
        StopReason::Stop => Ok(DoneReason::Stop),
        StopReason::Length => Ok(DoneReason::Length),
        StopReason::ToolUse => Ok(DoneReason::ToolUse),
    }
}

/// `stream` (google-vertex).
pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: GoogleVertexOptions,
) -> AssistantMessageEventStream {
    let event_stream = AssistantMessageEventStream::new();
    let task_stream = event_stream.clone();
    let model = model.clone();
    let context = context.clone();
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

/// `streamSimple` (google-vertex). No API-key preflight: a missing key is the
/// legitimate ADC path (unlike the Gemini API adapter).
pub fn stream_simple(
    model: &Model,
    context: &TranscriptContext,
    options: Option<SimpleStreamOptions>,
) -> Result<AssistantMessageEventStream, String> {
    let base = build_base_options(model, context, options.as_ref(), None);
    // `base` carries `toolChoice` on every branch upstream
    // (google-vertex.ts:321 — rc.12 review: previously dropped).
    let tool_choice = options
        .as_ref()
        .and_then(|o| o.tool_choice)
        .map(|choice| match choice {
            crate::types::SimpleToolChoice::Auto => GoogleToolChoice::Auto,
            crate::types::SimpleToolChoice::None => GoogleToolChoice::None,
        });
    let Some(reasoning) = options.as_ref().and_then(|o| o.reasoning) else {
        return Ok(stream(
            model,
            context,
            GoogleVertexOptions {
                stream: base,
                tool_choice,
                thinking: Some(GoogleThinking {
                    enabled: false,
                    budget_tokens: None,
                    level: None,
                }),
                project: None,
                location: None,
                adc_endpoints: None,
            },
        ));
    };

    let clamped = clamp_thinking_level(model, reasoning.to_model_level());
    // #9455 (16235fd93): a clamped "off" disables thinking outright (no
    // resolve — `off` is no longer force-mapped to `high`).
    if clamped == crate::types::ModelThinkingLevel::Off {
        return Ok(stream(
            model,
            context,
            GoogleVertexOptions {
                stream: base,
                tool_choice,
                thinking: Some(GoogleThinking {
                    enabled: false,
                    budget_tokens: None,
                    level: None,
                }),
                project: None,
                location: None,
                adc_endpoints: None,
            },
        ));
    }
    // `resolveGoogleThinkingLevel(model, clampedReasoning)` (af2c35223/#8135):
    // thinkingLevelMap entries win; xhigh/max without a mapping are an error.
    let effort = resolve_google_thinking_level(model, clamped)?;

    if uses_google_thinking_level(model) {
        return Ok(stream(
            model,
            context,
            GoogleVertexOptions {
                stream: base,
                tool_choice,
                thinking: Some(GoogleThinking {
                    enabled: true,
                    budget_tokens: None,
                    level: Some(to_google_thinking_level(effort)),
                }),
                project: None,
                location: None,
                adc_endpoints: None,
            },
        ));
    }

    Ok(stream(
        model,
        context,
        GoogleVertexOptions {
            stream: base,
            tool_choice,
            thinking: Some(GoogleThinking {
                enabled: true,
                budget_tokens: Some(get_google_budget(
                    model,
                    effort,
                    options.as_ref().and_then(|o| o.thinking_budgets.as_ref()),
                )),
                level: None,
            }),
            project: None,
            location: None,
            adc_endpoints: None,
        },
    ))
}

/// `ProviderStreams` implementation for `ApiKind::GOOGLE_VERTEX`.
///
/// The trait carries plain [`StreamOptions`]; Vertex-specific extras
/// ([`GoogleVertexOptions`]) reach [`stream`] only through direct calls or
/// via [`stream_simple`] reasoning mapping (design §3.3 collapses per-API
/// extras). Note that trait-level calls cannot pass `project`/`location`
/// options — the `GOOGLE_CLOUD_PROJECT` / `GCLOUD_PROJECT` /
/// `GOOGLE_CLOUD_LOCATION` env chain (including `StreamOptions::env`) covers
/// that path, as upstream.
#[derive(Debug, Clone, Copy, Default)]
pub struct GoogleVertex;

impl ProviderStreams for GoogleVertex {
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        stream(
            model,
            context,
            GoogleVertexOptions {
                stream: options.unwrap_or_default(),
                ..GoogleVertexOptions::default()
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
