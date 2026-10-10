use reqwest::StatusCode;
use roost_llm::{AssistantBlock, Catalog, Message, StopReason, StreamEvent, ToolSpec};
use serde_json::json;

use super::provider_support::{events, request, server};

#[tokio::test]
async fn recorded_provider_sse_builds_blocks_usage_and_followup_replay_data() {
    let catalog = Catalog::builtin();
    let tool = ToolSpec {
        name: "read".into(),
        description: "read a file".into(),
        parameters: json!({"type":"object","properties":{"path":{"type":"string"}}}),
    };

    let anthropic_model = catalog.get("anthropic", "claude-fable-5").unwrap();
    let (anthropic_url, anthropic_requests, _) =
        server(include_str!("fixtures/anthropic.sse"), StatusCode::OK).await;
    let mut anthropic_request = request(anthropic_model, vec![tool.clone()]);
    anthropic_request
        .messages
        .push(Message::user_text("follow up"));
    let anthropic_events = events(anthropic_request.clone(), &anthropic_url).await;
    assert!(
        anthropic_events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta { text, .. } if text == " world"))
    );
    assert!(anthropic_events.iter().any(|event| matches!(event, StreamEvent::ToolArgsDelta { json, .. } if json == "\"src/lib.rs\"}")));
    assert!(anthropic_events.iter().any(|event| matches!(event, StreamEvent::Usage(usage) if usage.input == 11 && usage.output == 7 && usage.cache_read == 2 && usage.cache_write == 3)));
    let thinking = anthropic_events
        .iter()
        .find_map(|event| match event {
            StreamEvent::BlockEnd {
                block:
                    AssistantBlock::Thinking {
                        provider_data: Some(data),
                        ..
                    },
                ..
            } => Some(data.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(thinking["signature"], "sig-anthropic");
    let redacted = anthropic_events
        .iter()
        .find_map(|event| match event {
            StreamEvent::BlockEnd {
                block:
                    AssistantBlock::Thinking {
                        provider_data: Some(data),
                        ..
                    },
                ..
            } if data.get("redacted_thinking").is_some() => Some(data.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(redacted["redacted_thinking"], "opaque-redacted");
    let starts: Vec<_> = anthropic_events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::BlockStart { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect();
    assert_eq!(
        starts,
        [
            roost_llm::BlockKind::Text,
            roost_llm::BlockKind::Thinking,
            roost_llm::BlockKind::Thinking
        ]
    );
    let blocks: Vec<_> = anthropic_events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::BlockEnd { block, .. } => Some(block.clone()),
            _ => None,
        })
        .collect();
    anthropic_request.messages = vec![
        Message::Assistant { blocks },
        Message::user_text("continue"),
    ];
    let _ = events(anthropic_request, &anthropic_url).await;
    let anthropic_bodies = anthropic_requests.lock().await;
    assert_eq!(anthropic_bodies.len(), 2);
    assert_eq!(
        anthropic_bodies[0]["system"][0]["text"],
        "System instructions"
    );
    assert_eq!(
        anthropic_bodies[0]["messages"][1]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
    assert_eq!(
        anthropic_bodies[1]["messages"][0]["content"][1]["signature"],
        "sig-anthropic"
    );
    assert_eq!(
        anthropic_bodies[1]["messages"][0]["content"][3]["type"],
        "redacted_thinking"
    );
    assert_eq!(
        anthropic_bodies[1]["messages"][0]["content"][3]["data"],
        "opaque-redacted"
    );
    drop(anthropic_bodies);

    let codex_model = catalog.get("openai-codex", "gpt-5.3-codex-spark").unwrap();
    let (codex_url, codex_requests, _) =
        server(include_str!("fixtures/codex.sse"), StatusCode::OK).await;
    let codex_request = request(codex_model, vec![tool.clone()]);
    let codex_events = events(codex_request.clone(), &codex_url).await;
    assert!(codex_events.iter().any(|event| matches!(event, StreamEvent::ToolArgsDelta { json, .. } if json == "\"src/main.rs\"}")));
    assert!(codex_events.iter().any(|event| matches!(event, StreamEvent::Usage(usage) if usage.input == 23 && usage.output == 8 && usage.cache_read == 5)));
    let encrypted = codex_events
        .iter()
        .find_map(|event| match event {
            StreamEvent::BlockEnd {
                block:
                    AssistantBlock::Thinking {
                        provider_data: Some(data),
                        ..
                    },
                ..
            } => Some(data.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(encrypted["encrypted_content"], "opaque-ciphertext");
    let codex_blocks: Vec<_> = codex_events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::BlockEnd { block, .. } => Some(block.clone()),
            _ => None,
        })
        .collect();
    let mut followup = codex_request;
    followup.messages = vec![
        Message::Assistant {
            blocks: codex_blocks,
        },
        Message::user_text("continue"),
    ];
    let _ = events(followup, &codex_url).await;
    let codex_bodies = codex_requests.lock().await;
    assert_eq!(codex_bodies[0]["instructions"], "System instructions");
    assert_eq!(
        codex_bodies[1]["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["type"] == "reasoning")
            .unwrap()["encrypted_content"],
        "opaque-ciphertext"
    );
    drop(codex_bodies);

    let openrouter_model = catalog
        .get("openrouter", "anthropic/claude-fable-5")
        .unwrap();
    let (openrouter_url, openrouter_requests, _) =
        server(include_str!("fixtures/completions.sse"), StatusCode::OK).await;
    let mut completion_model = openrouter_model.clone();
    completion_model.api = roost_llm::WireApi::OpenAiCompletions;
    let completion_events = events(
        request(
            &completion_model,
            vec![ToolSpec {
                name: "grep".into(),
                description: "search".into(),
                parameters: json!({"type":"object"}),
            }],
        ),
        &openrouter_url,
    )
    .await;
    assert!(completion_events.iter().any(
        |event| matches!(event, StreamEvent::ToolArgsDelta { json, .. } if json == "\"needle\"}")
    ));
    assert!(
        completion_events
            .iter()
            .any(|event| matches!(event, StreamEvent::Stop(StopReason::ToolUse)))
    );
    assert!(completion_events.iter().any(|event| matches!(event, StreamEvent::Usage(usage) if usage.input == 17 && usage.output == 9 && usage.cache_read == 4)));
    assert_eq!(
        openrouter_requests.lock().await[0]["reasoning"]["effort"],
        "high"
    );
}
