//! The agent-status tests' shared fixture: a migrated coordinator database with
//! a self-hosted tenant, one browser device, four open sessions, and a byte hub
//! whose route cache names the worker that owns each of them.
//!
//! Owned by the agent tests. Every `agent_*.rs` test binary builds one, so the
//! setup that could hide a defect -- a real migration, a real tenancy scope, a
//! real route cache binding -- is written once and is the same in all of them.

// The fixture is compiled into every `agent_*.rs` test binary and each uses a
// different subset of it, so an item unused by ONE of them is not dead.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code, unused_imports)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use roost_coord::agents::status_hub::AgentStatusHub;
use roost_coord::auth::principal::Principal;
use roost_coord::auth::self_hosted_tenant::SelfHostedTenant;
use roost_coord::coord_core::boot_facts::BootFacts;
use roost_coord::coord_core::seams::CoordTerminal;
use roost_coord::coord_core::{Caller, CoordCore, ListenerTrust};
use roost_coord::db::CoordDb;
use roost_coord::services::CoordServices;
use roost_protocol::wire::{ChannelId, SessionId, WorkerFp};
use serde_json::{Value, json};
use sqlx::AssertSqlSafe;

/// The worker that owns three of the four sessions.
pub const WORKER_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// The worker that owns the fourth, so a cross-worker claim is testable.
pub const WORKER_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// Four open sessions, deliberately NOT in the order a test reports them in.
pub const SESSION_IDS: [&str; 4] = [
    "30000000-0000-4000-8000-000000000030",
    "10000000-0000-4000-8000-000000000010",
    "40000000-0000-4000-8000-000000000040",
    "20000000-0000-4000-8000-000000000020",
];
/// A session id that was never created.
pub const SESSION_MISSING: &str = "30000000-0000-4000-8000-000000000099";

/// The status epoch most fixtures pin.
pub const EPOCH_A: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
/// A second epoch, lexically lower, so a test cannot pass by accident.
pub const EPOCH_B: &str = "00000000-0000-4000-8000-000000000000";
/// An occupant within [`EPOCH_A`].
pub const OCCUPANT_A: &str = "11111111-aaaa-4aaa-8aaa-111111111111";
/// A replacement occupant within [`EPOCH_A`].
pub const OCCUPANT_B: &str = "22222222-aaaa-4aaa-8aaa-222222222222";
/// An occupant within [`EPOCH_B`].
pub const OCCUPANT_C: &str = "33333333-aaaa-4aaa-8aaa-333333333333";

/// A typed session id.
pub fn session(value: &str) -> SessionId {
    SessionId::try_from(value).expect("a session id")
}

/// A typed worker fingerprint.
pub fn worker(value: &str) -> WorkerFp {
    WorkerFp::try_from(value).expect("a worker fingerprint")
}

/// A typed channel id.
pub fn channel(value: i64) -> ChannelId {
    ChannelId::try_from(value).expect("a channel id")
}

/// An agent status update, in the exact shape a worker frame carries.
pub fn status(session_id: &str, overrides: Value) -> Value {
    let mut value = json!({
        "session_id": session_id,
        "agent_id": "omp",
        "state": "working",
        "revision": 1,
        "completed_revision": 0,
        "updated_at": 1_800_000_000_000_i64,
        "active": true,
        "status_epoch": EPOCH_A,
        "occupant_id": OCCUPANT_A,
        "source": "integration",
    });
    merge(&mut value, overrides);
    value
}

/// An update with no identity triple, as a worker deployed before durable
/// observation reports.
pub fn legacy_status(session_id: &str, overrides: Value) -> Value {
    let mut value = json!({
        "session_id": session_id,
        "agent_id": "omp",
        "state": "working",
        "revision": 1,
        "completed_revision": 0,
        "updated_at": 1_800_000_000_000_i64,
        "active": true,
    });
    merge(&mut value, overrides);
    value
}

fn merge(into: &mut Value, overrides: Value) {
    let (Value::Object(into), Value::Object(overrides)) = (&mut *into, overrides) else {
        return;
    };
    for (key, value) in overrides {
        into.insert(key, value);
    }
}

/// A coordinator over a scratch database, with the hub under the test's seams.
pub struct AgentFixture {
    /// The shared state, as a handler receives it.
    pub core: CoordCore,
    /// The scratch directory, removed when the fixture drops.
    root: PathBuf,
    /// The browser device the RPC tests authenticate as.
    pub caller: Caller,
    /// A worker identity, which no device-scoped method may answer.
    pub worker_caller: Caller,
    /// The one dashboard.
    pub dashboard_id: String,
    /// The one account.
    pub account_id: String,
}

