//! Code-mode image output must remain model context after materialized forks.
//! The native runtime executes the script, stores its typed result, and opens
//! a fresh kernel namespace for the fork; no provider credentials are required.

mod support;

use roost_llm::{Message, UserContent};
use support::{Harness, text_reply, tool_reply};

const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/W5kAAAAASUVORK5CYII=";

#[tokio::test]
async fn code_mode_image_and_surrounding_text_survive_native_fork_rehydration() {
    let harness = Harness::new();
    let source_id = harness.conversation().await;
    let script = serde_json::json!({"code": format!(
        "text('before image'); image('data:image/png;base64,{PNG}'); text('after image');"
    )})
    .to_string();
    harness.llm.script(
        &source_id,
        tool_reply(&[("show-image", "codemode", &script)]),
    );
    harness
        .llm
        .script(&source_id, text_reply("Image reviewed."));
    harness
        .submit(&source_id, "Produce and review the image.")
        .await;
    harness.settle(&source_id).await;
    let original_requests = harness.llm.requests_for(&source_id);
    let original = image_result(&original_requests[1].messages);
    let image_index = original
        .iter()
        .position(|content| {
            matches!(content,
                UserContent::Image { mime, base64 } if mime == "image/png" && base64 == PNG
            )
        })
        .expect("the actual tool result carries the original image bytes");
    assert!(
        original[..image_index]
            .iter()
            .any(|content| matches!(content,
                UserContent::Text { text } if text.contains("before image")
            ))
    );
    assert!(
        original[image_index + 1..]
            .iter()
            .any(|content| matches!(content,
                UserContent::Text { text } if text.contains("after image")
            ))
    );

    let fork = harness.runtime.fork(&source_id, None).await.unwrap();
    harness
        .llm
        .script(&fork.id, text_reply("Fork retained the image."));
    harness
        .submit(&fork.id, "Continue reviewing that image.")
        .await;
    harness.settle(&fork.id).await;
    let fork_requests = harness.llm.requests_for(&fork.id);
    assert_eq!(
        image_result(&fork_requests[0].messages),
        original,
        "fork import retains the typed image and its position, not just display text"
    );
}

fn image_result(messages: &[Message]) -> &[UserContent] {
    messages
        .iter()
        .find_map(|message| match message {
            Message::ToolResult {
                call_id,
                is_error: false,
                content: Some(content),
                ..
            } if call_id == "show-image" => Some(content.as_slice()),
            _ => None,
        })
        .expect("a successful image-producing tool result reaches the model")
}
