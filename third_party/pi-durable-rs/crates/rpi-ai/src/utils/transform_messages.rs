//! Port of `packages/ai/src/api/transform-messages.ts` @ pi 0.82.1 (2efa728),
//! kept in `utils/` per design §3.6.
//!
//! Cross-provider handoff: non-vision image placeholders, thinking-block
//! conversion, tool-call id normalization with toolResult back-fill, orphan
//! tool-call synthetic results, error/aborted assistant messages skipped.
//!
//! The upstream null/undefined content normalization
//! (transform-messages.ts:71-73) has no runtime step here: Rust message types
//! cannot hold null, so `content: null` is tolerated as empty at the serde
//! boundary instead (`types.rs::null_default`).

use std::collections::{HashMap, HashSet};

use crate::types::{
    AssistantContent, AssistantMessage, Message, Model, StopReason, ToolResultContent,
    ToolResultMessage, ToolResultRole, UserContent, UserContentBlock,
};

pub const NON_VISION_USER_IMAGE_PLACEHOLDER: &str =
    "(image omitted: model does not support images)";
pub const NON_VISION_TOOL_IMAGE_PLACEHOLDER: &str =
    "(tool image omitted: model does not support images)";

/// Current unix time in milliseconds (`Date.now()`).
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn replace_images_with_placeholder_user(
    content: &[UserContentBlock],
    placeholder: &str,
) -> Vec<UserContentBlock> {
    let mut result = Vec::new();
    let mut previous_was_placeholder = false;

    for block in content {
        match block {
            UserContentBlock::Image(_) => {
                if !previous_was_placeholder {
                    result.push(UserContentBlock::Text(crate::types::TextContent {
                        text: placeholder.to_owned(),
                        text_signature: None,
                    }));
                }
                previous_was_placeholder = true;
            }
            UserContentBlock::Text(text) => {
                result.push(block.clone());
                previous_was_placeholder = text.text == placeholder;
            }
        }
    }

    result
}

fn replace_images_with_placeholder_tool_result(
    content: &[ToolResultContent],
    placeholder: &str,
) -> Vec<ToolResultContent> {
    let mut result = Vec::new();
    let mut previous_was_placeholder = false;

    for block in content {
        match block {
            ToolResultContent::Image(_) => {
                if !previous_was_placeholder {
                    result.push(ToolResultContent::Text(crate::types::TextContent {
                        text: placeholder.to_owned(),
                        text_signature: None,
                    }));
                }
                previous_was_placeholder = true;
            }
            ToolResultContent::Text(text) => {
                result.push(block.clone());
                previous_was_placeholder = text.text == placeholder;
            }
        }
    }

    result
}

fn downgrade_unsupported_images(messages: Vec<Message>, model: &Model) -> Vec<Message> {
    if model.input.contains(&crate::types::InputModality::Image) {
        return messages;
    }

    messages
        .into_iter()
        .map(|msg| match msg {
            Message::User(mut user) => {
                if let UserContent::Blocks(blocks) = &user.content {
                    user.content = UserContent::Blocks(replace_images_with_placeholder_user(
                        blocks,
                        NON_VISION_USER_IMAGE_PLACEHOLDER,
                    ));
                }
                Message::User(user)
            }
            Message::ToolResult(mut result) => {
                result.content = replace_images_with_placeholder_tool_result(
                    &result.content,
                    NON_VISION_TOOL_IMAGE_PLACEHOLDER,
                );
                Message::ToolResult(result)
            }
            other => other,
        })
        .collect()
}

/// The `normalizeToolCallId` callback type: `(id, targetModel, sourceMessage)`.
pub type NormalizeToolCallId = dyn FnMut(&str, &Model, &AssistantMessage) -> String;

