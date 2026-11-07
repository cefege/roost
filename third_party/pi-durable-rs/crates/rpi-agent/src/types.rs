//! Port of `packages/agent/src/types.ts` @ pi 0.82.1 (2efa728).
//!
//! Agent-side tool types and the `AgentEvent` contract (10 variants with
//! payloads — the payload structure is part of the parity contract,
//! requirements §4.2).
//!
//! Intentional differences:
//! - `AgentTool` (a TS interface with an `execute` callback) becomes an
//!   `async_trait`; parameters/results use `serde_json::Value` where upstream
//!   is generic over TypeBox schema types, keeping the trait object-safe.
//! - Hook context/config types (`AgentLoopConfig`, `BeforeToolCallContext`,
//!   ...) live in `agent_loop.rs` (loop-facing hooks) and `agent.rs`
//!   (`AgentState`), ported with T05.
//!
//! Scope of this file is locked at M0 (T01): variant order and field naming
//! mirror the upstream TS definitions item by item; changes require gate G3
//! and fixture updates (coding-standards §4.1).

use async_trait::async_trait;
use rpi_ai::types::{StreamEvent, ToolResultContent, ToolResultMessage, Usage};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::messages::AgentMessage;

/// Agent-side thinking level: `"off" | "minimal" | ... | "max"`.
///
/// Identical value set to `rpi_ai::types::ModelThinkingLevel`; re-exported
/// under the upstream name (`packages/agent/src/types.ts` ThinkingLevel).
pub type ThinkingLevel = rpi_ai::types::ModelThinkingLevel;

/// `ToolExecutionMode = "sequential" | "parallel"`.
///
/// - `Sequential`: each tool call is prepared, executed, and finalized before
///   the next one starts.
/// - `Parallel`: tool calls are prepared sequentially, then allowed tools
///   execute concurrently. `tool_execution_end` is emitted in completion
///   order; tool-result message artifacts later in assistant source order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolExecutionMode {
    #[serde(rename = "sequential")]
    Sequential,
    #[serde(rename = "parallel")]
    Parallel,
}

/// `AgentTool.replay` (`"never" | "safe"`) — recovery policy for an
/// effect whose durable intent exists but whose outcome is unknown
/// (packages/agent/src/types.ts:401-402).
///
/// Type-surface only: the durable harness reads it during recovery; the
/// classic agent loop never consumes this field (rpi keeps the same split,
/// the `harness/` module is out of scope for v0.1.4 per ADR-0023).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReplayPolicy {
    /// Never replay the effect; surface the unknown outcome instead.
    #[serde(rename = "never")]
    Never,
    /// Replay is safe (idempotent effects only).
    #[serde(rename = "safe")]
    Safe,
}

/// `QueueMode = "all" | "one-at-a-time"` — how many queued user messages are
/// injected when the agent loop reaches a queue drain point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueueMode {
    /// Drain and inject every queued message at that point.
    #[serde(rename = "all")]
    All,
    /// Drain and inject only the oldest queued message (default upstream).
    #[serde(rename = "one-at-a-time")]
    OneAtATime,
}

/// `AgentToolCall` — a single tool call content block from an assistant message.
pub type AgentToolCall = rpi_ai::types::ToolCall;

/// `AgentToolResult<T>` — final or partial result produced by a tool.
/// `details` is `serde_json::Value` where upstream is generic (see header).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentToolResult {
    /// Text or image content returned to the model.
    pub content: Vec<ToolResultContent>,
    /// Arbitrary structured details for logs or UI rendering. Optional on
    /// the wire like upstream (`details` is a plain JS property an
    /// extension may omit); a missing field deserializes to `null` instead
    /// of failing the whole tool result.
    #[serde(default)]
    pub details: Value,
    /// Machine-readable result matching the tool's `outputSchema`, for
    /// programmatic callers (types.ts:424-440 @ abe508e1b). Not sent to the
    /// model; `content` remains the model-facing result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    /// Usage from the final tool execution itself, if available. Not used for
    /// main LLM context accounting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Report a failure without throwing. The model sees `content` as an
    /// error result, like a thrown error, but `details` and
    /// `structured_content` are kept for the UI and programmatic callers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    /// Hint that the agent should stop after the current tool batch. Early
    /// termination only happens when every finalized tool result in the batch
    /// sets this to true. Runtime-only; never written to the transcript.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

