//! The push tests' shared fixture: a migrated coordinator database with a
//! self-hosted tenant, one account device, and the operator's push origins.
//!
//! Owned by the push tests. Every test in `push_*.rs` builds one of these, so
//! the setup that could hide a defect -- a real migration, a real tenancy
//! invariant, a real `authorized_keys` row the `push_subscriptions` foreign key
//! has to resolve against -- is written once and is the same in all of them.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod transport;

pub use transport::{CountingGenerator, FakeTransport, RecordedDelivery};

use std::path::PathBuf;
use std::sync::Arc;

use roost_coord::auth::principal::Principal;
use roost_coord::coord_core::{Caller, CoordCore, ListenerTrust};
use roost_coord::db::CoordDb;
use roost_coord::push::PushRuntime;
use roost_coord::push::vapid::{P256KeypairGenerator, VapidKeyGenerator};
use roost_coord::services::CoordServices;
use sqlx::AssertSqlSafe;

/// The one origin every push test's allowlist contains.
pub const PUSH_ORIGIN: &str = "https://push.example";

/// The session every dispatch test uses.
pub const SESSION_ID: &str = "11111111-1111-4111-8111-111111111111";

/// A browser device fingerprint: 64 hex characters, as the schema requires.
pub fn viewer_fp(seed: char) -> String {
    std::iter::repeat_n(seed, 64).collect()
}

/// A caller that passed the device gate.
pub fn browser_caller(fp: &str, account_id: &str) -> Caller {
    Caller {
        principal: Principal::AccountDevice {
            fingerprint: fp.to_owned(),
            label: "push-test".to_owned(),
            account_id: account_id.to_owned(),
        },
        tab_id: Some("push-test-tab".to_owned()),
        remote_address: Some("127.0.0.1".to_owned()),
        on_host: true,
        listener_trust: ListenerTrust::DirectLoopback,
    }
}

/// A coordinator over a scratch database, with a tenant and one browser device.
pub struct PushFixture {
    /// The coordinator's shared state, as a handler receives it.
    pub core: CoordCore,
    /// The push runtime, kept for the tests that vary the allowlist.
    pub push: PushRuntime,
    /// The scratch directory, removed when the fixture drops.
    root: PathBuf,
    /// The device the tests authenticate as.
    pub caller: Caller,
    /// The dashboard subscriptions are scoped to.
    pub dashboard_id: String,
    /// The account the browser device belongs to.
    pub account_id: String,
}

/// The production VAPID generator, for the tests that do not count draws.
#[must_use]
pub fn production_generator() -> Arc<dyn VapidKeyGenerator> {
    Arc::new(P256KeypairGenerator)
}

impl PushFixture {

    /// A fixture with `PUSH_ORIGIN` as its only allowed origin.
    pub async fn new(label: &str) -> Self {
        Self::build(label, vec![PUSH_ORIGIN.to_owned()], production_generator()).await
    }

    /// A fixture with a chosen allowlist and a chosen VAPID key generator.
    ///
    /// The generator is a parameter so a test can count how many identities a
    /// coordinator minted; that is the only way to prove first-use
    /// serialisation rather than merely proving a row exists.
    pub async fn build(
        label: &str,
        allowed_origins: Vec<String>,
        generator: Arc<dyn VapidKeyGenerator>,
    ) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-push-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = roost_coord::db::open(&root.join("coord.db"))
            .await
            .expect("a migrated coordinator database");
        let tenant =
            roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant(&database, 1_000)
                .await
                .expect("a self-hosted tenant");

        let fp = viewer_fp('a');
        seed_device(&database, &tenant.account_id, &fp).await;

        let push = PushRuntime::with_keypair_generator(
            tenant.dashboard_id.clone(),
            allowed_origins,
            generator,
        );
        let services = Arc::new(CoordServices::new(database));
        let core = CoordCore::with_push(Arc::clone(&services), push.clone());

