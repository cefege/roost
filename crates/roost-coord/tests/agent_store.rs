//! Persistence behaviour for coordinator-owned harness conversations and accounts.
//!
//! The test uses the shared database helper so the same assertions run against
//! SQLite locally and Postgres when `ROOST_TEST_DATABASE_URL` is configured.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use roost_agent::records::{AgentSettings, ConversationRecord, Entry, Mode, Role};
use roost_agent::traits::AgentStore;
use roost_coord::agent::credentials::CoordCredentialStore;
use roost_coord::agent::store::CoordAgentStore;
use roost_llm::credentials::{CredentialKind, CredentialStore};
use roost_llm::{AccountPool, Endpoints};
use roost_protocol::wire::agent_chat::{AgentRunState, ModelRef};

fn scratch_root() -> std::path::PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("roost-agent-store-{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&path).expect("create scratch directory");
    path
}

fn conversation(id: &str, parent_id: Option<&str>) -> ConversationRecord {
    ConversationRecord {
        id: id.to_owned(),
        title: format!("title-{id}"),
        worker_fp: "worker-fp".to_owned(),
        worker_label: "desktop".to_owned(),
        worker_os: "linux".to_owned(),
        cwd: "/work/project".to_owned(),
        model: Some(ModelRef {
            provider: "anthropic".to_owned(),
            model_id: "claude".to_owned(),
        }),
        thinking_level: "auto".to_owned(),
        mode: Mode::Plan,
        pre_plan_model: Some(ModelRef {
            provider: "openrouter".to_owned(),
            model_id: "model".to_owned(),
        }),
        parent_id: parent_id.map(str::to_owned),
        agent: parent_id.map(|_| "scout".to_owned()),
        advisor: Some(true),
        run_state: AgentRunState::Running,
        error: Some("test error".to_owned()),
        created_ms: 11,
        updated_ms: 22,
    }
}

#[tokio::test]
async fn conversation_entries_settings_and_credentials_round_trip() {
    let root = scratch_root();
    let db = db_support::open_test_database(&root)
        .await
        .expect("open database");
    let store = CoordAgentStore::new(db.clone());
    let parent = conversation("parent", None);
    let child = conversation("child", Some("parent"));
    store.save_conversation(&parent).await.expect("save parent");
    store.save_conversation(&child).await.expect("save child");
    store
        .append_entry(
            "child",
            &Entry::User {
                text: "child entry".to_owned(),
            },
        )
        .await
        .expect("append child entry");
    assert_eq!(
        store.conversation("parent").await.expect("read").as_ref(),
        Some(&parent)
    );
    assert_eq!(
        store.conversations().await.expect("list"),
        vec![child, parent.clone()]
    );

    let first = Entry::User {
        text: "hello".to_owned(),
    };
    let second = Entry::Notice {
        level: "info".to_owned(),
        title: "status".to_owned(),
        body: "ready".to_owned(),
    };
    assert_eq!(
        store.append_entry("parent", &first).await.expect("append"),
        1
    );
    assert_eq!(
        store.append_entry("parent", &second).await.expect("append"),
        2
    );
    assert_eq!(
        store.entries("parent").await.expect("entries"),
        vec![(1, first), (2, second)]
    );
    let settings = AgentSettings {
        model_roles: [(Role::Smol, "openrouter/model".to_owned())]
            .into_iter()
            .collect(),
        default_model: Some(ModelRef {
            provider: "anthropic".to_owned(),
            model_id: "claude".to_owned(),
        }),
        advisor_enabled: true,
    };
    store.save_settings(&settings).await.expect("save settings");
    assert_eq!(store.settings().await.expect("load settings"), settings);

    let credentials = CoordCredentialStore::new(db);
    let original = CredentialKind::ApiKey {
        key: "secret-first".to_owned(),
    };
    let id = credentials
        .upsert("openrouter", original, "identity", "first")
        .await;
    assert!(id > 0);
    let same_id = credentials
        .upsert(
            "openrouter",
            CredentialKind::ApiKey {
                key: "secret-second".to_owned(),
            },
            "identity",
            "updated",
        )
        .await;
    assert_eq!(same_id, id);
    let rows = credentials.list("openrouter").await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].label, "updated");
    assert_eq!(
        rows[0].kind,
        CredentialKind::ApiKey {
            key: "secret-second".to_owned()
        }
    );
    let second_id = credentials
        .upsert(
            "openrouter",
            CredentialKind::ApiKey {
                key: "second".to_owned(),
            },
            "identity-second",
            "second",
        )
        .await;
    assert_ne!(second_id, id);

    let block_until = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
    )
    .expect("millisecond timestamp fits")
        + 60_000;
    credentials.set_block(id, block_until).await;
    assert_eq!(
        credentials.blocks("openrouter").await,
        vec![(id, block_until)]
    );
    credentials.set_sticky("parent", "openrouter", id).await;
    assert_eq!(credentials.sticky("parent", "openrouter").await, Some(id));
    credentials.clear_sticky("parent", "openrouter").await;
    assert_eq!(credentials.sticky("parent", "openrouter").await, None);
    let pool = AccountPool::new(
        Arc::new(credentials.clone()) as Arc<dyn CredentialStore>,
        reqwest::Client::new(),
        Endpoints::production(),
    );
    assert_eq!(
        pool.resolve("openrouter", "parent")
            .await
            .expect("resolve unblocked account")
            .0,
        second_id,
    );

    store
        .delete_conversation("parent")
        .await
        .expect("delete tree");
    assert!(
        store
            .conversation("parent")
            .await
            .expect("read parent")
            .is_none()
    );
    assert!(
        store
            .conversation("child")
            .await
            .expect("read child")
            .is_none()
    );
    assert!(
        store
            .entries("parent")
            .await
            .expect("read entries")
            .is_empty()
    );
    assert!(
        store
            .entries("child")
            .await
            .expect("read child entries")
            .is_empty()
    );
    std::fs::remove_dir_all(root).expect("remove scratch directory");
}
