//! The MCP relay tests' shared fixture: a migrated coordinator database with a
//! self-hosted tenant, one device caller and one worker caller, and a way to
//! collect the relay stream while a body runs.
//!
//! Owned by the four `mcp_relays_*.rs` binaries. The fixture is compiled into
//! each of them and each uses a different subset, so an item unused by ONE of
//! them is not dead.

#![allow(clippy::unwrap_used, clippy::expect_used, dead_code, unused_imports)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use connectrpc::ConnectError;
use roost_coord::auth::principal::Principal;
use roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant;
use roost_coord::coord_core::{BootFacts, Caller, CoordCore, ListenerTrust};
use roost_coord::db::CoordDb;
use roost_coord::services::CoordServices;
use roost_proto as proto;
use roost_protocol::wire::McpStreamMessage;
use sqlx::Row;

/// A dashboard this coordinator does not hold, so a row can be planted in it.
pub const FOREIGN_DASHBOARD: &str = "00000000-0000-4000-8000-00000000dead";

/// A relay planted in the foreign dashboard. Well formed, so only the scope can
/// refuse it.
pub const FOREIGN_RELAY: &str = "00000000-0000-4000-8000-00000000beef";

/// A relay id this coordinator does not hold, for the paths that need one and
/// do not care whether it exists.
pub const UNKNOWN_RELAY: &str = "00000000-0000-4000-8000-00000000cafe";

/// A coordinator with the tenancy scope boot resolved, over a scratch database.
pub struct McpFixture {
    /// The coordinator's shared state, as a handler receives it.
    pub core: CoordCore,
    /// The dashboard the handlers scope to.
    pub dashboard_id: String,
    /// The scratch directory, removed when the fixture drops.
    root: PathBuf,
}

impl McpFixture {
    pub async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-mcp-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = super::db_support::open_test_database(&root)
            .await
            .expect("a migrated coordinator database");
        let tenant = ensure_self_hosted_tenant(&database, 0)
            .await
            .expect("a self-hosted tenant");
        // The dashboard the handlers scope to is a boot fact, so the fixture
        // installs one: a coordinator built without it is what an unbooted
        // `CoordServices` is, and `mcp_relays_tenancy.rs` asserts that refusal
        // rather than the fixture providing a fallback for it.
        let services = CoordServices::booted(
            database,
            BootFacts {
                tenant: Some(tenant.clone()),
                ..BootFacts::unbooted()
            },
        );
        Self {
            core: CoordCore::new(Arc::new(services)),
            dashboard_id: tenant.dashboard_id,
            root,
        }
    }

    pub fn database(&self) -> &CoordDb {
        &self.core.services.db
    }

    /// A paired browser: the authority the relay registry admits.
    pub fn device(&self) -> Caller {
        caller(Principal::AccountDevice {
            fingerprint: std::iter::repeat_n('a', 64).collect(),
            label: "MCP pane".to_owned(),
            account_id: "account-under-test".to_owned(),
        })
    }

    /// A registered machine: the authority the relay registry refuses.
    pub fn worker(&self) -> Caller {
        caller(Principal::Worker {
            fingerprint: "mcp-worker".to_owned(),
            label: "a worker".to_owned(),
        })
    }

    /// Run `body` with the relay stream collected, and hand back what it saw.
    ///
    /// The subscription is taken before the body starts and dropped after it
    /// ends, so the count a handler reports and the messages it published are
    /// the same observation rather than two.
    pub async fn collect<T, F>(&self, body: F) -> (T, Vec<McpStreamMessage>)
    where
        F: Future<Output = T>,
    {
        let seen: Arc<Mutex<Vec<McpStreamMessage>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let subscription =
            self.core
                .services
                .buses
                .mcp_bus
                .subscribe(move |message: &McpStreamMessage| {
                    sink.lock()
                        .expect("the stream sink lock")
                        .push(message.clone());
                });
        let outcome = body.await;
        drop(subscription);
        let messages = seen.lock().expect("the stream sink lock").clone();
        (outcome, messages)
    }

    /// Every relay row, whatever dashboard holds it, as (id, dashboard_id).
    pub async fn rows(&self) -> Vec<(String, String)> {
        let rows = sqlx::query("SELECT id, dashboard_id FROM mcp_relays")
            .fetch_all(self.database().pool())
            .await
            .expect("the relay rows");
        rows.iter()
            .map(|row| {
                (
                    row.try_get::<String, _>("id").expect("the relay id"),
                    row.try_get::<String, _>("dashboard_id")
                        .expect("the relay dashboard"),
                )
            })
            .collect()
    }
}

impl Drop for McpFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub fn caller(principal: Principal) -> Caller {
    Caller {
        principal,
        tab_id: Some("tab-under-test".to_owned()),
        remote_address: Some("127.0.0.1".to_owned()),
        on_host: true,
        listener_trust: ListenerTrust::DirectLoopback,
    }
}

pub fn create_request(label: &str, kind: &str, config_json: &str) -> proto::McpCreateRequest {
    proto::McpCreateRequest {
        label: label.to_owned(),
        kind: kind.to_owned(),
        config_json: config_json.to_owned(),
        ..Default::default()
    }
}

pub fn publish_request(id: &str, payload_json: &str) -> proto::McpPublishRequest {
    proto::McpPublishRequest {
        id: id.to_owned(),
        payload_json: payload_json.to_owned(),
        ..Default::default()
    }
}

pub fn delete_request(id: &str) -> proto::McpDeleteRequest {
    proto::McpDeleteRequest {
        id: id.to_owned(),
        ..Default::default()
    }
}

pub fn message_of(error: &ConnectError) -> String {
    // `ConnectError::message` is a FIELD, an `Option<String>` carrying the text
    // the client will read, and `payload::message::<M>()` is a different thing
    // that decodes a body. A test asserting a refusal's wording wants the former.
    error.message.clone().unwrap_or_default()
}

/// Plant a relay that exists but belongs to a dashboard this coordinator does
/// not hold, which is the only way a scoped read can be proved.
///
/// The dashboard is a real row, because `mcp_relays.dashboard_id` is a foreign
/// key and the store runs with `foreign_keys(true)`: a relay pointing at no
/// dashboard would fail to insert, and the test would pass by never reaching
/// the scope it exists to exercise.
pub async fn plant_foreign_relay(fixture: &McpFixture) {
    sqlx::query(
        "INSERT INTO dashboards (id, organization_id, slug, name, status, created_at_ms) \
         SELECT $1, organization_id, 'other-dashboard', 'Another dashboard', 'active', 0 \
         FROM dashboards LIMIT 1",
    )
    .bind(FOREIGN_DASHBOARD)
    .execute(fixture.database().pool())
    .await
    .expect("the foreign dashboard row");
    sqlx::query(
        "INSERT INTO mcp_relays (id, label, kind, config_json, created_at_ms, dashboard_id) \
         VALUES ($1, 'A relay of somebody else''s', 'sse', '{}', 5, $2)",
    )
    .bind(FOREIGN_RELAY)
    .bind(FOREIGN_DASHBOARD)
    .execute(fixture.database().pool())
    .await
    .expect("the foreign relay row");
}
