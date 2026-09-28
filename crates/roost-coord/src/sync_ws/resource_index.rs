//! The resources one Sync socket may observe: the sessions, workers and
//! workspaces a frame is narrowed against, loaded once when the socket opens
//! and kept current by the live feed for the socket's lifetime.
//!
//! Owned by `sync_ws::socket`, which loads it, and mutated only by
//! `sync_ws::live_feed` under the socket's link lock. Ports
//! `loadSyncResourceIndex` and `SyncResourceIndex` from
//! `apps/coord/src/sync/sync-feed-seed.ts`; the retained seeds in that file are
//! not here.
//!
//! A BROWSER SEES THE INSTALL, A WORKER SEES ITSELF. `owner_worker_fp` is `None`
//! for an account device and the worker's own fingerprint for a worker socket,
//! and every set below is narrowed by it at load. A worker's read-only
//! firehose that carried another worker's state would be a cross-tenant leak
//! on a machine the operator did not choose to trust with it.

use std::collections::BTreeSet;

use roost_protocol::wire::WorkerFp;

use crate::db::CoordDb;

/// What one socket may observe, as of now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncResourceIndex {
    /// The worker a read-only worker socket is scoped to, or `None` for a
    /// browser, which admits every resource in the install.
    pub owner_worker_fp: Option<String>,
    /// Sessions whose title, presence, activity and agent status this socket
    /// receives. Durable `opened`/`snapshot`/`closed` events keep it current.
    pub session_ids: BTreeSet<String>,
    /// Workers whose routability this socket receives.
    pub worker_fps: BTreeSet<WorkerFp>,
    /// Workspaces whose deletes and membership changes this socket receives.
    pub workspace_ids: BTreeSet<String>,
}

impl SyncResourceIndex {
    /// Whether this socket observes `worker_fp`'s resources.
    #[must_use]
    pub fn owns_worker(&self, worker_fp: &str) -> bool {
        self.owner_worker_fp
            .as_deref()
            .is_none_or(|owner| owner == worker_fp)
    }

    /// Whether this socket sees install-wide resources -- tasks, MCP relays,
    /// pair requests and audit rows -- which have no worker owner and so no
    /// worker socket is one of their viewers.
    #[must_use]
    pub fn is_install_wide(&self) -> bool {
        self.owner_worker_fp.is_none()
    }
}

/// Load the persisted resources a socket for `owner_worker_fp` may observe.
///
/// Three reads, not one join: the sets are independent and each is narrowed by
/// the same owner. A stored worker fingerprint that is not a fingerprint is a
/// corrupt row, and it is skipped with a warning rather than failing a socket
/// that has nothing to do with it.
pub async fn load_sync_resource_index(
    db: &CoordDb,
    owner_worker_fp: Option<&str>,
) -> Result<SyncResourceIndex, sqlx::Error> {
    let (workers, sessions, workspaces) = match owner_worker_fp {
        None => (
            ids(
                db,
                "SELECT fp FROM workers WHERE deleted_at_ms IS NULL",
                None,
            )
            .await?,
            ids(db, "SELECT id FROM sessions", None).await?,
            ids(db, "SELECT id FROM workspaces", None).await?,
        ),
        Some(owner) => (
            ids(
                db,
                "SELECT fp FROM workers WHERE deleted_at_ms IS NULL AND fp = ?1",
                Some(owner),
            )
            .await?,
            ids(
                db,
                "SELECT id FROM sessions WHERE worker_fp = ?1",
                Some(owner),
            )
            .await?,
            ids(
                db,
                "SELECT id FROM workspaces WHERE worker_fp = ?1",
                Some(owner),
            )
            .await?,
        ),
    };
    let mut worker_fps = BTreeSet::new();
    for fp in workers {
        match WorkerFp::try_from(fp.as_str()) {
            Ok(worker_fp) => {
                worker_fps.insert(worker_fp);
            }
            Err(error) => tracing::warn!(
                event = "sync-ws",
                action = "resource_index_bad_worker",
                worker_fp = %fp,
                error = %error,
                "a stored worker fingerprint is malformed and is not observable"
            ),
        }
    }
    Ok(SyncResourceIndex {
        owner_worker_fp: owner_worker_fp.map(str::to_owned),
        session_ids: sessions.into_iter().collect(),
        worker_fps,
        workspace_ids: workspaces.into_iter().collect(),
    })
}

async fn ids(
    db: &CoordDb,
    sql: &'static str,
    owner: Option<&str>,
) -> Result<Vec<String>, sqlx::Error> {
    let query = sqlx::query_scalar::<_, String>(sql);
    let query = match owner {
        Some(owner) => query.bind(owner.to_owned()),
        None => query,
    };
    query.fetch_all(db.pool()).await
}