/// `AgentToolUpdateCallback` — streams partial execution updates. Scoped to
/// the current `execute()` invocation; calls made after the tool future
/// settles are ignored.
pub type AgentToolUpdateCallback = Box<dyn Fn(AgentToolResult) + Send + Sync>;

/// `AgentTool` — tool definition used by the agent runtime.
///
/// Upstream `execute` throws on failure instead of encoding errors in
/// `content`; the Rust equivalent returns `Err(AgentError)`.
#[async_trait]
pub trait AgentTool: Send + Sync {
    /// Tool name (as seen by the model).
    fn name(&self) -> &str;
    /// Human-readable label for UI display.
    fn label(&self) -> &str;
    /// Tool description sent to the model.
    fn description(&self) -> &str;
    /// JSON Schema of the tool parameters (TypeBox upstream).
    fn parameters(&self) -> &Value;
    /// JSON Schema of `structuredContent` in successful results
    /// (types.ts:590 @ abe508e1b). Default: no structured output.
    fn output_schema(&self) -> Option<&Value> {
        None
    }
    /// Optional provider-side constrained sampling config (from `Tool`).
    fn constrained_sampling(&self) -> Option<rpi_ai::types::ConstrainedSampling> {
        None
    }
    /// Per-tool execution mode override; `None` applies the default mode.
    fn execution_mode(&self) -> Option<ToolExecutionMode> {
        None
    }
    /// Recovery policy for an effect whose durable intent exists but whose
    /// outcome is unknown (upstream `AgentTool.replay`, types.ts:401-402).
    /// Declared for SDK/extension tool definitions only — the classic agent
    /// loop never reads it (durable-harness recovery policy).
    fn replay(&self) -> Option<ReplayPolicy> {
        None
    }
    /// Optional compatibility shim for raw tool-call arguments before schema
    /// validation. Must return a value matching the parameters schema.
    /// Default: identity.
    fn prepare_arguments(&self, args: Value) -> Value {
        args
    }
    /// Execute the tool call. Return `Err` on failure instead of encoding
    /// errors in `content`.
    async fn execute(
        &self,
        tool_call_id: &str,
        params: Value,
        signal: CancellationToken,
        on_update: Option<AgentToolUpdateCallback>,
    ) -> Result<AgentToolResult, AgentError>;
}

