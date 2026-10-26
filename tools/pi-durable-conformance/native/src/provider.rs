use async_trait::async_trait;
use rpi_ai::auth::{
    ApiKeyAuth, ApiKeyCredential, AuthContext, AuthResult, ModelAuth, ProviderAuth,
};
use rpi_ai::models::{Models, Provider};
use rpi_ai::types::{
    ApiKind, AssistantContent, AssistantMessage, AssistantRole, CacheRetention, DoneReason,
    ErrorReason, InputModality, Message, Model, ModelCost, SimpleStreamOptions, StopReason,
    StreamEvent, StreamOptions, SystemContent, TextContent, ToolCall, ToolResultContent,
    TranscriptContext, Usage, UsageCost, UserContent, UserContentBlock,
};
use rpi_ai::utils::event_stream::AssistantMessageEventStream;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct FauxUsageEstimator {
    prompt_cache: Mutex<HashMap<String, Vec<String>>>,
}

impl FauxUsageEstimator {
    pub fn apply(
        &self,
        message: &mut AssistantMessage,
        context: &TranscriptContext,
        options: Option<&StreamOptions>,
    ) {
        let prompt = context
            .messages
            .iter()
            .map(message_text)
            .collect::<Vec<_>>();
        let prompt_chars = joined_length(&prompt);
        let prompt_tokens = prompt_chars.div_ceil(4) as u64;
        let output_tokens =
            text_length(&assistant_content_text(&message.content)).div_ceil(4) as u64;
        let mut input_tokens = prompt_tokens;
        let mut cache_read = 0;
        let mut cache_write = 0;
        if let Some(session_id) = options
            .filter(|options| options.cache_retention != Some(CacheRetention::None))
            .and_then(|options| options.session_id.as_deref())
        {
            let mut prompt_cache = self
                .prompt_cache
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let previous = prompt_cache.insert(session_id.to_string(), prompt.clone());
            if let Some(previous) = previous {
                cache_read = common_prompt_prefix_length(&previous, &prompt).div_ceil(4) as u64;
                cache_write = prompt_tokens.saturating_sub(cache_read);
            } else {
                cache_write = prompt_tokens;
            }
            input_tokens = 0;
        }
        message.usage = Usage {
            input: input_tokens,
            output: output_tokens,
            cache_read,
            cache_write,
            cache_write1h: None,
            reasoning: None,
            total_tokens: input_tokens + output_tokens + cache_read + cache_write,
            cost: UsageCost::default(),
        };
    }
}

fn text_length(text: &str) -> usize {
    text.encode_utf16().count()
}

fn joined_length(messages: &[String]) -> usize {
    messages
        .iter()
        .map(|message| text_length(message))
        .sum::<usize>()
        + messages.len().saturating_sub(1) * 2
}

fn common_prompt_prefix_length(previous: &[String], current: &[String]) -> usize {
    let previous = previous.join("\n\n");
    let current = current.join("\n\n");
    previous
        .chars()
        .zip(current.chars())
        .take_while(|(left, right)| left == right)
        .map(|(character, _)| character.len_utf16())
        .sum()
}

fn message_text(message: &Message) -> String {
    let (role, text) = match message {
        Message::System(message) => ("system", system_message_text(message)),
        Message::User(message) => ("user", user_content_text(&message.content)),
        Message::Assistant(message) => ("assistant", assistant_content_text(&message.content)),
        Message::ToolResult(message) => ("toolResult", tool_result_text(message)),
    };
    format!("{role}:{text}")
}

