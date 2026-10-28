//! Provider quota labels follow reported durations, not primary/secondary position.
//! The account pool fetches these fixtures over HTTP, including credential lookup.
//! Parent account tests own the isolated server helper.

use std::{collections::BTreeMap, sync::Arc};

use axum::{Json, Router, routing::get};
use roost_llm::{AccountPool, CredentialKind, CredentialStore, Endpoints, InMemoryCredentialStore};
use serde_json::json;

#[tokio::test]
async fn codex_weekly_and_short_windows_follow_reported_duration() {
    let base = super::start_server(Router::new().route(
        "/wham/usage",
        get(|| async {
            Json(json!({"rate_limit": {
                "primary_window": {
                    "used_percent": 75,
                    "limit_window_seconds": 604800,
                    "reset_at": 1794000000_i64
                },
                "secondary_window": {
                    "used_percent": 20,
                    "limit_window_seconds": 18000,
                    "reset_at": 1793400000_i64
                }
            }}))
        }),
    ))
    .await;
    let store = Arc::new(InMemoryCredentialStore::default());
    let credential = store
        .upsert(
            "openai-codex",
            CredentialKind::OAuth {
                access: "fixture-token".into(),
                refresh: "fixture-refresh".into(),
                expires_ms: i64::MAX,
                account_id: Some("fixture-account".into()),
                email: None,
            },
            "fixture-account",
            "fixture account",
        )
        .await
        .expect("store fixture credential");
    let pool = AccountPool::new(
        store,
        reqwest::Client::new(),
        Endpoints::with_overrides(BTreeMap::from([("openai-codex".into(), base)])),
    );
    let report = pool
        .refresh_usage(credential, "openai-codex")
        .await
        .expect("fetch quota")
        .expect("credential quota report");
    let windows: Vec<_> = report
        .windows
        .iter()
        .map(|window| {
            (
                window.name.as_str(),
                window.used_fraction,
                window.resets_at_ms,
            )
        })
        .collect();
    assert_eq!(
        windows,
        [
            ("7d", 0.75, Some(1794000000000)),
            ("5h", 0.2, Some(1793400000000)),
        ]
    );
}
