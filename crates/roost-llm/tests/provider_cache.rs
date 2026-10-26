//! Explicit no-cache calls must not acquire provider cache directives.
//! Compaction uses this policy through the same native HTTP provider clients;
//! the fixture server records the actual serialized request, not a mock call.

use reqwest::StatusCode;
use roost_llm::{AssistantBlock, CacheRetention, Catalog, Message, ToolSpec};
use serde_json::{Value, json};

use super::provider_support::{events, request, server};

#[tokio::test]
async fn no_cache_disables_anthropic_breakpoints_and_codex_affinity() {
    let catalog = Catalog::builtin();
    for (provider, model_id, frames) in [
        (
            "anthropic",
            "claude-fable-5",
            include_str!("fixtures/anthropic.sse"),
        ),
        (
            "openai-codex",
            "gpt-5.3-codex-spark",
            include_str!("fixtures/codex.sse"),
        ),
    ] {
        let (url, requests, _) = server(frames, StatusCode::OK).await;
        let mut call = request(
            catalog.get(provider, model_id).unwrap(),
            vec![ToolSpec {
                name: "read".into(),
                description: "Read a file".into(),
                parameters: json!({"type":"object","properties":{"path":{"type":"string"}}}),
            }],
        );
        call.cache_retention = Some(CacheRetention::None);
        call.messages.extend([
            Message::Assistant {
                blocks: vec![AssistantBlock::ToolCall {
                    call_id: "read-1".into(),
                    name: "read".into(),
                    args_json: r#"{"path":"fixture"}"#.into(),
                    provider_data: None,
                }],
                provider_data: None,
            },
            Message::ToolResult {
                call_id: "read-1".into(),
                tool_name: "read".into(),
                text: "bytes".into(),
                is_error: false,
                content: None,
                provider_data: None,
            },
        ]);
        let _ = events(call, &url).await;
        let bodies = requests.lock().await;
        assert!(
            !contains_cache_directive(&bodies[0]),
            "{provider}: {}",
            bodies[0]
        );
    }
}

fn contains_cache_directive(value: &Value) -> bool {
    match value {
        Value::Object(fields) => fields.iter().any(|(key, value)| {
            matches!(
                key.as_str(),
                "cache_control" | "prompt_cache_key" | "prompt_cache_retention"
            ) || contains_cache_directive(value)
        }),
        Value::Array(values) => values.iter().any(contains_cache_directive),
        _ => false,
    }
}
