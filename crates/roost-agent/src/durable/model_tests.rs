//! Deterministic tests for Pi/Roost provider-boundary message preservation.

use roost_llm::{AssistantBlock, Message as LlmMessage, UserContent};
use rpi_ai::types::{
    AssistantContent, Message, ToolResultContent, UserContent as PiUserContent, UserContentBlock,
};

use super::message::{from_durable_message, to_durable_message};

#[test]
fn user_images_keep_order_and_mime_type_across_the_boundary() {
    let message = LlmMessage::User {
        content: vec![
            UserContent::Text {
                text: "before".to_owned(),
            },
            UserContent::Image {
                mime: "image/png".to_owned(),
                base64: "aGVsbG8=".to_owned(),
            },
            UserContent::Text {
                text: "after".to_owned(),
            },
        ],
    };
    let durable = to_durable_message(&message).expect("representable user message");
    let Message::User(user) = &durable else {
        panic!("user role preserved")
    };
    let PiUserContent::Blocks(blocks) = &user.content else {
        panic!("ordered blocks preserved")
    };
    assert!(matches!(&blocks[1], UserContentBlock::Image(image)
        if image.mime_type == "image/png" && image.data == "aGVsbG8="));
    assert_eq!(
        from_durable_message(&durable, true).expect("round trip"),
        message
    );
}

#[test]
fn text_only_conversion_omits_image_bytes_and_preserves_literal_marker_text() {
    let text = "\u{2063}[attached image: literal]\u{2063}".to_owned();
    let original = LlmMessage::User {
        content: vec![
            UserContent::Text { text: text.clone() },
            UserContent::Image {
                mime: "image/png".to_owned(),
                base64: "aGVsbG8=".to_owned(),
            },
        ],
    };
    let durable = to_durable_message(&original).expect("representable user message");
    let LlmMessage::User { content } =
        from_durable_message(&durable, false).expect("text-only projection")
    else {
        panic!("user role preserved")
    };
    assert_eq!(
        content.first(),
        Some(&UserContent::Text { text }),
        "literal marker-delimited text remains untouched"
    );
    assert!(content.iter().any(|block| matches!(
        block,
        UserContent::Text { text }
            if text.contains("this model does not support image input")
    )));
    assert!(
        content
            .iter()
            .all(|block| !matches!(block, UserContent::Image { .. }))
    );
}

#[test]
fn thinking_and_tool_signatures_keep_provider_data() {
    let message = LlmMessage::Assistant {
        blocks: vec![
            AssistantBlock::Text {
                text: "signed".to_owned(),
                provider_data: Some(serde_json::json!({"textSignature": "text-sig"})),
            },
            AssistantBlock::Thinking {
                text: "reasoning".to_owned(),
                provider_data: Some(
                    serde_json::json!({"thinkingSignature": "think-sig", "redacted": true}),
                ),
            },
            AssistantBlock::ToolCall {
                call_id: "call-1".to_owned(),
                name: "lookup".to_owned(),
                args_json: "{\"q\":\"x\"}".to_owned(),
                provider_data: Some(
                    serde_json::json!({"thoughtSignature": "thought-sig", "namespace": "ns"}),
                ),
            },
        ],
        provider_data: None,
    };
    let durable = to_durable_message(&message).expect("representable assistant blocks");
    let Message::Assistant(assistant) = &durable else {
        panic!("assistant role preserved")
    };
    assert!(matches!(&assistant.content[0], AssistantContent::Text(text)
        if text.text_signature.as_deref() == Some("text-sig")));
    assert!(
        matches!(&assistant.content[2], AssistantContent::ToolCall(call)
        if call.thought_signature.as_deref() == Some("thought-sig") && call.namespace.as_deref() == Some("ns"))
    );
    let converted = from_durable_message(&durable, true).expect("round trip");
    let LlmMessage::Assistant { blocks, .. } = converted else {
        panic!("assistant role preserved")
    };
    assert_eq!(
        blocks,
        match message {
            LlmMessage::Assistant { blocks, .. } => blocks,
            _ => Vec::new(),
        }
    );
}

#[test]
fn tool_result_images_and_metadata_keep_order() {
    let message = LlmMessage::ToolResult {
        call_id: "call-2".to_owned(),
        tool_name: "read".to_owned(),
        text: "caption".to_owned(),
        is_error: false,
        content: Some(vec![
            UserContent::Text {
                text: "first".to_owned(),
            },
            UserContent::Image {
                mime: "image/jpeg".to_owned(),
                base64: "YWJj".to_owned(),
            },
            UserContent::Text {
                text: "last".to_owned(),
            },
        ]),
        provider_data: Some(serde_json::json!({"details":{"source":"tool"}})),
    };
    let durable = to_durable_message(&message).expect("representable tool result");
    let Message::ToolResult(result) = &durable else {
        panic!("tool role preserved")
    };
    assert!(matches!(&result.content[1], ToolResultContent::Image(image)
        if image.mime_type == "image/jpeg" && image.data == "YWJj"));
    let LlmMessage::ToolResult {
        content: Some(content),
        provider_data,
        ..
    } = from_durable_message(&durable, true).expect("round trip")
    else {
        panic!("tool result preserved")
    };
    assert_eq!(
        content,
        match message {
            LlmMessage::ToolResult {
                content: Some(content),
                ..
            } => content,
            _ => Vec::new(),
        }
    );
    assert_eq!(
        provider_data.and_then(|value| value.get("details").cloned()),
        Some(serde_json::json!({"source":"tool"}))
    );
}

#[test]
fn text_only_conversion_notices_image_only_tool_results() {
    let durable = to_durable_message(&LlmMessage::ToolResult {
        call_id: "call-image".to_owned(),
        tool_name: "render".to_owned(),
        text: String::new(),
        is_error: false,
        content: Some(vec![UserContent::Image {
            mime: "image/png".to_owned(),
            base64: "aGVsbG8=".to_owned(),
        }]),
        provider_data: None,
    })
    .expect("representable tool result");
    let LlmMessage::ToolResult {
        text,
        content: Some(content),
        ..
    } = from_durable_message(&durable, false).expect("text-only projection")
    else {
        panic!("tool result role preserved")
    };
    assert!(text.contains("this model does not support image input"));
    assert!(
        content
            .iter()
            .all(|block| !matches!(block, UserContent::Image { .. }))
    );
}
