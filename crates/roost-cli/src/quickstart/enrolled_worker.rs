//! Whether this machine's worker is already a member of the fleet, so a
//! quickstart rerun mints an enrollment grant only for a worker that needs
//! one. Called by `quickstart::install_everything`; depends on `roost-worker`
//! for the key file and its fingerprint and on `roost-coord` for the database.
//!
//! A grant written into the definition of an enrolled worker is a credential
//! that outlives its purpose: it expires in a day, and every later restart
//! re-offers a token the coordinator then refuses.

use std::path::{Path, PathBuf};

use roost_host::{EnvSource, HostPlatform, worker_data_dir};
use roost_worker::host::jwt::read_worker_fingerprint;
use roost_worker::runtime::boot::{ENV_WORKER_KEY_PATH, WORKER_KEY_NAME};

use crate::command_error::CommandFailure;

/// Whether the local worker's key has a live row in the coordinator's
/// `workers` table at `database`.
///
/// No key file, or one that does not parse, is "not enrolled": the worker
/// generates or refuses its key at boot, and either way a grant is the only
/// thing that could enroll it.
pub async fn worker_already_enrolled(
    env: &dyn EnvSource,
    platform: HostPlatform,
    database: &Path,
) -> Result<bool, CommandFailure> {
    key_has_worker_row(&local_worker_key_path(env, platform)?, database).await
}

/// The key file the worker boot resolves: the override when one is set, else
/// the key inside the worker's data directory.
fn local_worker_key_path(
    env: &dyn EnvSource,
    platform: HostPlatform,
) -> Result<PathBuf, CommandFailure> {
    if let Some(path) = env.get(ENV_WORKER_KEY_PATH).filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let data_dir = worker_data_dir(env, platform).map_err(|error| {
        CommandFailure::generic(format!(
            "the worker data directory could not be resolved: {error}"
        ))
    })?;
    Ok(data_dir.join(WORKER_KEY_NAME))
}

async fn key_has_worker_row(key_path: &Path, database: &Path) -> Result<bool, CommandFailure> {
    let Ok(fingerprint) = read_worker_fingerprint(key_path) else {
        return Ok(false);
    };
    let opened = roost_coord::db::open(database).await.map_err(|error| {
        CommandFailure::generic(format!(
            "the coordinator database {} could not be opened: {error}",
            database.display()
        ))
    })?;
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT 1 FROM workers WHERE fp = ? AND deleted_at_ms IS NULL")
            .bind(fingerprint.as_str())
            .fetch_optional(opened.pool())
            .await
            .map_err(|error| {
                CommandFailure::generic(format!(
                    "the worker roster in {} could not be read: {error}",
                    database.display()
                ))
            })?;
    Ok(row.is_some())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::PathBuf;

    use super::key_has_worker_row;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(case: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "roost-enrolled-worker-{}-{case}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("scratch");
            Self(root)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    async fn register(database: &std::path::Path, fingerprint: &str, deleted: Option<i64>) {
        let opened = roost_coord::db::open(database).await.expect("opened");
        let tenant = roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant(&opened, 1)
            .await
            .expect("the tenant");
        sqlx::query(
            "INSERT INTO workers \
             (fp, label, os, registered_at_ms, last_seen_ms, deleted_at_ms, dashboard_id) \
             VALUES (?, 'here', 'linux', 1, 1, ?, ?)",
        )
        .bind(fingerprint)
        .bind(deleted)
        .bind(&tenant.dashboard_id)
        .execute(opened.pool())
        .await
        .expect("a worker row");
    }

    /// A rerun on an enrolled machine must not arm a fresh grant, and a first
    /// run, a deleted worker or a missing key must.
    #[tokio::test]
    async fn only_a_key_with_a_live_worker_row_counts_as_enrolled() {
        let scratch = Scratch::new("roster");
        let database = scratch.0.join("coordinator.sqlite");
        roost_coord::db::open(&database).await.expect("migrated");
        let key = scratch.0.join("worker.key");

        assert!(
            !key_has_worker_row(&key, &database).await.unwrap(),
            "no key"
        );

        let fingerprint = roost_worker::host::jwt::load_worker_key(&key)
            .expect("a key is generated")
            .fingerprint()
            .to_string();
        assert!(
            !key_has_worker_row(&key, &database).await.unwrap(),
            "a key the coordinator never registered"
        );

        register(&database, &fingerprint, None).await;
        assert!(
            key_has_worker_row(&key, &database).await.unwrap(),
            "enrolled"
        );

        let deleted = Scratch::new("deleted");
        let deleted_database = deleted.0.join("coordinator.sqlite");
        roost_coord::db::open(&deleted_database)
            .await
            .expect("migrated");
        register(&deleted_database, &fingerprint, Some(5)).await;
        assert!(
            !key_has_worker_row(&key, &deleted_database).await.unwrap(),
            "a deleted worker is not enrolled"
        );
    }
}