fn system_message_text(message: &rpi_ai::types::SystemMessage) -> String {
    let mut prompt = vec![system_content_text(&message.content)];
    if let Some(sections) = &message.sections {
        prompt.extend(
            sections
                .values()
                .filter_map(Value::as_str)
                .map(str::to_string),
        );
    }
    let system_text = prompt
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut parts = Vec::new();
    if !system_text.is_empty() {
        parts.push(system_text);
    }
    if let Some(tools) = &message.tools_removed {
        parts.extend(
            tools
                .iter()
                .map(|tool| format!("tool-:{}", json!({"name": &tool.name}))),
        );
    }
    if let Some(tools) = &message.tools_added {
        parts.extend(tools.iter().map(|tool| {
            format!("tool+:{}", json!({"name": &tool.name, "description": &tool.description, "parameters": &tool.parameters}))
        }));
    }
    parts.join("\n")
}

fn system_content_text(content: &SystemContent) -> String {
    match content {
        SystemContent::Text(text) => text.clone(),
        SystemContent::Blocks(blocks) => blocks
            .iter()
            .map(|block| block.text.clone())
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn user_content_text(content: &UserContent) -> String {
    match content {
        UserContent::Text(text) => text.clone(),
        UserContent::Blocks(blocks) => blocks
            .iter()
            .map(|block| match block {
                UserContentBlock::Text(text) => text.text.clone(),
                UserContentBlock::Image(image) => {
                    format!("[image:{}:{}]", image.mime_type, image.data.len())
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn assistant_content_text(content: &[AssistantContent]) -> String {
    content
        .iter()
        .map(|part| match part {
            AssistantContent::Text(text) => text.text.clone(),
            AssistantContent::Thinking(thinking) => thinking.thinking.clone(),
            AssistantContent::ToolCall(call) => {
                format!("{}:{}", call.name, Value::Object(call.arguments.clone()))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn tool_result_text(message: &rpi_ai::types::ToolResultMessage) -> String {
    let mut parts = vec![message.tool_name.clone()];
    parts.extend(message.content.iter().map(|content| match content {
        ToolResultContent::Text(text) => text.text.clone(),
        ToolResultContent::Image(image) => {
            format!("[image:{}:{}]", image.mime_type, image.data.len())
        }
    }));
    parts.join("\n")
}

pub struct OracleProvider {
    model: Model,
    auth: ProviderAuth,
    scripts: Mutex<VecDeque<AssistantMessage>>,
    pub requests: Arc<Mutex<Vec<Value>>>,
    pub request_error: Mutex<Option<String>>,
    usage: FauxUsageEstimator,
}

struct OracleAuth;

#[async_trait]
impl ApiKeyAuth for OracleAuth {
    fn name(&self) -> &str {
        "pi-durable-conformance"
    }

    async fn resolve(
        &self,
        _context: &dyn AuthContext,
        _credential: Option<&ApiKeyCredential>,
    ) -> Result<Option<AuthResult>, rpi_ai::auth::ModelsError> {
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: None,
                headers: None,
                base_url: None,
            },
            env: None,
            source: None,
        }))
    }
}

impl OracleProvider {
    pub fn new(messages: impl IntoIterator<Item = AssistantMessage>) -> (Models, Arc<Self>) {
        let provider = Arc::new(Self {
            model: Model {
                id: "faux-1".to_string(),
                name: "Faux 1".to_string(),
                api: ApiKind::from("faux"),
                provider: "faux".to_string(),
                base_url: "http://localhost".to_string(),
                reasoning: false,
                thinking_level_map: None,
                input: vec![InputModality::Text],
                input_limits: None,
                cost: ModelCost::default(),
                prompt_cache: None,
                context_window: 200_000,
                max_tokens: 4_096,
                sampling_params: None,
                sampling_params_by_thinking_level: None,
                headers: None,
                compat: None,
            },
            auth: ProviderAuth {
                api_key: Some(Arc::new(OracleAuth)),
                oauth: None,
            },
            scripts: Mutex::new(messages.into_iter().collect()),
            requests: Arc::new(Mutex::new(Vec::new())),
            request_error: Mutex::new(None),
            usage: FauxUsageEstimator::default(),
        });
        let models = Models::new(None);
        models.set_provider(Arc::clone(&provider) as Arc<dyn Provider>);
        (models, provider)
    }

    fn next(&self) -> Option<AssistantMessage> {
        self.scripts
            .lock()
            .ok()
            .and_then(|mut scripts| scripts.pop_front())
    }

    fn stream_message(message: AssistantMessage) -> AssistantMessageEventStream {
        let stream = AssistantMessageEventStream::new();
        let mut partial = message.clone();
        partial.stop_reason = StopReason::Pending;
        stream.push(StreamEvent::Start {
            partial: Arc::new(partial),
        });
        match message.stop_reason {
            StopReason::Error => stream.push(StreamEvent::Error {
                reason: ErrorReason::Error,
                error: message,
            }),
            StopReason::Deferred => stream.push(StreamEvent::Done {
                reason: DoneReason::Deferred,
                message,
            }),
            StopReason::Length => stream.push(StreamEvent::Done {
                reason: DoneReason::Length,
                message,
            }),
            StopReason::ToolUse => stream.push(StreamEvent::Done {
                reason: DoneReason::ToolUse,
                message,
            }),
            _ => stream.push(StreamEvent::Done {
                reason: DoneReason::Stop,
                message,
            }),
        }
        stream
    }

    fn record_request(&self, context: &TranscriptContext) {
        match serde_json::to_value(&context.messages) {
            Ok(request) => {
                if let Ok(mut requests) = self.requests.lock() {
                    requests.push(request);
                }
            }
            Err(error) => {
                if let Ok(mut failure) = self.request_error.lock() {
                    *failure = Some(error.to_string());
                }
            }
        }
    }
}

impl Provider for OracleProvider {
    fn id(&self) -> &str {
        "faux"
    }
    fn name(&self) -> &str {
        "Faux"
    }
    fn base_url(&self) -> Option<&str> {
        Some("http://localhost")
    }
    fn headers(&self) -> Option<&rpi_ai::types::ProviderHeaders> {
        None
    }
    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }
    fn get_models(&self) -> Vec<Model> {
        vec![self.model.clone()]
    }

    fn stream(
        &self,
        _model: &Model,
        context: &TranscriptContext,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        self.record_request(context);
        let mut message = self.next().unwrap_or_else(|| {
            message(
                "no scripted faux reply",
                StopReason::Error,
                Some("no scripted faux reply"),
            )
        });
        self.usage.apply(&mut message, context, options.as_ref());
        Self::stream_message(message)
    }

    fn stream_simple(
        &self,
        _model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> Result<AssistantMessageEventStream, String> {
        self.record_request(context);
        let mut message = self.next().unwrap_or_else(|| {
            message(
                "no scripted faux reply",
                StopReason::Error,
                Some("no scripted faux reply"),
            )
        });
        self.usage.apply(
            &mut message,
            context,
            options.as_ref().map(|options| &options.stream),
        );
        Ok(Self::stream_message(message))
    }
}

pub fn message(text: &str, stop_reason: StopReason, error: Option<&str>) -> AssistantMessage {
    let content = if text.is_empty() {
        Vec::new()
    } else {
        vec![AssistantContent::Text(TextContent {
            text: text.to_string(),
            text_signature: None,
        })]
    };
    AssistantMessage {
        role: AssistantRole::Assistant,
        content,
        api: ApiKind::from(format!(
            "faux:{}:{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis())
                .unwrap_or_default(),
            uuid::Uuid::new_v4().simple(),
        )),
        provider: "faux".to_string(),
        model: "faux-1".to_string(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason,
        deferred: None,
        error_message: error.map(str::to_string),
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 1,
        duration_ms: None,
    }
}

pub fn tool_call(name: &str, id: &str, arguments: Value) -> AssistantMessage {
    let arguments = arguments.as_object().cloned().unwrap_or_else(Map::new);
    let mut result = message("", StopReason::ToolUse, None);
    result.content.push(AssistantContent::ToolCall(ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments,
        thought_signature: None,
        namespace: None,
    }));
    result
}
