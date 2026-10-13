//! Credential encryption, cross-replica OAuth refresh and durable login behavior.
//! The same fixtures run against SQLite and the configured Postgres test server.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use roost_coord::agent::LoginRegistry;
use roost_coord::agent::credentials::CoordCredentialStore;
use roost_llm::credentials::{CredentialKind, CredentialStore};
use roost_llm::{AccountPool, Endpoints};
use serde_json::{Value, json};
use tokio::net::TcpListener;

const SECRET_KEY: [u8; 32] = [17; 32];

fn scratch_root() -> std::path::PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "roost-agent-credentials-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("create scratch directory");
    path
}

async fn serve(app: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake endpoint");
    let address = listener.local_addr().expect("fake endpoint address");
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake endpoint");
    });
    format!("http://{address}")
}

async fn refresh_token(State(calls): State<Arc<AtomicUsize>>) -> Json<Value> {
    calls.fetch_add(1, Ordering::SeqCst);
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    Json(json!({
        "access_token": "fresh-access-secret",
        "refresh_token": "rotated-refresh-secret",
        "expires_in": 3600
    }))
}

async fn login_token() -> Json<Value> {
    Json(json!({
        "access_token": "login-access-secret",
        "refresh_token": "login-refresh-secret",
        "expires_in": 3600
    }))
}

async fn login_profile() -> Json<Value> {
    Json(json!({"email":"user@example.test","org_uuid":"org-1"}))
}