/// `AgentEvent` — events emitted by the Agent for UI updates.
///
/// `agent_end` is the last event emitted for a run, but awaited
/// `Agent.subscribe()` listeners for that event are still part of run
/// settlement: the agent becomes idle only after those listeners finish.
///
/// Serde shape is the RPC / JSON-mode wire format (coding-standards §4.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AgentEvent {
    // Agent lifecycle
    AgentStart,
    AgentEnd {
        messages: Vec<AgentMessage>,
    },
    // Turn lifecycle — a turn is one assistant response + any tool calls/results
    TurnStart,
    TurnEnd {
        message: AgentMessage,
        tool_results: Vec<ToolResultMessage>,
    },
    // Message lifecycle — emitted for user, assistant, and toolResult messages
    MessageStart {
        message: AgentMessage,
    },
    /// Only emitted for assistant messages during streaming.
    ///
    /// V15-13 (rpi#53): both payloads are shared behind `Arc` — the
    /// accumulated partial and the delta event flow agent loop -> listeners
    /// -> UI queue without deep copies (clone is refcount only; serde shape
    /// unchanged). `assistant_message_event` is `Arc`-ed to keep the enum
    /// small (`clippy::large_enum_variant`).
    MessageUpdate {
        message: std::sync::Arc<rpi_ai::types::AssistantMessage>,
        assistant_message_event: std::sync::Arc<StreamEvent>,
    },
    MessageEnd {
        message: AgentMessage,
    },
    // Tool execution lifecycle
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        args: Value,
        /// Set when another tool (for example a codemode script) made this
        /// call (types.ts:1044-1051 @ abe508e1b; `parentToolCallId` at :1050).
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_tool_call_id: Option<String>,
    },
    ToolExecutionUpdate {
        tool_call_id: String,
        tool_name: String,
        args: Value,
        partial_result: Value,
        /// Set when another tool (for example a codemode script) made this
        /// call.
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_tool_call_id: Option<String>,
    },
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        result: Value,
        is_error: bool,
        /// Milliseconds `execute()` took, measured with a monotonic clock;
        /// absent when the tool did not run (types.ts:1087-1094
        /// @ abe508e1b, #10549).
        #[serde(skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        /// Set when another tool (for example a codemode script) made this
        /// call.
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_tool_call_id: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use rpi_ai::types::{AssistantRole, StopReason, TextContent, ToolResultRole, Usage, UserRole};
    use serde_json::{Value, json};

    use super::*;
    use crate::messages::BashExecutionMessage;

    fn to_json<T: Serialize>(v: &T) -> String {
        serde_json::to_string(v).expect("serialization must succeed")
    }

    fn user_msg() -> AgentMessage {
        AgentMessage::User(rpi_ai::types::UserMessage {
            role: UserRole::User,
            content: rpi_ai::types::UserContent::Text("hi".to_owned()),
            timestamp: 1,
        })
    }

    fn assistant_msg() -> AgentMessage {
        AgentMessage::Assistant(rpi_ai::types::AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![],
            api: "anthropic-messages".into(),
            provider: "anthropic".to_owned(),
            model: "m".to_owned(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 2,
            deferred: None,
            end_turn: None,
            raw_stop_reason: None,
            duration_ms: None,
        })
    }

    fn tool_result_msg() -> ToolResultMessage {
        ToolResultMessage {
            role: ToolResultRole::ToolResult,
            tool_call_id: "c1".to_owned(),
            tool_name: "read".to_owned(),
            content: vec![ToolResultContent::Text(TextContent {
                text: "out".to_owned(),
                text_signature: None,
            })],
            details: None,
            usage: None,
            nested_calls: None,
            is_error: false,
            timestamp: 3,
            duration_ms: None,
        }
    }

    #[test]
    fn agent_event_variant_type_literals() {
        // All 10 variants, `type` tags as upstream agent/types.ts.
        let cases: Vec<(AgentEvent, &str)> = vec![
            (AgentEvent::AgentStart, "agent_start"),
            (AgentEvent::AgentEnd { messages: vec![] }, "agent_end"),
            (AgentEvent::TurnStart, "turn_start"),
            (
                AgentEvent::TurnEnd {
                    message: assistant_msg(),
                    tool_results: vec![],
                },
                "turn_end",
            ),
            (
                AgentEvent::MessageStart {
                    message: user_msg(),
                },
                "message_start",
            ),
            (
                AgentEvent::MessageUpdate {
                    message: std::sync::Arc::new(match assistant_msg() {
                        AgentMessage::Assistant(m) => m,
                        _ => unreachable!("constructed above"),
                    }),
                    assistant_message_event: std::sync::Arc::new(StreamEvent::Done {
                        reason: rpi_ai::types::DoneReason::Stop,
                        message: match assistant_msg() {
                            AgentMessage::Assistant(m) => m,
                            _ => unreachable!("constructed above"),
                        },
                    }),
                },
                "message_update",
            ),
            (
                AgentEvent::MessageEnd {
                    message: user_msg(),
                },
                "message_end",
            ),
            (
                AgentEvent::ToolExecutionStart {
                    tool_call_id: "c".into(),
                    tool_name: "t".into(),
                    args: json!({}),

                    parent_tool_call_id: None,
                },
                "tool_execution_start",
            ),
            (
                AgentEvent::ToolExecutionUpdate {
                    tool_call_id: "c".into(),
                    tool_name: "t".into(),
                    args: json!({}),
                    partial_result: json!({}),

                    parent_tool_call_id: None,
                },
                "tool_execution_update",
            ),
            (
                AgentEvent::ToolExecutionEnd {
                    tool_call_id: "c".into(),
                    tool_name: "t".into(),
                    result: json!({}),
                    is_error: false,
                    duration_ms: Some(7),

                    parent_tool_call_id: None,
                },
                "tool_execution_end",
            ),
        ];
        assert_eq!(cases.len(), 10, "AgentEvent has exactly 10 variants");
        for (ev, expected_type) in &cases {
            let v: Value = serde_json::from_str(&to_json(ev)).expect("parse");
            assert_eq!(&v["type"], &json!(expected_type));
            // Roundtrip through the tagged union.
            let back: AgentEvent = serde_json::from_str(&to_json(ev)).expect("roundtrip");
            assert_eq!(&back, ev);
        }
    }

    #[test]
    fn agent_event_payload_field_names() {
        let turn_end = AgentEvent::TurnEnd {
            message: assistant_msg(),
            tool_results: vec![tool_result_msg()],
        };
        let v: Value = serde_json::from_str(&to_json(&turn_end)).expect("parse");
        assert!(v.get("message").is_some());
        assert!(v.get("toolResults").is_some());
        assert_eq!(v["toolResults"][0]["toolCallId"], json!("c1"));

        let update = AgentEvent::MessageUpdate {
            message: std::sync::Arc::new(match assistant_msg() {
                AgentMessage::Assistant(m) => m,
                _ => unreachable!("constructed above"),
            }),
            assistant_message_event: std::sync::Arc::new(StreamEvent::TextDelta {
                content_index: 0,
                delta: "d".to_owned(),
                partial: std::sync::Arc::new(match assistant_msg() {
                    AgentMessage::Assistant(m) => m,
                    _ => unreachable!("constructed above"),
                }),
            }),
        };
        let v: Value = serde_json::from_str(&to_json(&update)).expect("parse");
        assert_eq!(v["assistantMessageEvent"]["type"], json!("text_delta"));
        assert_eq!(v["assistantMessageEvent"]["contentIndex"], json!(0));

        let end = AgentEvent::ToolExecutionEnd {
            tool_call_id: "c1".to_owned(),
            tool_name: "bash".to_owned(),
            result: json!({"content": []}),
            is_error: true,
            duration_ms: None,

            parent_tool_call_id: None,
        };
        assert_eq!(
            to_json(&end),
            r#"{"type":"tool_execution_end","toolCallId":"c1","toolName":"bash","result":{"content":[]},"isError":true}"#
        );
        // `durationMs` rides the wire when the tool ran and stays absent
        // (not null) when it did not (types.ts:1093-1094 @ abe508e1b).
        let ran = AgentEvent::ToolExecutionEnd {
            tool_call_id: "c1".to_owned(),
            tool_name: "bash".to_owned(),
            result: json!({"content": []}),
            is_error: false,
            duration_ms: Some(42),

            parent_tool_call_id: None,
        };
        assert_eq!(
            to_json(&ran),
            r#"{"type":"tool_execution_end","toolCallId":"c1","toolName":"bash","result":{"content":[]},"isError":false,"durationMs":42}"#
        );
    }

    #[test]
    fn tool_execution_mode_and_queue_mode_literals() {
        assert_eq!(to_json(&ToolExecutionMode::Sequential), "\"sequential\"");
        assert_eq!(to_json(&ToolExecutionMode::Parallel), "\"parallel\"");
        assert_eq!(to_json(&QueueMode::All), "\"all\"");
        assert_eq!(to_json(&QueueMode::OneAtATime), "\"one-at-a-time\"");
    }

    #[test]
    fn agent_tool_result_shape() {
        let result = AgentToolResult {
            content: vec![ToolResultContent::Text(TextContent {
                text: "ok".to_owned(),
                text_signature: None,
            })],
            details: json!({"lines": 1}),
            usage: None,
            terminate: Some(false),

            structured_content: None,
            is_error: None,
        };
        assert_eq!(
            to_json(&result),
            r#"{"content":[{"type":"text","text":"ok"}],"details":{"lines":1},"terminate":false}"#
        );
    }

    #[test]
    fn agent_tool_result_without_details_deserializes() {
        // Extension tool results cross the ABI as plain JSON; upstream's
        // `details` is an optional JS property, so a plugin omitting it must
        // not fail the whole tool execution ("missing field `details`").
        let result: AgentToolResult =
            serde_json::from_str(r#"{"content":[{"type":"text","text":"ok"}],"isError":false}"#)
                .expect("details is optional on the wire");
        assert_eq!(result.details, Value::Null);
        assert_eq!(result.content.len(), 1);
    }

    #[test]
    fn agent_message_union_keeps_custom_roles() {
        // A bashExecution message survives an AgentMessage roundtrip with its
        // role literal intact (union disambiguation via role markers).
        let msg = AgentMessage::BashExecution(BashExecutionMessage {
            role: crate::messages::BashExecutionRole::BashExecution,
            command: "pwd".to_owned(),
            output: "/repo".to_owned(),
            exit_code: Some(0),
            cancelled: false,
            truncated: false,
            full_output_path: None,
            timestamp: 9,
            exclude_from_context: None,
        });
        let back: AgentMessage = serde_json::from_str(&to_json(&msg)).expect("roundtrip");
        assert_eq!(back, msg);
    }
}