        Self {
            core,
            push,
            root,
            caller: browser_caller(&fp, &tenant.account_id),
            dashboard_id: tenant.dashboard_id,
            account_id: tenant.account_id,
        }
    }

    /// The coordinator's database handle.
    pub fn database(&self) -> &CoordDb {
        &self.core.services.db
    }

    /// The fingerprint the tests authenticate as.
    pub fn fp(&self) -> String {
        viewer_fp('a')
    }

    /// Run a statement against the fixture's database.
    pub async fn exec(&self, sql: &str) {
        sqlx::query(AssertSqlSafe(sql))
            .execute(self.database().pool())
            .await
            .expect("the statement applies");
    }

    /// The endpoints one device currently holds, sorted.
    pub async fn endpoints_for(&self, viewer_fp: &str) -> Vec<String> {
        sqlx::query_scalar::<_, String>(
            "SELECT endpoint FROM push_subscriptions WHERE viewer_fp = ?1 ORDER BY endpoint",
        )
        .bind(viewer_fp)
        .fetch_all(self.database().pool())
        .await
        .expect("the rows read")
    }

    /// The stored `p256dh` and `auth` for one endpoint.
    pub async fn keys_for(&self, endpoint: &str) -> Option<(String, String)> {
        use sqlx::Row as _;
        let row =
            sqlx::query("SELECT p256dh, auth FROM push_subscriptions WHERE endpoint = ?1 LIMIT 1")
                .bind(endpoint)
                .fetch_optional(self.database().pool())
                .await
                .expect("the row reads");
        row.map(|row| {
            (
                row.try_get("p256dh").expect("p256dh"),
                row.try_get("auth").expect("auth"),
            )
        })
    }

    /// Insert a subscription row directly, for the delivery tests that need a
    /// stored endpoint without going through the RPC.
    pub async fn seed_subscription(&self, dashboard_id: &str, viewer_fp: &str, endpoint: &str) {
        sqlx::query(
            "INSERT INTO push_subscriptions \
               (dashboard_id, viewer_fp, endpoint, p256dh, auth, created_at_ms) \
             VALUES (?1, ?2, ?3, 'abc', 'def', ?4)",
        )
        .bind(dashboard_id)
        .bind(viewer_fp)
        .bind(endpoint)
        .bind(1_000_i64)
        .execute(self.database().pool())
        .await
        .expect("the subscription row");
    }
}

impl Drop for PushFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Insert the `authorized_keys` and `account_devices` rows a subscription's
/// foreign keys resolve against.
async fn seed_device(database: &CoordDb, account_id: &str, fp: &str) {
    sqlx::query(
        "INSERT INTO authorized_keys (fingerprint, public_key, label, added_at) \
         VALUES (?1, ?2, ?3, ?4)",
    )
    .bind(fp)
    .bind(vec![0_u8; 32])
    .bind("push-device")
    .bind(1_000_i64)
    .execute(database.pool())
    .await
    .expect("the device key row");
    sqlx::query(
        "INSERT INTO account_devices (fingerprint, account_id, added_at_ms, last_seen_at_ms) \
         VALUES (?1, ?2, ?3, ?4)",
    )
    .bind(fp)
    .bind(account_id)
    .bind(1_000_i64)
    .bind(1_000_i64)
    .execute(database.pool())
    .await
    .expect("the account device row");
}

/// Insert a worker and an open session, so a dispatch has a session to find.
pub async fn seed_open_session(fixture: &PushFixture, cwd: &str) {
    let worker_fp = viewer_fp('c');
    sqlx::query(
        "INSERT INTO workers (dashboard_id, fp, label, os, git_sha, host_metrics_json, \
                              registered_at_ms, last_seen_ms, reachable_addr) \
         VALUES (?1, ?2, 'push-worker', 'linux', NULL, NULL, ?3, ?3, NULL)",
    )
    .bind(&fixture.dashboard_id)
    .bind(&worker_fp)
    .bind(1_000_i64)
    .execute(fixture.database().pool())
    .await
    .expect("the worker row");
    sqlx::query(
        "INSERT INTO sessions (id, dashboard_id, worker_fp, channel, kind, cwd, workspace_id, \
                               status, created_at, closed_at, custom_title, spawn_cwd) \
         VALUES (?1, ?2, ?3, 1, 'shell', ?4, NULL, 'open', ?5, NULL, NULL, ?4)",
    )
    .bind(SESSION_ID)
    .bind(&fixture.dashboard_id)
    .bind(&worker_fp)
    .bind(cwd)
    .bind(1_000_i64)
    .execute(fixture.database().pool())
    .await
    .expect("the session row");
}
