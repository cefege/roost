use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use futures::StreamExt;
use reqwest::StatusCode;
use roost_llm::{Catalog, Endpoints, ResolvedAuth, stream_chat};
use tokio_util::sync::CancellationToken;

use super::provider_support::{auth, request, server};

#[tokio::test]
async fn anthropic_oauth_identity_and_codex_account_headers_are_sent() {
    let catalog = Catalog::builtin();
    let (anthropic_url, anthropic_requests, anthropic_headers) =
        server(include_str!("fixtures/anthropic.sse"), StatusCode::OK).await;
    let mut overrides = BTreeMap::new();
    overrides.insert("anthropic".into(), anthropic_url);
    let _ = stream_chat(
        &reqwest::Client::new(),
        &Endpoints::with_overrides(overrides),
        request(catalog.get("anthropic", "claude-fable-5").unwrap(), vec![]),
        ResolvedAuth::OAuth {
            access_token: "sk-ant-oat-test".into(),
            account_id: None,
        },
        CancellationToken::new(),
        None,
    )
    .collect::<Vec<_>>()
    .await;
    let body = anthropic_requests.lock().await;
    assert_eq!(
        body[0]["system"][0]["text"],
        "You are Claude Code, Anthropic's official CLI for Claude."
    );
    drop(body);
    let headers = anthropic_headers.lock().await;
    assert_eq!(
        headers[0].get("anthropic-beta").unwrap(),
        "claude-code-20250219,oauth-2025-04-20"
    );
    assert_eq!(headers[0].get("user-agent").unwrap(), "claude-cli/2.1.280");
    assert_eq!(headers[0].get("x-app").unwrap(), "cli");

    let (codex_url, codex_requests, codex_headers) =
        server(include_str!("fixtures/codex.sse"), StatusCode::OK).await;
    let mut overrides = BTreeMap::new();
    overrides.insert("openai-codex".into(), codex_url);
    let _ = stream_chat(
        &reqwest::Client::new(),
        &Endpoints::with_overrides(overrides),
        request(
            catalog.get("openai-codex", "gpt-5.3-codex-spark").unwrap(),
            vec![],
        ),
        ResolvedAuth::OAuth {
            access_token: "token".into(),
            account_id: Some("acct-123".into()),
        },
        CancellationToken::new(),
        None,
    )
    .collect::<Vec<_>>()
    .await;
    assert_eq!(
        codex_requests.lock().await[0]["model"],
        "gpt-5.3-codex-spark"
    );
    assert_eq!(
        codex_headers.lock().await[0]
            .get("chatgpt-account-id")
            .unwrap(),
        "acct-123"
    );

    let (openrouter_url, _, openrouter_headers) =
        server(include_str!("fixtures/anthropic.sse"), StatusCode::OK).await;
    let mut overrides = BTreeMap::new();
    overrides.insert("openrouter".into(), openrouter_url);
    let _ = stream_chat(
        &reqwest::Client::new(),
        &Endpoints::with_overrides(overrides),
        request(
            catalog
                .get("openrouter", "anthropic/claude-fable-5")
                .unwrap(),
            vec![],
        ),
        auth(),
        CancellationToken::new(),
        None,
    )
    .collect::<Vec<_>>()
    .await;
    let headers = openrouter_headers.lock().await;
    assert_eq!(headers[0].get("authorization").unwrap(), "Bearer test-key");
    assert!(!headers[0].contains_key("x-api-key"));
}

#[tokio::test]
async fn rate_limit_error_parses_retry_and_anthropic_reset_headers_and_observes_headers() {
    let (endpoint, _, _) = server(
        "{\"error\":\"too many requests\"}",
        StatusCode::TOO_MANY_REQUESTS,
    )
    .await;
    let mut overrides = BTreeMap::new();
    overrides.insert("anthropic".into(), endpoint);
    let observed = Arc::new(AtomicBool::new(false));
    let observed_headers = observed.clone();
    let observer = Arc::new(move |headers: &reqwest::header::HeaderMap| {
        observed_headers.store(headers.contains_key("retry-after"), Ordering::Relaxed);
    });
    let result = stream_chat(
        &reqwest::Client::new(),
        &Endpoints::with_overrides(overrides),
        request(
            Catalog::builtin()
                .get("anthropic", "claude-fable-5")
                .unwrap(),
            vec![],
        ),
        auth(),
        CancellationToken::new(),
        Some(observer),
    )
    .next()
    .await
    .unwrap();
    match result.unwrap_err() {
        roost_llm::LlmError::RateLimited {
            retry_after_ms,
            reset_at_ms,
        } => {
            assert_eq!(retry_after_ms, Some(2000));
            assert!(reset_at_ms.is_some());
        }
        error => panic!("unexpected error: {error}"),
    }
    assert!(observed.load(Ordering::Relaxed));
}

#[tokio::test]
async fn cancelled_request_reports_cancelled() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let result = stream_chat(
        &reqwest::Client::new(),
        &Endpoints::production(),
        request(
            Catalog::builtin()
                .get("anthropic", "claude-fable-5")
                .unwrap(),
            vec![],
        ),
        auth(),
        cancel,
        None,
    )
    .next()
    .await
    .unwrap();
    assert_eq!(result.unwrap_err(), roost_llm::LlmError::Cancelled);
}
