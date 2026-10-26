//! Port of `packages/ai/src/utils/text.ts` @ pi 0.82.1 (2efa728).
//!
//! `contentText` extracts and joins the text blocks of message content.
//!
//! Intentional differences: the upstream parameter is `string | Content[]`;
//! Rust callers hold typed content (`UserContent` / `[AssistantContent]` /
//! `[ToolResultContent]`), so there is one function per content kind. The
//! filter-join semantics are identical.

use crate::types::{
    AssistantContent, SystemContent, SystemMessage, ToolResultContent, UserContent,
    UserContentBlock,
};

/// `contentText` for user/custom message content (`string | (TextContent | ImageContent)[]`).
pub fn content_text_user(content: &UserContent, separator: &str) -> String {
    match content {
        UserContent::Text(text) => text.clone(),
        UserContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                UserContentBlock::Text(text) => Some(text.text.as_str()),
                UserContentBlock::Image(_) => None,
            })
            .collect::<Vec<_>>()
            .join(separator),
    }
}

/// `contentText` for assistant content blocks.
pub fn content_text_assistant(content: &[AssistantContent], separator: &str) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            AssistantContent::Text(text) => Some(text.text.as_str()),
            AssistantContent::Thinking(_) | AssistantContent::ToolCall(_) => None,
        })
        .collect::<Vec<_>>()
        .join(separator)
}

/// `contentText` for tool result content blocks.
pub fn content_text_tool_result(content: &[ToolResultContent], separator: &str) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ToolResultContent::Text(text) => Some(text.text.as_str()),
            ToolResultContent::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join(separator)
}

/// `contentText` for system message content (`string | TextContent[]`,
/// #9548).
pub fn content_text_system(content: &SystemContent, separator: &str) -> String {
    match content {
        SystemContent::Text(text) => text.clone(),
        SystemContent::Blocks(blocks) => blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join(separator),
    }
}

/// `getSystemMessageText` (utils/text.ts:14-21 @ #9548): render a system
/// message as a complete prompt — its content followed by its sections.
pub fn get_system_message_text(message: &SystemMessage) -> String {
    let mut parts = vec![content_text_system(&message.content, "\n")];
    for (_name, value) in message.iter_sections() {
        if let Some(text) = value {
            parts.push(text.to_owned());
        }
    }
    parts
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// `renderSystemMessageUpdate` (utils/text.ts:28-40 @ #9548): render a later
/// system message for APIs that accept system messages mid-conversation.
/// Section changes are framed by name so the model can relate them to the
/// leading prompt. This framing is request-time only and may change between
/// versions.
pub fn render_system_message_update(message: &SystemMessage) -> String {
    let mut parts: Vec<String> = Vec::new();
    let text = content_text_system(&message.content, "\n");
    if !text.is_empty() {
        parts.push(text);
    }
    for (name, value) in message.iter_sections() {
        parts.push(match value {
            None => format!("Removed system prompt section \"{name}\"."),
            Some(value) => {
                format!("Updated system prompt section \"{name}\":\n\n{value}")
            }
        });
    }
    parts.join("\n\n")
}

/// `stripBom` (utils/text.ts:7-9 @ 1355cd36e): strip a leading UTF-8 BOM
/// (`\u{FEFF}`) from a file's text before JSON parsing (#8337).
pub fn strip_bom(content: &str) -> &str {
    content.strip_prefix('\u{FEFF}').unwrap_or(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ImageContent, TextContent, ThinkingContent, ToolCall};

    fn text_block(t: &str) -> UserContentBlock {
        UserContentBlock::Text(TextContent {
            text: t.to_owned(),
            text_signature: None,
        })
    }

    #[test]
    fn test_content_text_user_string_and_blocks() {
        assert_eq!(
            content_text_user(&UserContent::Text("plain".to_owned()), "\n"),
            "plain"
        );
        let blocks = UserContent::Blocks(vec![
            text_block("a"),
            UserContentBlock::Image(ImageContent {
                data: "QUJD".to_owned(),
                mime_type: "image/png".to_owned(),
            }),
            text_block("b"),
        ]);
        assert_eq!(content_text_user(&blocks, "\n"), "a\nb");
        assert_eq!(content_text_user(&blocks, ""), "ab");
    }

    #[test]
    fn test_content_text_assistant_skips_non_text() {
        let content = vec![
            AssistantContent::Thinking(ThinkingContent {
                thinking: "t".to_owned(),
                thinking_signature: None,
                redacted: None,
            }),
            AssistantContent::Text(TextContent {
                text: "x".to_owned(),
                text_signature: None,
            }),
            AssistantContent::ToolCall(ToolCall {
                id: "c".to_owned(),
                name: "read".to_owned(),
                arguments: serde_json::Map::new(),
                thought_signature: None,
                namespace: None,
            }),
            AssistantContent::Text(TextContent {
                text: "y".to_owned(),
                text_signature: None,
            }),
        ];
        assert_eq!(content_text_assistant(&content, "\n"), "x\ny");
        assert_eq!(content_text_assistant(&[], "\n"), "");
    }
}