/// `transformMessages` — normalize tool call IDs for cross-provider
/// compatibility and synthesize results for orphaned tool calls.
///
/// `normalize_tool_call_id` mirrors the upstream callback; it is only invoked
/// for cross-model tool calls.
#[allow(unused_assignments)] // faithful port: the trailing reset mirrors upstream's insertSyntheticToolResults
pub fn transform_messages(
    messages: &[Message],
    model: &Model,
    mut normalize_tool_call_id: Option<&mut NormalizeToolCallId>,
) -> Vec<Message> {
    // Map of original tool call IDs to normalized IDs.
    let mut tool_call_id_map: HashMap<String, String> = HashMap::new();
    let image_aware_messages = downgrade_unsupported_images(messages.to_vec(), model);

    // First pass: transform messages (unsupported image downgrade, thinking
    // blocks, tool call ID normalization).
    let transformed: Vec<Message> = image_aware_messages
        .into_iter()
        .map(|msg| {
            match msg {
                // System and user messages pass through unchanged (#9548:
                // transform-messages.ts:78-80).
                Message::System(_) | Message::User(_) => msg,

                // ToolResult: normalize toolCallId if we have a mapping.
                Message::ToolResult(mut result) => {
                    if let Some(normalized_id) = tool_call_id_map.get(&result.tool_call_id)
                        && *normalized_id != result.tool_call_id
                    {
                        result.tool_call_id = normalized_id.clone();
                    }
                    Message::ToolResult(result)
                }

                Message::Assistant(assistant_msg) => {
                    let is_same_model = assistant_msg.provider == model.provider
                        && assistant_msg.api == model.api
                        && assistant_msg.model == model.id;

                    let mut transformed_content: Vec<AssistantContent> = Vec::new();
                    for block in assistant_msg.content.iter() {
                        match block {
                            AssistantContent::Thinking(thinking) => {
                                // Redacted thinking is opaque encrypted content,
                                // only valid for the same model.
                                if thinking.redacted.unwrap_or(false) {
                                    if is_same_model {
                                        transformed_content.push(block.clone());
                                    }
                                    continue;
                                }
                                // Same model: keep thinking blocks with signatures
                                // (needed for replay) even when the thinking text
                                // is empty (OpenAI encrypted reasoning).
                                if is_same_model && thinking.thinking_signature.is_some() {
                                    transformed_content.push(block.clone());
                                    continue;
                                }
                                // Skip empty thinking blocks, convert others to text.
                                if thinking.thinking.trim().is_empty() {
                                    continue;
                                }
                                if is_same_model {
                                    transformed_content.push(block.clone());
                                } else {
                                    transformed_content.push(AssistantContent::Text(
                                        crate::types::TextContent {
                                            text: thinking.thinking.clone(),
                                            text_signature: None,
                                        },
                                    ));
                                }
                            }
                            AssistantContent::Text(text) => {
                                if is_same_model {
                                    transformed_content.push(block.clone());
                                } else {
                                    transformed_content.push(AssistantContent::Text(
                                        crate::types::TextContent {
                                            text: text.text.clone(),
                                            text_signature: None,
                                        },
                                    ));
                                }
                            }
                            AssistantContent::ToolCall(tool_call) => {
                                let mut normalized_tool_call = tool_call.clone();

                                if !is_same_model
                                    && normalized_tool_call.thought_signature.is_some()
                                {
                                    normalized_tool_call.thought_signature = None;
                                }

                                if !is_same_model
                                    && let Some(normalize) = normalize_tool_call_id.as_deref_mut()
                                {
                                    let normalized_id =
                                        normalize(&tool_call.id, model, &assistant_msg);
                                    if normalized_id != tool_call.id {
                                        tool_call_id_map
                                            .insert(tool_call.id.clone(), normalized_id.clone());
                                        normalized_tool_call.id = normalized_id;
                                    }
                                }

                                transformed_content
                                    .push(AssistantContent::ToolCall(normalized_tool_call));
                            }
                        }
                    }

                    Message::Assistant(AssistantMessage {
                        content: transformed_content,
                        ..assistant_msg
                    })
                }
            }
        })
        .collect();

    // Second pass: insert synthetic empty tool results for orphaned tool
    // calls (preserves thinking signatures and satisfies API requirements).
    let mut result: Vec<Message> = Vec::new();
    let mut pending_tool_calls: Vec<crate::types::ToolCall> = Vec::new();
    let mut existing_tool_result_ids: HashSet<String> = HashSet::new();
    // System messages are transparent to tool-call accounting: one that
    // lands between a tool call and its results is held back and emitted
    // after the results (synthetic ones included), so it never causes a
    // duplicate result for a call that is answered later (#9548,
    // transform-messages.ts:163-186).
    let mut held_system_messages: Vec<Message> = Vec::new();

    macro_rules! insert_synthetic_tool_results {
        () => {
            if !pending_tool_calls.is_empty() {
                for tc in pending_tool_calls.drain(..) {
                    if !existing_tool_result_ids.contains(&tc.id) {
                        result.push(Message::ToolResult(ToolResultMessage {
                            role: ToolResultRole::ToolResult,
                            tool_call_id: tc.id,
                            tool_name: tc.name,
                            content: vec![ToolResultContent::Text(crate::types::TextContent {
                                text: "No result provided".to_owned(),
                                text_signature: None,
                            })],
                            details: None,
                            usage: None,
                            nested_calls: None,
                            is_error: true,
                            timestamp: now_ms(),
                            duration_ms: None,
                        }));
                    }
                }
                existing_tool_result_ids = HashSet::new();
            }
            // System messages are transparent to tool-call accounting: one
            // that lands between a tool call and its results is held back
            // and emitted after the results (synthetic ones included), so it
            // never causes a duplicate result for a call that is answered
            // later (#9548, transform-messages.ts:163-186).
            result.append(&mut held_system_messages);
        };
    }

    for msg in transformed {
        match &msg {
            Message::Assistant(assistant_msg) => {
                // Pending orphaned tool calls from a previous assistant:
                // insert synthetic results now.
                insert_synthetic_tool_results!();

                // Skip errored/aborted assistant messages entirely: incomplete
                // turns must not be replayed (partial content, OpenAI
                // "reasoning without following item" errors).
                if assistant_msg.stop_reason == StopReason::Error
                    || assistant_msg.stop_reason == StopReason::Aborted
                {
                    continue;
                }

                let tool_calls: Vec<crate::types::ToolCall> = assistant_msg
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContent::ToolCall(call) => Some(call.clone()),
                        _ => None,
                    })
                    .collect();
                if !tool_calls.is_empty() {
                    pending_tool_calls = tool_calls;
                    existing_tool_result_ids = HashSet::new();
                }

                result.push(msg);
            }
            Message::ToolResult(result_msg) => {
                existing_tool_result_ids.insert(result_msg.tool_call_id.clone());
                result.push(msg);
            }
            Message::System(_) => {
                if !pending_tool_calls.is_empty() {
                    held_system_messages.push(msg);
                } else {
                    result.push(msg);
                }
            }
            Message::User(_) => {
                // A new user turn interrupts tool flow: insert synthetic
                // results for orphaned calls.
                insert_synthetic_tool_results!();
                result.push(msg);
            }
        }
    }

    // Conversation ends with unresolved tool calls: synthesize results now.
    insert_synthetic_tool_results!();

    result
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, json};

    use super::*;
    use crate::types::{
        ApiKind, AssistantRole, ImageContent, TextContent, ThinkingContent, ToolCall, Usage,
        UserMessage, UserRole,
    };

    fn model(id: &str, provider: &str, input: &[&str]) -> Model {
        serde_json::from_value(json!({
            "id": id, "name": id, "api": "anthropic-messages", "provider": provider,
            "baseUrl": "https://example.com", "reasoning": false,
            "input": input,
            "cost": {"input": 1.0, "output": 1.0, "cacheRead": 0.1, "cacheWrite": 1.0},
            "contextWindow": 1000, "maxTokens": 100
        }))
        .expect("model")
    }

    fn assistant(
        provider: &str,
        model: &str,
        stop_reason: StopReason,
        content: Vec<AssistantContent>,
    ) -> Message {
        Message::Assistant(AssistantMessage {
            role: AssistantRole::Assistant,
            content,
            api: ApiKind::from("anthropic-messages"),
            provider: provider.to_owned(),
            model: model.to_owned(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason,
            error_message: None,
            timestamp: 1,
            deferred: None,
            end_turn: None,
            raw_stop_reason: None,
            duration_ms: None,
        })
    }

    fn tool_call(id: &str, name: &str) -> AssistantContent {
        AssistantContent::ToolCall(ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            arguments: Map::new(),
            thought_signature: None,
            namespace: None,
        })
    }

    fn tool_result(id: &str, name: &str) -> Message {
        Message::ToolResult(ToolResultMessage {
            role: ToolResultRole::ToolResult,
            tool_call_id: id.to_owned(),
            tool_name: name.to_owned(),
            content: vec![ToolResultContent::Text(TextContent {
                text: "ok".to_owned(),
                text_signature: None,
            })],
            details: None,
            usage: None,
            nested_calls: None,
            is_error: false,
            timestamp: 2,
            duration_ms: None,
        })
    }

    #[test]
    fn test_orphan_tool_call_synthesizes_error_result() {
        let messages = vec![assistant(
            "anthropic",
            "m",
            StopReason::ToolUse,
            vec![tool_call("c1", "read")],
        )];
        let out = transform_messages(&messages, &model("m", "anthropic", &["text"]), None);
        assert_eq!(out.len(), 2);
        match &out[1] {
            Message::ToolResult(result) => {
                assert_eq!(result.tool_call_id, "c1");
                assert!(result.is_error);
                assert_eq!(
                    result.content,
                    vec![ToolResultContent::Text(TextContent {
                        text: "No result provided".to_owned(),
                        text_signature: None
                    })]
                );
            }
            other => panic!("expected tool result, got {other:?}"),
        }
    }

    #[test]
    fn test_orphan_tool_call_before_user_message() {
        let messages = vec![
            assistant(
                "anthropic",
                "m",
                StopReason::ToolUse,
                vec![tool_call("c1", "read")],
            ),
            Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("next".to_owned()),
                timestamp: 3,
            }),
        ];
        let out = transform_messages(&messages, &model("m", "anthropic", &["text"]), None);
        assert_eq!(out.len(), 3);
        assert!(matches!(out[1], Message::ToolResult(_)));
        assert!(matches!(out[2], Message::User(_)));
    }

    #[test]
    fn test_error_and_aborted_messages_not_replayed() {
        let messages = vec![
            assistant(
                "anthropic",
                "m",
                StopReason::Error,
                vec![tool_call("c1", "read")],
            ),
            assistant("anthropic", "m", StopReason::Aborted, vec![]),
            Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("hi".to_owned()),
                timestamp: 3,
            }),
        ];
        let out = transform_messages(&messages, &model("m", "anthropic", &["text"]), None);
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], Message::User(_)));
    }

    #[test]
    fn test_resolved_tool_calls_untouched() {
        let messages = vec![
            assistant(
                "anthropic",
                "m",
                StopReason::ToolUse,
                vec![tool_call("c1", "read")],
            ),
            tool_result("c1", "read"),
        ];
        let out = transform_messages(&messages, &model("m", "anthropic", &["text"]), None);
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0], Message::Assistant(_)));
        assert!(matches!(&out[1], Message::ToolResult(r) if !r.is_error));
    }

    #[test]
    fn test_non_vision_image_placeholder() {
        let image = UserContentBlock::Image(ImageContent {
            data: "AAAA".to_owned(),
            mime_type: "image/png".to_owned(),
        });
        let messages = vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![
                image.clone(),
                image,
                UserContentBlock::Text(TextContent {
                    text: "look".to_owned(),
                    text_signature: None,
                }),
            ]),
            timestamp: 1,
        })];
        let out = transform_messages(&messages, &model("m", "anthropic", &["text"]), None);
        match &out[0] {
            Message::User(user) => match &user.content {
                UserContent::Blocks(blocks) => {
                    // Consecutive images collapse to one placeholder.
                    assert_eq!(blocks.len(), 2);
                    assert_eq!(
                        blocks[0],
                        UserContentBlock::Text(TextContent {
                            text: NON_VISION_USER_IMAGE_PLACEHOLDER.to_owned(),
                            text_signature: None
                        })
                    );
                }
                other => panic!("expected blocks, got {other:?}"),
            },
            other => panic!("expected user, got {other:?}"),
        }
        // Vision models pass images through.
        let out = transform_messages(
            &messages,
            &model("m", "anthropic", &["text", "image"]),
            None,
        );
        match &out[0] {
            Message::User(user) => match &user.content {
                UserContent::Blocks(blocks) => assert_eq!(blocks.len(), 3),
                other => panic!("expected blocks, got {other:?}"),
            },
            other => panic!("expected user, got {other:?}"),
        }
    }

    #[test]
    fn test_cross_model_thinking_to_text() {
        let thinking = AssistantContent::Thinking(ThinkingContent {
            thinking: "deep thought".to_owned(),
            thinking_signature: Some("sig".to_owned()),
            redacted: None,
        });
        let messages = vec![assistant(
            "anthropic",
            "m",
            StopReason::Stop,
            vec![thinking],
        )];
        // Same model: kept (signature present).
        let out = transform_messages(&messages, &model("m", "anthropic", &["text"]), None);
        match &out[0] {
            Message::Assistant(a) => assert!(matches!(a.content[0], AssistantContent::Thinking(_))),
            other => panic!("expected assistant, got {other:?}"),
        }
        // Cross model: converted to plain text.
        let out = transform_messages(&messages, &model("other", "openai", &["text"]), None);
        match &out[0] {
            Message::Assistant(a) => assert_eq!(
                a.content[0],
                AssistantContent::Text(TextContent {
                    text: "deep thought".to_owned(),
                    text_signature: None
                })
            ),
            other => panic!("expected assistant, got {other:?}"),
        }
    }

    #[test]
    fn test_cross_model_tool_call_id_normalization_backfills_results() {
        let mut args = Map::new();
        args.insert("x".to_owned(), json!(1));
        let messages = vec![
            assistant(
                "openai",
                "gpt",
                StopReason::ToolUse,
                vec![AssistantContent::ToolCall(ToolCall {
                    id: "call_long|item_id_with|pipes".to_owned(),
                    name: "read".to_owned(),
                    arguments: args,
                    thought_signature: Some("ts".to_owned()),
                    namespace: None,
                })],
            ),
            tool_result("call_long|item_id_with|pipes", "read"),
        ];
        let mut normalize = |id: &str, _model: &Model, _source: &AssistantMessage| -> String {
            id.replace('|', "_")
        };
        let out = transform_messages(
            &messages,
            &model("m", "anthropic", &["text"]),
            Some(&mut normalize),
        );
        match &out[0] {
            Message::Assistant(a) => match &a.content[0] {
                AssistantContent::ToolCall(call) => {
                    assert_eq!(call.id, "call_long_item_id_with_pipes");
                    // thoughtSignature is dropped cross-model.
                    assert_eq!(call.thought_signature, None);
                }
                other => panic!("expected tool call, got {other:?}"),
            },
            other => panic!("expected assistant, got {other:?}"),
        }
        match &out[1] {
            Message::ToolResult(result) => {
                assert_eq!(result.tool_call_id, "call_long_item_id_with_pipes")
            }
            other => panic!("expected tool result, got {other:?}"),
        }
    }

    #[test]
    fn test_redacted_thinking_dropped_cross_model() {
        let redacted = AssistantContent::Thinking(ThinkingContent {
            thinking: String::new(),
            thinking_signature: Some("encrypted".to_owned()),
            redacted: Some(true),
        });
        let messages = vec![assistant(
            "anthropic",
            "m",
            StopReason::Stop,
            vec![redacted],
        )];
        let out = transform_messages(&messages, &model("other", "openai", &["text"]), None);
        match &out[0] {
            Message::Assistant(a) => assert!(a.content.is_empty()),
            other => panic!("expected assistant, got {other:?}"),
        }
    }

    #[test]
    fn test_synthetic_results_only_for_still_missing_tool_calls() {
        // Copilot test intent ("adds synthetic results only for trailing tool
        // calls that are still missing results"): of two trailing calls only
        // the unresolved one gets a synthetic error result; the resolved
        // result is back-filled to its normalized id.
        let messages = vec![
            assistant(
                "openai",
                "gpt",
                StopReason::ToolUse,
                vec![
                    tool_call("call_1|fc_1", "read"),
                    tool_call("call_2|fc_2", "bash"),
                ],
            ),
            tool_result("call_1|fc_1", "read"),
        ];
        let mut normalize = |id: &str, _model: &Model, _source: &AssistantMessage| -> String {
            id.replace('|', "_")
        };
        let out = transform_messages(
            &messages,
            &model("m", "anthropic", &["text"]),
            Some(&mut normalize),
        );
        assert_eq!(out.len(), 3);
        match &out[1] {
            Message::ToolResult(result) => {
                assert_eq!(result.tool_call_id, "call_1_fc_1");
                assert!(!result.is_error);
            }
            other => panic!("expected tool result, got {other:?}"),
        }
        match &out[2] {
            Message::ToolResult(result) => {
                assert_eq!(result.tool_call_id, "call_2_fc_2");
                assert_eq!(result.tool_name, "bash");
                assert!(result.is_error);
                assert_eq!(
                    result.content,
                    vec![ToolResultContent::Text(TextContent {
                        text: "No result provided".to_owned(),
                        text_signature: None
                    })]
                );
            }
            other => panic!("expected tool result, got {other:?}"),
        }
    }
}
