//! `AgentMessage` union and the extension-message text format constants.
//!
//! Port of the `AgentMessage` union from `packages/agent/src/types.ts` plus
//! the coding-agent custom message types and format constants from
//! `packages/coding-agent/src/core/messages.ts` @ pi 0.82.1 (2efa728).
//!
//! Intentional differences: TS declaration merging
//! (`interface CustomAgentMessages`) has no Rust equivalent, so the four
//! coding-agent custom message types (`bashExecution`, `custom`,
//! `branchSummary`, `compactionSummary`) are folded directly into the
//! [`AgentMessage`] union. This is a structural, not behavioral, difference:
//! the wire shapes are identical.
//!
//! The conversion logic (`bashExecutionToText`, `convertToLlm`) is a verbatim
//! port of `packages/coding-agent/src/core/messages.ts` @ 2efa728 (T05).

use rpi_ai::types::{
    AssistantMessage, Message, TextContent, ToolResultMessage, UserContent, UserContentBlock,
    UserMessage, UserRole,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `COMPACTION_SUMMARY_PREFIX` — byte-exact port (messages.ts).
pub const COMPACTION_SUMMARY_PREFIX: &str = "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";

/// `COMPACTION_SUMMARY_SUFFIX` — byte-exact port (messages.ts).
pub const COMPACTION_SUMMARY_SUFFIX: &str = "\n</summary>";

/// `BRANCH_SUMMARY_PREFIX` — byte-exact port (messages.ts).
pub const BRANCH_SUMMARY_PREFIX: &str =
    "The following is a summary of a branch that this conversation came back from:\n\n<summary>\n";

/// `BRANCH_SUMMARY_SUFFIX` — byte-exact port (messages.ts).
pub const BRANCH_SUMMARY_SUFFIX: &str = "</summary>";

/// Role marker for [`BashExecutionMessage`] (`role: "bashExecution"`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BashExecutionRole {
    #[default]
    #[serde(rename = "bashExecution")]
    BashExecution,
}

/// Role marker for [`CustomMessage`] (`role: "custom"`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CustomRole {
    #[default]
    #[serde(rename = "custom")]
    Custom,
}

/// Role marker for [`BranchSummaryMessage`] (`role: "branchSummary"`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BranchSummaryRole {
    #[default]
    #[serde(rename = "branchSummary")]
    BranchSummary,
}

/// Role marker for [`CompactionSummaryMessage`] (`role: "compactionSummary"`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompactionSummaryRole {
    #[default]
    #[serde(rename = "compactionSummary")]
    CompactionSummary,
}

/// `BashExecutionMessage` — bash executions via the `!` command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashExecutionMessage {
    pub role: BashExecutionRole,
    pub command: String,
    pub output: String,
    /// `number | undefined` upstream (undefined = still running / unknown).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub cancelled: bool,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
    /// Unix timestamp in milliseconds.
    pub timestamp: i64,
    /// If true, this message is excluded from LLM context (`!!` prefix).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude_from_context: Option<bool>,
}

/// `CustomMessage` — extension-injected messages via `sendMessage()`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessage {
    pub role: CustomRole,
    pub custom_type: String,
    pub content: UserContent,
    pub display: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    /// Unix timestamp in milliseconds.
    pub timestamp: i64,
}

/// `BranchSummaryMessage`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryMessage {
    pub role: BranchSummaryRole,
    pub summary: String,
    pub from_id: String,
    /// Unix timestamp in milliseconds.
    pub timestamp: i64,
}

/// `CompactionSummaryMessage`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSummaryMessage {
    pub role: CompactionSummaryRole,
    pub summary: String,
    pub tokens_before: u64,
    /// Unix timestamp in milliseconds.
    pub timestamp: i64,
}

/// `AgentMessage` — union of LLM messages + custom messages.
///
/// Untagged: each member struct carries a single-variant role marker, so the
/// `role` literal disambiguates variants and every member also serializes
/// correctly standalone (upstream includes `role` in every serialized
/// message object).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentMessage {
    User(UserMessage),
    Assistant(AssistantMessage),
    ToolResult(ToolResultMessage),
    System(rpi_ai::types::SystemMessage),
    BashExecution(BashExecutionMessage),
    Custom(CustomMessage),
    BranchSummary(BranchSummaryMessage),
    CompactionSummary(CompactionSummaryMessage),
}

