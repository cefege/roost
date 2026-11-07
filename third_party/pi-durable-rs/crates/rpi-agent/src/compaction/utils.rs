//! Port of `packages/coding-agent/src/core/compaction/utils.ts` @ pi 0.82.1
//! (2efa728).
//!
//! Shared utilities for compaction and branch summarization: file-operation
//! tracking and conversation serialization for the summarization prompts.
//!
//! Intentional difference: JS `String.length` counts UTF-16 code units;
//! `chars().count()` here counts Unicode scalar values. Identical for BMP
//! text (same convention as `rpi_ai::utils::estimate`, D-003). The truncation
//! marker reports the scalar count, matching the algorithm shape
//! (ADR-0002 §4).

use std::collections::HashSet;

use rpi_ai::types::Message;
use rpi_ai::utils::text::{content_text_assistant, content_text_tool_result, content_text_user};

use crate::messages::AgentMessage;

// ============================================================================
// File Operation Tracking
// ============================================================================

/// `FileOperations` (utils.ts:12-16).
///
/// Serde derives serve the harness type layer (`harness::types::FileOperations`,
/// harness/types.ts:862-866), where the same type rides inside event payloads;
/// the sets serialize as JSON arrays.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileOperations {
    pub read: HashSet<String>,
    pub written: HashSet<String>,
    pub edited: HashSet<String>,
}

/// `createFileOps` (utils.ts:18-24).
pub fn create_file_ops() -> FileOperations {
    FileOperations::default()
}

/// Final file lists from file operations (`computeFileLists`, utils.ts:62-67):
/// `readFiles` are files only read, `modifiedFiles` = edited ∪ written, both
/// sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileLists {
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

/// Extract file operations from tool calls in an assistant message
/// (`extractFileOpsFromMessage`, utils.ts:29-56). Only `read`/`write`/`edit`
/// tool calls with a string `path` argument count.
pub fn extract_file_ops_from_message(message: &AgentMessage, file_ops: &mut FileOperations) {
    let AgentMessage::Assistant(assistant) = message else {
        return;
    };
    for block in &assistant.content {
        let rpi_ai::types::AssistantContent::ToolCall(tool_call) = block else {
            continue;
        };
        let Some(path) = tool_call.arguments.get("path").and_then(|v| v.as_str()) else {
            continue;
        };
        match tool_call.name.as_str() {
            "read" => {
                file_ops.read.insert(path.to_owned());
            }
            "write" => {
                file_ops.written.insert(path.to_owned());
            }
            "edit" => {
                file_ops.edited.insert(path.to_owned());
            }
            _ => {}
        }
    }
}

/// `computeFileLists` (utils.ts:62-67).
pub fn compute_file_lists(file_ops: &FileOperations) -> FileLists {
    let mut modified: Vec<String> = file_ops.edited.union(&file_ops.written).cloned().collect();
    modified.sort();
    let mut read_only: Vec<String> = file_ops
        .read
        .iter()
        .filter(|f| !file_ops.edited.contains(*f) && !file_ops.written.contains(*f))
        .cloned()
        .collect();
    read_only.sort();
    FileLists {
        read_files: read_only,
        modified_files: modified,
    }
}

/// Format file operations as XML tags for the summary (`formatFileOperations`,
/// utils.ts:72-82). Empty when both lists are empty.
pub fn format_file_operations(read_files: &[String], modified_files: &[String]) -> String {
    let mut sections: Vec<String> = Vec::new();
    if !read_files.is_empty() {
        sections.push(format!(
            "<read-files>\n{}\n</read-files>",
            read_files.join("\n")
        ));
    }
    if !modified_files.is_empty() {
        sections.push(format!(
            "<modified-files>\n{}\n</modified-files>",
            modified_files.join("\n")
        ));
    }
    if sections.is_empty() {
        return String::new();
    }
    format!("\n\n{}", sections.join("\n\n"))
}

// ============================================================================
// Message Serialization
// ============================================================================

/// Maximum characters for a tool result in serialized summaries
/// (`TOOL_RESULT_MAX_CHARS`, utils.ts:89).
pub const TOOL_RESULT_MAX_CHARS: usize = 2000;

/// `truncateForSummary` (utils.ts:95-99): keep the beginning, append a marker
/// with the number of dropped characters.
pub fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    let len = text.chars().count();
    if len <= max_chars {
        return text.to_owned();
    }
    let truncated_chars = len - max_chars;
    let prefix: String = text.chars().take(max_chars).collect();
    format!("{prefix}\n\n[... {truncated_chars} more characters truncated]")
}

/// Serialize LLM messages to text for summarization (`serializeConversation`,
/// utils.ts:109-150). Call `convert_to_llm` first to handle custom message
/// types. Tool results are truncated to keep the summarization request within
/// reasonable token budgets.
pub fn serialize_conversation(messages: &[Message]) -> String {
    let mut parts: Vec<String> = Vec::new();

    for msg in messages {
        match msg {
            // System messages carry prompt/tool state, not conversation
            // content (#9548) — not serialized.
            Message::System(_) => {}
            Message::User(user) => {
                let content = content_text_user(&user.content, "");
                if !content.is_empty() {
                    parts.push(format!("[User]: {content}"));
                }
            }
            Message::Assistant(assistant) => {
                let mut thinking_parts: Vec<&str> = Vec::new();
                let mut tool_calls: Vec<String> = Vec::new();
                let mut has_text = false;

                for block in &assistant.content {
                    match block {
                        rpi_ai::types::AssistantContent::Thinking(thinking) => {
                            thinking_parts.push(thinking.thinking.as_str());
                        }
                        rpi_ai::types::AssistantContent::ToolCall(tool_call) => {
                            let args_str = tool_call
                                .arguments
                                .iter()
                                .map(|(k, v)| {
                                    // JSON.stringify(v) — compact JSON, key
                                    // order preserved (serde_json
                                    // preserve_order).
                                    let json =
                                        serde_json::to_string(v).unwrap_or_else(|_| "null".into());
                                    format!("{k}={json}")
                                })
                                .collect::<Vec<_>>()
                                .join(", ");
                            tool_calls.push(format!("{}({args_str})", tool_call.name));
                        }
                        rpi_ai::types::AssistantContent::Text(_) => {
                            has_text = true;
                        }
                    }
                }

                if !thinking_parts.is_empty() {
                    parts.push(format!(
                        "[Assistant thinking]: {}",
                        thinking_parts.join("\n")
                    ));
                }
                if has_text {
                    parts.push(format!(
                        "[Assistant]: {}",
                        content_text_assistant(&assistant.content, "\n")
                    ));
                }
                if !tool_calls.is_empty() {
                    parts.push(format!("[Assistant tool calls]: {}", tool_calls.join("; ")));
                }
            }
            Message::ToolResult(tool_result) => {
                let content = content_text_tool_result(&tool_result.content, "");
                if !content.is_empty() {
                    parts.push(format!(
                        "[Tool result]: {}",
                        truncate_for_summary(&content, TOOL_RESULT_MAX_CHARS)
                    ));
                }
            }
        }
    }

    parts.join("\n\n")
}

// ============================================================================
// Summarization System Prompt
// ============================================================================

/// `SUMMARIZATION_SYSTEM_PROMPT` (utils.ts:156-158). Byte-exact; the golden
/// prompt fixtures compare against this string.
pub const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.\n\nDo NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";
