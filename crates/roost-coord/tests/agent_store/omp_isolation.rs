//! OMP rows must never enter native adoption or model history.
//! Uses the native store fixture and the real migrated database.
//! Backend isolation belongs at the SQL read boundary.

use super::*;

#[tokio::test]
async fn omp_rows_are_invisible_to_the_native_runtime() {
    let root = scratch_root();
    let db = db_support::open_test_database(&root)
        .await
        .expect("database");
    let store = CoordAgentStore::new(db.clone());
    store
        .save_conversation(&conversation("native", None), 0)
        .await
        .expect("native");
    sqlx::query("INSERT INTO agent_conversations(id,title,worker_fp,worker_label,worker_os,cwd,thinking_level,mode,run_state,created_ms,updated_ms,backend,model_provider,model_id) VALUES('omp','OMP','fp','machine','linux','/work','','normal','running',1,2,'omp','omp-provider','omp-model')")
        .execute(db.pool()).await.expect("OMP row");
    assert_eq!(
        store
            .conversations()
            .await
            .expect("list")
            .iter()
            .map(|record| record.id.as_str())
            .collect::<Vec<_>>(),
        vec!["native"]
    );
    assert_eq!(store.conversation("omp").await.expect("lookup"), None);
    assert_eq!(
        roost_coord::agent::store::recent_models(&db, 10)
            .await
            .expect("models"),
        vec![ModelRef {
            provider: "anthropic".into(),
            model_id: "claude".into()
        }]
    );
    db.pool().close().await;
    let _ = std::fs::remove_dir_all(root);
}