impl AgentFixture {
    /// A fixture whose hub reads the real clock and v2's push debounce.
    pub async fn new(label: &str) -> Self {
        Self::build(
            label,
            Arc::new(roost_coord::serve::now_ms),
            Duration::from_secs(1),
        )
        .await
    }

    /// A fixture whose hub clock and push debounce the test chooses.
    pub async fn build(
        label: &str,
        now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
        push_delay: Duration,
    ) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-agents-{label}-{}-{:?}",
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
        seed_workers(&database, &tenant).await;
        seed_sessions(&database, &tenant).await;

        let mut services = CoordServices::booted(
            database,
            BootFacts {
                tenant: Some(tenant.clone()),
                config: None,
                process_epoch: "agent-fixture".to_owned(),
                boot_ms: 1_000,
            },
        );
        services.agents.status = AgentStatusHub::with_seams(now_ms, push_delay);
        // The route cache is the ownership authority the hub consults, exactly
        // as it is in production: a worker's frame is accepted only when the
        // coordinator's own binding names that worker.
        services.byte_hub.prime_channel_map(&route_rows());
        let core = CoordCore::with_terminal(Arc::new(services), CoordTerminal::none());

        Self {
            core,
            root,
            caller: browser_caller(&tenant),
            worker_caller: worker_caller(),
            dashboard_id: tenant.dashboard_id,
            account_id: tenant.account_id,
        }
    }

    /// The coordinator's database handle.
    pub fn database(&self) -> &CoordDb {
        &self.core.services.db
    }

    /// The hub under test.
    pub fn hub(&self) -> &AgentStatusHub {
        &self.core.services.agents.status
    }

    /// Run a statement against the fixture's database.
    pub async fn exec(&self, sql: &str) {
        sqlx::query(AssertSqlSafe(sql))
            .execute(self.database().pool())
            .await
            .expect("the statement applies");
    }
}

impl Drop for AgentFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The byte hub bindings: sessions one, two and four belong to worker A, and
/// session three to worker B.
fn route_rows() -> Vec<(SessionId, WorkerFp, ChannelId)> {
    vec![
        (session(SESSION_IDS[0]), worker(WORKER_A), channel(10)),
        (session(SESSION_IDS[1]), worker(WORKER_A), channel(11)),
        (session(SESSION_IDS[2]), worker(WORKER_B), channel(12)),
        (session(SESSION_IDS[3]), worker(WORKER_A), channel(13)),
    ]
}

async fn seed_workers(database: &CoordDb, tenant: &SelfHostedTenant) {
    for fp in [WORKER_A, WORKER_B] {
        sqlx::query(
            "INSERT INTO workers (dashboard_id, fp, label, os, git_sha, host_metrics_json, \
                                  registered_at_ms, last_seen_ms, reachable_addr) \
             VALUES (?1, ?2, 'agent-worker', 'linux', NULL, NULL, 1000, 1000, NULL)",
        )
        .bind(&tenant.dashboard_id)
        .bind(fp)
        .execute(database.pool())
        .await
        .expect("the worker row");
    }
}

async fn seed_sessions(database: &CoordDb, tenant: &SelfHostedTenant) {
    for (index, id) in SESSION_IDS.iter().enumerate() {
        let worker_fp = if index == 2 { WORKER_B } else { WORKER_A };
        sqlx::query(
            "INSERT INTO sessions (id, dashboard_id, worker_fp, channel, kind, cwd, workspace_id, \
                                   status, created_at, closed_at, custom_title, spawn_cwd) \
             VALUES (?1, ?2, ?3, ?4, 'shell', '/tmp', NULL, 'open', 1000, NULL, NULL, '/tmp')",
        )
        .bind(id)
        .bind(&tenant.dashboard_id)
        .bind(worker_fp)
        .bind(10_i64 + i64::try_from(index).expect("an index"))
        .execute(database.pool())
        .await
        .expect("the session row");
    }
}

/// A browser device of the one account.
fn browser_caller(tenant: &SelfHostedTenant) -> Caller {
    Caller {
        principal: Principal::AccountDevice {
            fingerprint: "agent-fixture-device".to_owned(),
            label: "agent fixture".to_owned(),
            account_id: tenant.account_id.clone(),
        },
        tab_id: Some("agent-fixture-tab".to_owned()),
        remote_address: Some("127.0.0.1".to_owned()),
        on_host: true,
        listener_trust: ListenerTrust::DirectLoopback,
    }
}

/// A worker identity, which no device-scoped method may answer.
fn worker_caller() -> Caller {
    Caller {
        principal: Principal::Worker {
            fingerprint: WORKER_A.to_owned(),
            label: "agent fixture worker".to_owned(),
        },
        tab_id: None,
        remote_address: Some("127.0.0.1".to_owned()),
        on_host: true,
        listener_trust: ListenerTrust::DirectLoopback,
    }
}
