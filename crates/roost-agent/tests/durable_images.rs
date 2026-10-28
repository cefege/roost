//! Code-mode image output must remain model context after materialized forks.
//! The native runtime executes the script, stores its typed result, and opens
//! a fresh kernel namespace for the fork; no provider credentials are required.

mod support;

use roost_agent::SubmitMode;
use roost_llm::{Message, UserContent};
use roost_protocol::wire::agent_chat::{AgentChatAttachment, TranscriptItem};
use support::{Harness, text_reply, tool_reply, unpinned_conversation, user_content};

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

#[tokio::test]
async fn submitted_images_and_file_paths_reach_the_model_and_transcript() {
    let harness = Harness::new();
    let conversation_id = harness.conversation().await;
    harness.llm.script(
        &conversation_id,
        text_reply("I can see the attached image."),
    );
    let image = AgentChatAttachment {
        filename: "sample.png".to_owned(),
        mime_type: "image/png".to_owned(),
        data: b"image bytes".to_vec(),
        path: String::new(),
    };
    harness
        .runtime
        .submit_request_with_attachments(
            &conversation_id,
            "image-submit",
            "Describe this.".to_owned(),
            vec![image],
            SubmitMode::Steer,
        )
        .await
        .unwrap();
    let transcript = harness.settle(&conversation_id).await;
    let image_requests = harness.llm.requests_for(&conversation_id);
    assert!(image_requests[0].messages.iter().any(|message| {
        matches!(
            message,
            Message::User { content } if content.iter().any(|block| matches!(
                block,
                UserContent::Image { mime, base64 }
                    if mime == "image/png" && base64 == "aW1hZ2UgYnl0ZXM="
            ))
        )
    }));
    assert!(transcript.items.iter().any(|item| {
        matches!(
            item,
            TranscriptItem::User {
                text,
                attachment_names,
                ..
            } if text == "Describe this."
                && attachment_names.len() == 1
                && attachment_names[0] == "sample.png"
        )
    }));

    harness
        .llm
        .script(&conversation_id, text_reply("I can read the file path."));
    harness
        .runtime
        .submit_request_with_attachments(
            &conversation_id,
            "file-submit",
            String::new(),
            vec![AgentChatAttachment {
                filename: "brief.pdf".to_owned(),
                mime_type: "application/pdf".to_owned(),
                data: Vec::new(),
                path: "/repo/brief.pdf".to_owned(),
            }],
            SubmitMode::Steer,
        )
        .await
        .unwrap();
    let file_transcript = harness.settle(&conversation_id).await;
    assert!(file_transcript.items.iter().any(|item| {
        matches!(
            item,
            TranscriptItem::User {
                text,
                attachment_names,
                ..
            } if text == "Please inspect the attached file."
                && attachment_names.len() == 1
                && attachment_names[0] == "brief.pdf"
        )
    }));
    let requests = harness.llm.requests_for(&conversation_id);
    assert!(requests[1].messages.iter().any(|message| {
        matches!(
            message,
            Message::User { content } if content.iter().any(|block| matches!(
                block,
                UserContent::Text { text } if text.contains("/repo/brief.pdf")
            ))
        )
    }));
    harness.llm.script(
        &conversation_id,
        text_reply("The file path survived retry."),
    );
    harness
        .runtime
        .submit_request_with_attachments(
            &conversation_id,
            "file-retry",
            "/retry".to_owned(),
            Vec::new(),
            SubmitMode::Steer,
        )
        .await
        .unwrap();
    harness.settle(&conversation_id).await;
    let requests = harness.llm.requests_for(&conversation_id);
    assert!(
        requests[2].messages.iter().any(|message| {
            matches!(
                message,
                Message::User { content } if content.iter().any(|block| matches!(
                    block,
                    UserContent::Text { text }
                        if text.matches("/repo/brief.pdf").count() == 1
                ))
            )
        }),
        "retry must synthesize the worker path once from the original attachment"
    );
}

#[tokio::test]
async fn vision_fallback_receives_images_after_a_text_only_primary() {
    let harness = Harness::new();
    enable_codex_fallback(&harness);
    let id = unpinned_conversation(&harness.runtime).await;
    configure_image_chain(
        &harness,
        "openai-codex/gpt-5.3-codex-spark, anthropic/claude-sonnet-5",
    )
    .await;
    script_primary_auth_failures(&harness, &id);
    harness.llm.script(&id, text_reply("The image is visible."));
    submit_image(&harness, &id).await;

    let requests = harness.llm.requests_for(&id);
    assert_eq!(requests.len(), 5);
    assert_image_reference(user_content(&requests[0]));
    assert_vision_image(user_content(&requests[4]));
}

