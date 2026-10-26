//! Port of `packages/ai/src/api/github-copilot-headers.ts` @ pi 0.82.1
//! (2efa728).
//!
//! Dynamic Copilot headers (`X-Initiator`, `Openai-Intent`,
//! `Copilot-Vision-Request`) shared by the anthropic-messages and
//! openai-completions adapters when `model.provider == "github-copilot"`.

use std::collections::HashMap;

use crate::types::{Message, ToolResultContent, UserContent, UserContentBlock};

/// `inferCopilotInitiator`: Copilot expects `X-Initiator` to indicate whether
/// the request is user-initiated or agent-initiated (e.g. follow-up after
/// assistant/tool messages).
pub fn infer_copilot_initiator(messages: &[Message]) -> &'static str {
    match messages.last() {
        Some(message) if message.role() != crate::types::Role::User => "agent",
        _ => "user",
    }
}

/// `hasCopilotVisionInput`: Copilot requires the `Copilot-Vision-Request`
/// header when sending images.
pub fn has_copilot_vision_input(messages: &[Message]) -> bool {
    messages.iter().any(|message| match message {
        // System messages carry no image content (#9548).
        Message::System(_) => false,
        Message::User(user) => match &user.content {
            UserContent::Blocks(blocks) => blocks
                .iter()
                .any(|block| matches!(block, UserContentBlock::Image(_))),
            UserContent::Text(_) => false,
        },
        Message::ToolResult(result) => result
            .content
            .iter()
            .any(|block| matches!(block, ToolResultContent::Image(_))),
        Message::Assistant(_) => false,
    })
}

/// `buildCopilotDynamicHeaders`.
pub fn build_copilot_dynamic_headers(
    messages: &[Message],
    has_images: bool,
) -> HashMap<String, String> {
    let mut headers = HashMap::from([
        (
            "X-Initiator".to_owned(),
            infer_copilot_initiator(messages).to_owned(),
        ),
        ("Openai-Intent".to_owned(), "conversation-edits".to_owned()),
    ]);
    if has_images {
        headers.insert("Copilot-Vision-Request".to_owned(), "true".to_owned());
    }
    headers
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::types::Message;

    #[test]
    fn test_infer_copilot_initiator() {
        let user: Message = serde_json::from_value(json!({
            "role": "user", "content": "hi", "timestamp": 0
        }))
        .expect("user");
        let assistant: Message = serde_json::from_value(json!({
            "role": "assistant", "content": [], "api": "anthropic-messages",
            "provider": "p", "model": "m",
            "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0,
                      "totalTokens": 0,
                      "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0,
                               "cacheWrite": 0.0, "total": 0.0}},
            "stopReason": "stop", "timestamp": 0
        }))
        .expect("assistant");
        assert_eq!(infer_copilot_initiator(std::slice::from_ref(&user)), "user");
        assert_eq!(infer_copilot_initiator(&[user, assistant]), "agent");
        assert_eq!(infer_copilot_initiator(&[]), "user");
    }

    #[test]
    fn test_has_copilot_vision_input() {
        let text: Message = serde_json::from_value(json!({
            "role": "user", "content": "hi", "timestamp": 0
        }))
        .expect("text");
        let image: Message = serde_json::from_value(json!({
            "role": "user", "timestamp": 0,
            "content": [{"type": "image", "data": "AAAA", "mimeType": "image/png"}]
        }))
        .expect("image");
        assert!(!has_copilot_vision_input(std::slice::from_ref(&text)));
        assert!(has_copilot_vision_input(&[text, image]));
    }

    #[test]
    fn test_build_copilot_dynamic_headers() {
        let headers = build_copilot_dynamic_headers(&[], true);
        assert_eq!(headers.get("X-Initiator").map(String::as_str), Some("user"));
        assert_eq!(
            headers.get("Openai-Intent").map(String::as_str),
            Some("conversation-edits")
        );
        assert_eq!(
            headers.get("Copilot-Vision-Request").map(String::as_str),
            Some("true")
        );
        let headers = build_copilot_dynamic_headers(&[], false);
        assert!(!headers.contains_key("Copilot-Vision-Request"));
    }
}
