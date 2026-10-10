//! Behavior tests for account selection, OAuth input parsing, and judgment retries.
//! Local HTTP handlers keep provider traffic deterministic and exercise the real clients.

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use roost_llm::{
    AccountPool, Answer, CredentialKind, CredentialStore, Endpoints, InMemoryCredentialStore,
    Judge, ModelInfo, ModelKind, Question, WireApi, oauth::parse_authorization_input,
};
use serde_json::{Value, json};
use tokio::net::TcpListener;

async fn start_server(router: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{address}")
}

#[tokio::test]
async fn pool_skips_blocked_credential_and_returns_earliest_unblock() {
    let store = Arc::new(InMemoryCredentialStore::default());
    let first = store
        .upsert(
            "typesafe",
            CredentialKind::ApiKey {
                key: "first".into(),
            },
            "one",
            "one",
        )
        .await;
    let second = store
        .upsert(
            "typesafe",
            CredentialKind::ApiKey {
                key: "second".into(),
            },
            "two",
            "two",
        )
        .await;
    let pool = AccountPool::new(
        store.clone(),
        reqwest::Client::new(),
        Endpoints::production(),
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let reset = now + 120_000;
    store.set_block(first, reset + 20_000).await;
    let (selected, auth) = pool.resolve("typesafe", "conversation").await.unwrap();
    assert_eq!(selected, second);
    assert_eq!(auth.secret(), "second");
    store.set_block(first, now - 1).await;
    let (sticky_id, _) = pool.resolve("typesafe", "conversation").await.unwrap();
    assert_eq!(sticky_id, second);
    store.set_block(first, reset + 20_000).await;
    store.set_block(second, reset).await;
    let error = pool
        .rotate_after_rate_limit(
            first,
            "typesafe",
            "conversation",
            &roost_llm::LlmError::RateLimited {
                retry_after_ms: Some(60_000),
                reset_at_ms: None,
            },
        )
        .await
        .unwrap_err();
    let roost_llm::LlmError::RateLimited { reset_at_ms, .. } = error else {
        panic!("expected aggregate rate limit")
    };
    assert!(reset_at_ms.is_some_and(|until| until < reset));
}

#[test]
fn anthropic_paste_parser_accepts_bare_and_redirect_values() {
    assert_eq!(
        parse_authorization_input("plain-code").unwrap(),
        ("plain-code".into(), None)
    );
    assert_eq!(
        parse_authorization_input(
            "https://platform.claude.com/oauth/code/callback?code=abc%23def&state=xyz"
        )
        .unwrap(),
        ("abc#def".into(), Some("xyz".into()))
    );
}

async fn judgment(
    State(count): State<Arc<AtomicUsize>>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    assert_eq!(body["questions"]["finished"]["type"], "noul");
    assert_eq!(
        body["questions"]["finished"]["criteria"]["true"],
        "the task is done"
    );
    assert_eq!(body["model"], "jev-latest");
    if count.fetch_add(1, Ordering::SeqCst) == 0 {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"busy"})),
        );
    }
    (
        StatusCode::OK,
        Json(json!({"model":"jev-latest","answers":{"finished":{"type":"noul","noul":0.8}}})),
    )
}

#[tokio::test]
async fn judge_translates_noul_and_retries_service_unavailable() {
    let count = Arc::new(AtomicUsize::new(0));
    let base = start_server(
        Router::new()
            .route("/v1/systemone", post(judgment))
            .with_state(count.clone()),
    )
    .await;
    let store = Arc::new(InMemoryCredentialStore::default());
    store
        .upsert(
            "typesafe",
            CredentialKind::ApiKey {
                key: "test-key".into(),
            },
            "key",
            "test",
        )
        .await;
    let pool = Arc::new(AccountPool::new(
        store,
        reqwest::Client::new(),
        Endpoints::production(),
    ));
    let judge = Judge::new(reqwest::Client::new(), Endpoints::production(), pool);
    let model = ModelInfo {
        provider: "typesafe".into(),
        id: "jev-latest".into(),
        name: "Jev".into(),
        api: WireApi::TypesafeSystemOne,
        base_url: base,
        reasoning: false,
        images: false,
        cost: Default::default(),
        context_window: 4096,
        max_tokens: 1024,
        thinking_level_map: BTreeMap::new(),
        kind: ModelKind::Classifier,
        compat: Value::Null,
    };
    let answers = judge
        .judge(
            &model,
            json!({"state":"sample"}),
            vec![Question::boolean(
                "finished",
                "Is the task finished?",
                "the task is done",
                "work remains",
            )],
        )
        .await
        .unwrap();
    assert_eq!(
        answers.get("finished"),
        Some(&Answer::Bool { probability: 0.8 })
    );
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

async fn refresh_failure(State(count): State<Arc<AtomicUsize>>) -> (StatusCode, &'static str) {
    count.fetch_add(1, Ordering::SeqCst);
    (StatusCode::UNAUTHORIZED, "expired refresh token")
}

#[tokio::test]
async fn refresh_is_single_flight_and_failed_credential_is_disabled() {
    let count = Arc::new(AtomicUsize::new(0));
    let base = start_server(
        Router::new()
            .route("/oauth/token", post(refresh_failure))
            .with_state(count.clone()),
    )
    .await;
    let store = Arc::new(InMemoryCredentialStore::default());
    store
        .upsert(
            "openai-codex",
            CredentialKind::OAuth {
                access: "old".into(),
                refresh: "refresh".into(),
                expires_ms: 0,
                account_id: Some("account".into()),
                email: None,
            },
            "account",
            "account",
        )
        .await;
    let mut overrides = BTreeMap::new();
    overrides.insert("openai-auth".into(), base);
    let pool = Arc::new(AccountPool::new(
        store.clone(),
        reqwest::Client::new(),
        Endpoints::with_overrides(overrides),
    ));
    let (first, second) = tokio::join!(
        pool.resolve("openai-codex", "first"),
        pool.resolve("openai-codex", "second")
    );
    assert!(first.is_err());
    assert!(second.is_err());
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(
        store.list("openai-codex").await[0]
            .disabled_cause
            .as_deref(),
        Some("refresh_failed")
    );
}