#[tokio::test]
async fn encrypted_credentials_round_trip_and_legacy_rows_migrate_on_read() {
    let root = scratch_root();
    let db = db_support::open_test_database(&root)
        .await
        .expect("open database");
    let store = CoordCredentialStore::new(db.clone(), Some(SECRET_KEY));
    let id = store
        .upsert(
            "openrouter",
            CredentialKind::ApiKey {
                key: "sensitive-api-key-material".to_owned(),
            },
            "account:one",
            "One",
        )
        .await
        .expect("store encrypted credential");
    let encrypted: String =
        sqlx::query_scalar("SELECT data_json FROM agent_credentials WHERE id=$1")
            .bind(id)
            .fetch_one(db.pool())
            .await
            .expect("read encrypted row");
    let envelope: Value = serde_json::from_str(&encrypted).expect("envelope JSON");
    assert_eq!(envelope["v"], 1);
    assert!(!encrypted.contains("sensitive-api-key-material"));
    assert!(!envelope["nonce"].as_str().expect("nonce").is_empty());
    assert!(!envelope["ct"].as_str().expect("ciphertext").is_empty());

    let pool = AccountPool::new(
        Arc::new(store.clone()),
        reqwest::Client::new(),
        Endpoints::production(),
    );
    let (_, auth) = pool
        .resolve("openrouter", "conversation")
        .await
        .expect("resolve decrypted credential");
    assert_eq!(auth.secret(), "sensitive-api-key-material");

    let legacy_kind = CredentialKind::ApiKey {
        key: "legacy-cleartext-key".to_owned(),
    };
    let legacy_json = serde_json::to_string(&legacy_kind).expect("serialize legacy credential");
    let legacy_id: i64 = sqlx::query_scalar("INSERT INTO agent_credentials(provider,kind,identity_key,label,data_json,created_ms,updated_ms) VALUES($1,'api_key',$2,$3,$4,1,1) RETURNING id")
        .bind("legacy-provider")
        .bind("legacy:one")
        .bind("Legacy")
        .bind(legacy_json)
        .fetch_one(db.pool())
        .await
        .expect("insert legacy row");
    let legacy = store
        .list("legacy-provider")
        .await
        .expect("read and migrate legacy row");
    assert_eq!(legacy[0].id, legacy_id);
    assert_eq!(legacy[0].kind, legacy_kind);
    let migrated: String =
        sqlx::query_scalar("SELECT data_json FROM agent_credentials WHERE id=$1")
            .bind(legacy_id)
            .fetch_one(db.pool())
            .await
            .expect("read migrated row");
    let migrated: Value = serde_json::from_str(&migrated).expect("migrated envelope");
    assert_eq!(migrated["v"], 1);
    assert!(!migrated.to_string().contains("legacy-cleartext-key"));

    let plaintext_store = CoordCredentialStore::new(db.clone(), None);
    let plaintext_id = plaintext_store
        .upsert(
            "plaintext-provider",
            CredentialKind::ApiKey {
                key: "sqlite-without-key".to_owned(),
            },
            "plain:one",
            "Plain",
        )
        .await
        .expect("store unencrypted credential without a key");
    let plaintext: String =
        sqlx::query_scalar("SELECT data_json FROM agent_credentials WHERE id=$1")
            .bind(plaintext_id)
            .fetch_one(db.pool())
            .await
            .expect("read plaintext row");
    assert!(plaintext.contains("sqlite-without-key"));
    assert!(
        serde_json::from_str::<Value>(&plaintext)
            .expect("credential JSON")
            .get("v")
            .is_none()
    );
    assert!(matches!(
        plaintext_store.list("openrouter").await,
        Err(roost_llm::CredentialStoreError::EncryptionKeyMissing)
    ));
    let wrong_key = CoordCredentialStore::new(db.clone(), Some([18; 32]));
    assert!(matches!(
        wrong_key.list("openrouter").await,
        Err(roost_llm::CredentialStoreError::WrongEncryptionKey)
    ));
    db.pool().close().await;
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn two_database_pools_refresh_one_oauth_credential_once() {
    let root = scratch_root();
    let db_a = db_support::open_test_database(&root)
        .await
        .expect("open first database pool");
    let db_b = db_support::open_test_database(&root)
        .await
        .expect("open second database pool");
    let store_a = Arc::new(CoordCredentialStore::new(db_a.clone(), Some(SECRET_KEY)));
    let store_b = Arc::new(CoordCredentialStore::new(db_b.clone(), Some(SECRET_KEY)));
    let credential_id = store_a
        .upsert(
            "anthropic",
            CredentialKind::OAuth {
                access: "expired-access-secret".to_owned(),
                refresh: "refresh-secret".to_owned(),
                expires_ms: 0,
                account_id: None,
                email: Some("user@example.test".to_owned()),
            },
            "oauth:user@example.test",
            "User",
        )
        .await
        .expect("store expired OAuth credential");
    let calls = Arc::new(AtomicUsize::new(0));
    let endpoint = serve(
        Router::new()
            .route("/v1/oauth/token", post(refresh_token))
            .with_state(Arc::clone(&calls)),
    )
    .await;
    let endpoints = Endpoints::with_overrides([("anthropic-console".to_owned(), endpoint)].into());
    let pool_a = AccountPool::new(store_a.clone(), reqwest::Client::new(), endpoints.clone());
    let pool_b = AccountPool::new(store_b.clone(), reqwest::Client::new(), endpoints);
    let (first, second) = tokio::join!(
        pool_a.resolve("anthropic", "conversation-a"),
        pool_b.resolve("anthropic", "conversation-b"),
    );
    assert_eq!(
        first.expect("first replica resolves").1.secret(),
        "fresh-access-secret"
    );
    assert_eq!(
        second.expect("second replica resolves").1.secret(),
        "fresh-access-secret"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let latest = store_b
        .list("anthropic")
        .await
        .expect("read refreshed credential");
    assert_eq!(latest[0].id, credential_id);
    assert!(matches!(
        &latest[0].kind,
        CredentialKind::OAuth { access, refresh, .. }
            if access == "fresh-access-secret" && refresh == "rotated-refresh-secret"
    ));
    let raw: String = sqlx::query_scalar("SELECT data_json FROM agent_credentials WHERE id=$1")
        .bind(credential_id)
        .fetch_one(db_a.pool())
        .await
        .expect("read encrypted refreshed row");
    assert!(!raw.contains("refresh-secret"));
    assert!(!raw.contains("fresh-access-secret"));
    db_a.pool().close().await;
    db_b.pool().close().await;
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn login_started_on_one_registry_can_be_polled_and_answered_on_another() {
    let root = scratch_root();
    let db_a = db_support::open_test_database(&root)
        .await
        .expect("open first database pool");
    let db_b = db_support::open_test_database(&root)
        .await
        .expect("open second database pool");
    let store_a = Arc::new(CoordCredentialStore::new(db_a.clone(), Some(SECRET_KEY)));
    let store_b = Arc::new(CoordCredentialStore::new(db_b.clone(), Some(SECRET_KEY)));
    let endpoint = serve(
        Router::new()
            .route("/v1/oauth/token", post(login_token))
            .route("/api/oauth/profile", get(login_profile)),
    )
    .await;
    let endpoints = Endpoints::with_overrides([("anthropic-console".to_owned(), endpoint)].into());
    let pool_a = Arc::new(AccountPool::new(
        store_a,
        reqwest::Client::new(),
        endpoints.clone(),
    ));
    let pool_b = Arc::new(AccountPool::new(
        store_b.clone(),
        reqwest::Client::new(),
        endpoints.clone(),
    ));
    let registry_a = LoginRegistry::new(
        db_a.clone(),
        "login-replica-a".to_owned(),
        reqwest::Client::new(),
        endpoints.clone(),
        pool_a,
    );
    let registry_b = LoginRegistry::new(
        db_b.clone(),
        "login-replica-b".to_owned(),
        reqwest::Client::new(),
        endpoints,
        pool_b,
    );

    let login_id = registry_a.start("anthropic").await.expect("start login");
    let waiting = registry_b
        .state(&login_id)
        .await
        .expect("poll from replica B");
    assert_eq!(
        waiting.prompt.as_ref().map(|prompt| prompt.id.as_str()),
        Some("manual_code")
    );
    registry_b
        .respond(&login_id, "manual_code", "authorization-code")
        .await
        .expect("respond from replica B");
    let finished = registry_a
        .state(&login_id)
        .await
        .expect("poll result on replica A");
    assert_eq!(finished.state, roost_llm::oauth::LoginStatus::Done);
    assert_eq!(
        store_b
            .list("anthropic")
            .await
            .expect("list saved account")
            .len(),
        1
    );
    registry_b
        .cancel(&login_id)
        .await
        .expect("cancel on replica B");
    assert!(matches!(
        registry_a.state(&login_id).await,
        Err(roost_coord::agent::LoginError::NotFound(_))
    ));
    db_a.pool().close().await;
    db_b.pool().close().await;
    let _ = std::fs::remove_dir_all(root);
}