#[tokio::test]
async fn text_only_fallback_receives_image_reference_after_a_vision_primary() {
    let harness = Harness::new();
    enable_codex_fallback(&harness);
    let id = unpinned_conversation(&harness.runtime).await;
    configure_image_chain(
        &harness,
        "anthropic/claude-sonnet-5, openai-codex/gpt-5.3-codex-spark",
    )
    .await;
    script_primary_auth_failures(&harness, &id);
    harness
        .llm
        .script(&id, text_reply("The filename is available."));
    submit_image(&harness, &id).await;

    let requests = harness.llm.requests_for(&id);
    assert_eq!(requests.len(), 5);
    assert_vision_image(user_content(&requests[0]));
    assert_image_reference(user_content(&requests[4]));
}

fn assert_image_reference(content: &[UserContent]) {
    assert!(content.iter().any(|block| matches!(
        block,
        UserContent::Text { text } if text.matches("sample.png").count() == 1
    )));
    assert!(
        content
            .iter()
            .all(|block| !matches!(block, UserContent::Image { .. }))
    );
}

fn assert_vision_image(content: &[UserContent]) {
    assert!(content.iter().any(|block| matches!(
        block,
        UserContent::Image { mime, base64 }
            if mime == "image/png" && base64 == "aW1hZ2UgYnl0ZXM="
    )));
}

fn script_primary_auth_failures(harness: &Harness, id: &str) {
    for _ in 0..4 {
        harness
            .llm
            .script(id, Err(roost_llm::LlmError::Auth("primary refused".into())));
    }
}

fn enable_codex_fallback(harness: &Harness) {
    harness
        .llm
        .providers
        .lock()
        .unwrap()
        .push("openai-codex".to_owned());
}

async fn configure_image_chain(harness: &Harness, chain: &str) {
    let mut settings = roost_agent::AgentSettings::default();
    settings
        .model_roles
        .insert(roost_agent::Role::Default, chain.to_owned());
    harness.runtime.set_settings(settings).await.unwrap();
}

async fn submit_image(harness: &Harness, id: &str) {
    harness
        .runtime
        .submit_request_with_attachments(
            id,
            "mixed-image-fallback",
            "Describe this.".to_owned(),
            vec![AgentChatAttachment {
                filename: "sample.png".to_owned(),
                mime_type: "image/png".to_owned(),
                data: b"image bytes".to_vec(),
                path: String::new(),
            }],
            SubmitMode::Steer,
        )
        .await
        .unwrap();
    harness.settle(id).await;
}

#[tokio::test]
async fn slash_retry_resubmits_the_original_image_bytes() {
    let harness = Harness::new();
    let conversation_id = harness.conversation().await;
    let image = AgentChatAttachment {
        filename: "retry.png".to_owned(),
        mime_type: "image/png".to_owned(),
        data: b"retry image bytes".to_vec(),
        path: String::new(),
    };
    harness
        .llm
        .script(&conversation_id, text_reply("First answer."));
    harness
        .runtime
        .submit_request_with_attachments(
            &conversation_id,
            "original-image",
            "Describe the original.".to_owned(),
            vec![image],
            SubmitMode::Steer,
        )
        .await
        .unwrap();
    harness.settle(&conversation_id).await;

    harness
        .llm
        .script(&conversation_id, text_reply("Retried answer."));
    harness
        .runtime
        .submit_request_with_attachments(
            &conversation_id,
            "retry-command",
            "/retry".to_owned(),
            Vec::new(),
            SubmitMode::Steer,
        )
        .await
        .unwrap();
    harness.settle(&conversation_id).await;

    let requests = harness.llm.requests_for(&conversation_id);
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1].messages.iter().any(|message| {
            matches!(
                message,
                Message::User { content } if content.iter().any(|block| matches!(
                    block,
                    UserContent::Image { mime, base64 }
                        if mime == "image/png" && base64 == "cmV0cnkgaW1hZ2UgYnl0ZXM="
                ))
            )
        }),
        "retry must dispatch the original image payload to the model"
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