/// `bashExecutionToText` — verbatim port of
/// `packages/coding-agent/src/core/messages.ts:63-79` @ 2efa728.
pub fn bash_execution_to_text(msg: &BashExecutionMessage) -> String {
    let mut text = format!("Ran `{}`\n", msg.command);
    if msg.output.is_empty() {
        text.push_str("(no output)");
    } else {
        text.push_str(&format!("```\n{}\n```", msg.output));
    }
    if msg.cancelled {
        text.push_str("\n\n(command cancelled)");
    } else if let Some(exit_code) = msg.exit_code
        && exit_code != 0
    {
        text.push_str(&format!("\n\nCommand exited with code {exit_code}"));
    }
    if msg.truncated
        && let Some(full_output_path) = &msg.full_output_path
        && !full_output_path.is_empty()
    {
        text.push_str(&format!(
            "\n\n[Output truncated. Full output: {full_output_path}]"
        ));
    }
    text
}

/// `convertToLlm` — verbatim port of
/// `packages/coding-agent/src/core/messages.ts:120-164` @ 2efa728.
///
/// Converts `AgentMessage[]` to LLM-compatible `Message[]`; messages that
/// cannot be converted are filtered out.
pub fn convert_to_llm(messages: &[AgentMessage]) -> Vec<Message> {
    messages
        .iter()
        .filter_map(|m| match m {
            AgentMessage::BashExecution(b) => {
                if b.exclude_from_context == Some(true) {
                    return None;
                }
                Some(Message::User(UserMessage {
                    role: UserRole::User,
                    content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent {
                        text: bash_execution_to_text(b),
                        text_signature: None,
                    })]),
                    timestamp: b.timestamp,
                }))
            }
            AgentMessage::Custom(c) => {
                let content = match &c.content {
                    UserContent::Text(s) => {
                        UserContent::Blocks(vec![UserContentBlock::Text(TextContent {
                            text: s.clone(),
                            text_signature: None,
                        })])
                    }
                    UserContent::Blocks(blocks) => UserContent::Blocks(blocks.clone()),
                };
                Some(Message::User(UserMessage {
                    role: UserRole::User,
                    content,
                    timestamp: c.timestamp,
                }))
            }
            AgentMessage::BranchSummary(b) => Some(Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent {
                    text: format!(
                        "{}{}{}",
                        BRANCH_SUMMARY_PREFIX, b.summary, BRANCH_SUMMARY_SUFFIX
                    ),
                    text_signature: None,
                })]),
                timestamp: b.timestamp,
            })),
            AgentMessage::CompactionSummary(c) => Some(Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent {
                    text: format!(
                        "{}{}{}",
                        COMPACTION_SUMMARY_PREFIX, c.summary, COMPACTION_SUMMARY_SUFFIX
                    ),
                    text_signature: None,
                })]),
                timestamp: c.timestamp,
            })),
            AgentMessage::User(u) => Some(Message::User(u.clone())),
            AgentMessage::Assistant(a) => Some(Message::Assistant(a.clone())),
            AgentMessage::ToolResult(t) => Some(Message::ToolResult(t.clone())),
            // #9548 (agent messages.ts `case "system"`): the transcript's
            // system messages pass through to the LLM context.
            AgentMessage::System(s) => Some(Message::System(s.clone())),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use rpi_ai::types::{AssistantRole, StopReason, Usage};
    use serde_json::{Value, json};

    use super::*;

    fn assistant_msg() -> AssistantMessage {
        AssistantMessage {
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
            timestamp: 0,
            deferred: None,
            end_turn: None,
            raw_stop_reason: None,
            duration_ms: None,
        }
    }

    fn to_json<T: Serialize>(v: &T) -> String {
        serde_json::to_string(v).expect("serialization must succeed")
    }

    #[test]
    fn summary_format_constants_are_byte_exact() {
        // Pinned literals from packages/coding-agent/src/core/messages.ts.
        assert_eq!(
            COMPACTION_SUMMARY_PREFIX,
            "The conversation history before this point was compacted into the following summary:\n\n<summary>\n"
        );
        assert_eq!(COMPACTION_SUMMARY_SUFFIX, "\n</summary>");
        assert_eq!(
            BRANCH_SUMMARY_PREFIX,
            "The following is a summary of a branch that this conversation came back from:\n\n<summary>\n"
        );
        assert_eq!(BRANCH_SUMMARY_SUFFIX, "</summary>");
    }

    #[test]
    fn bash_execution_message_shape() {
        let msg = BashExecutionMessage {
            role: BashExecutionRole::BashExecution,
            command: "ls -la".to_owned(),
            output: "total 0".to_owned(),
            exit_code: Some(0),
            cancelled: false,
            truncated: true,
            full_output_path: Some("/tmp/x".to_owned()),
            timestamp: 1,
            exclude_from_context: Some(true),
        };
        assert_eq!(
            to_json(&msg),
            r#"{"role":"bashExecution","command":"ls -la","output":"total 0","exitCode":0,"cancelled":false,"truncated":true,"fullOutputPath":"/tmp/x","timestamp":1,"excludeFromContext":true}"#
        );

        // `exitCode: undefined` upstream -> key omitted when None.
        let running = BashExecutionMessage {
            exit_code: None,
            full_output_path: None,
            exclude_from_context: None,
            ..msg.clone()
        };
        let v: Value = serde_json::from_str(&to_json(&running)).expect("parse");
        assert!(v.get("exitCode").is_none());
        assert!(v.get("fullOutputPath").is_none());
        assert!(v.get("excludeFromContext").is_none());

        // Roundtrip through the AgentMessage union (role literal routes it).
        let union: AgentMessage = serde_json::from_str(&to_json(&msg)).expect("union roundtrip");
        assert_eq!(union, AgentMessage::BashExecution(msg));
    }

    #[test]
    fn custom_message_shape() {
        let msg = CustomMessage {
            role: CustomRole::Custom,
            custom_type: "artifact".to_owned(),
            content: UserContent::Text("hi".to_owned()),
            display: true,
            details: Some(json!({"k": 1})),
            timestamp: 2,
        };
        assert_eq!(
            to_json(&msg),
            r#"{"role":"custom","customType":"artifact","content":"hi","display":true,"details":{"k":1},"timestamp":2}"#
        );
        let union: AgentMessage = serde_json::from_str(&to_json(&msg)).expect("union roundtrip");
        assert_eq!(union, AgentMessage::Custom(msg));
    }

    #[test]
    fn branch_and_compaction_summary_shapes() {
        let branch = BranchSummaryMessage {
            role: BranchSummaryRole::BranchSummary,
            summary: "s".to_owned(),
            from_id: "e1".to_owned(),
            timestamp: 3,
        };
        assert_eq!(
            to_json(&branch),
            r#"{"role":"branchSummary","summary":"s","fromId":"e1","timestamp":3}"#
        );

        let compaction = CompactionSummaryMessage {
            role: CompactionSummaryRole::CompactionSummary,
            summary: "s".to_owned(),
            tokens_before: 10,
            timestamp: 4,
        };
        assert_eq!(
            to_json(&compaction),
            r#"{"role":"compactionSummary","summary":"s","tokensBefore":10,"timestamp":4}"#
        );

        let b: AgentMessage = serde_json::from_str(&to_json(&branch)).expect("roundtrip");
        assert_eq!(b, AgentMessage::BranchSummary(branch));
        let c: AgentMessage = serde_json::from_str(&to_json(&compaction)).expect("roundtrip");
        assert_eq!(c, AgentMessage::CompactionSummary(compaction));
    }

    #[test]
    fn llm_messages_roundtrip_through_agent_message_union() {
        let assistant = assistant_msg();
        let v = to_json(&assistant);
        let union: AgentMessage = serde_json::from_str(&v).expect("assistant roundtrip");
        assert_eq!(union, AgentMessage::Assistant(assistant));
    }
}
