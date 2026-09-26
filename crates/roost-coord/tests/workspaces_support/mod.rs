// The fixture every workspace test drives: a BOOTED coordinator over a
// temporary database, with enrolled workers, enrolled sessions, and a recording
// subscriber on the workspace bus.
//
// It is booted, unlike the workers fixture, because two of the five methods
// write a `dashboard_id` and read it from `core.services.boot` at call time --
// the rule that lets a config change be visible without a restart. A test that
// wanted the unbooted answer would get `coordinator booted without tenant`,
// which is the point of the rule, not a gap in the fixture.
//
// The bus subscription is the Sync seam, not a mock: `sync_ws/feed`'s workspace
// adapter subscribes to this bus and turns each `WorkspaceDelta` into one
// firehose frame, so a delta recorded here is what a live socket would receive.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use roost_coord::auth::principal::Principal;
use roost_coord::coord_core::boot_facts::BootFacts;
use roost_coord::coord_core::{Caller, CoordCore, ListenerTrust};
use roost_coord::db::CoordDb;
use roost_coord::events::bus::Subscription;
use roost_coord::services::CoordServices;
use roost_protocol::wire::WorkspaceDelta;
use sqlx::AssertSqlSafe;

/// A valid worker fingerprint.
pub const WORKER_FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A second machine, for the tests that need more than one.
pub const OTHER_WORKER_FP: &str =
    "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

/// The account device an operator acts as.
pub const DEVICE_FP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// Session ids, valid UUIDs because the junction brands every one it reads.
pub const SESSION_A: &str = "11111111-1111-4111-8111-111111111111";
pub const SESSION_B: &str = "22222222-2222-4222-8222-222222222222";
pub const SESSION_C: &str = "33333333-3333-4333-8333-333333333333";

/// A booted coordinator over a temporary database, plus a workspace-bus listener.
pub struct WorkspacesFixture {
    /// The shared process state, and the only way into a handler.
    pub core: CoordCore,
    /// The database handle, for the rows a test asserts on directly.
    pub database: CoordDb,
    /// The dashboard every row this fixture writes is scoped to.
    pub dashboard_id: String,
    /// The `WorkspaceDelta`s a subscriber has seen, in order.
    pub deltas: Arc<Mutex<Vec<WorkspaceDelta>>>,
    /// The subscription, held so the fixture outlives the first publish.
    _subscription: Subscription<WorkspaceDelta>,
    root: PathBuf,
}

impl WorkspacesFixture {
    /// A booted fixture with the self-hosted tenant and one enrolled worker.
    pub async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-workspaces-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = roost_coord::db::open(&root.join("coord.db"))
            .await
            .expect("a migrated database");
        let tenant = roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant(&database, 1_000)
            .await
            .expect("the self-hosted tenant");
        let services = Arc::new(CoordServices::booted(
            database.clone(),
            BootFacts {
                tenant: Some(tenant.clone()),
                process_epoch: "epoch-1".to_owned(),
                boot_ms: 1_000,
                ..Default::default()
            },
        ));
        let core = CoordCore::new(Arc::clone(&services));
        let deltas = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&deltas);
        let subscription = core.services.buses.workspace_bus.subscribe(move |delta| {
            recorded
                .lock()
                .expect("the recording lock")
                .push(delta.clone());
        });
        let fixture = Self {
            core,
            database,
            dashboard_id: tenant.dashboard_id,
            deltas,
            _subscription: subscription,
            root,
        };
        fixture.enroll_worker(WORKER_FP, "build-box").await;
        fixture
    }

    /// Run one statement against the fixture's database.
    pub async fn exec(&self, sql: &str) {
        sqlx::query(AssertSqlSafe(sql))
            .execute(self.database.pool())
            .await
            .expect("the statement applies");
    }

    /// Enroll a worker row, as a redeemed bootstrap token would have.
    pub async fn enroll_worker(&self, worker_fp: &str, label: &str) {
        self.exec(&format!(
            "INSERT INTO workers (fp, label, os, registered_at_ms, last_seen_ms, dashboard_id) \
             VALUES ('{worker_fp}', '{label}', 'linux', 1, 1, '{}')",
            self.dashboard_id
        ))
        .await;
    }

    /// Enroll one open session, in no workspace.
    pub async fn enroll_session(&self, session_id: &str, cwd: &str) {
        self.exec(&format!(
            "INSERT INTO sessions (id, worker_fp, channel, kind, cwd, status, created_at, \
             dashboard_id) VALUES ('{session_id}', '{WORKER_FP}', 1, 'shell', '{cwd}', 'open', 1, \
             '{}')",
            self.dashboard_id
        ))
        .await;
    }

    /// One integer out of a query.
    pub async fn scalar_i64(&self, sql: &str) -> i64 {
        sqlx::query_scalar::<_, i64>(AssertSqlSafe(sql))
            .fetch_one(self.database.pool())
            .await
            .expect("a scalar")
    }

    /// The workspace a session's column names, empty when it names none.
    pub async fn session_workspace_id(&self, session_id: &str) -> String {
        let row: Option<String> =
            sqlx::query_scalar("SELECT workspace_id FROM sessions WHERE id = ?")
                .bind(session_id)
                .fetch_one(self.database.pool())
                .await
                .expect("the session row");
        row.unwrap_or_default()
    }

    /// How many junction rows the workspace holds.
    pub async fn junction_rows(&self, workspace_id: &str) -> i64 {
        self.scalar_i64(&format!(
            "SELECT COUNT(*) FROM workspace_sessions WHERE workspace_id = '{workspace_id}'"
        ))
        .await
    }

    /// The deltas a subscriber has seen so far.
    pub fn recorded(&self) -> Vec<WorkspaceDelta> {
        self.deltas.lock().expect("the recording lock").clone()
    }
}

impl Drop for WorkspacesFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The account device an operator acts as.
#[must_use]
pub fn device_caller() -> Caller {
    Caller {
        principal: Principal::AccountDevice {
            fingerprint: DEVICE_FP.to_owned(),
            label: "test device".to_owned(),
            account_id: "acct_self_hosted".to_owned(),
        },
        tab_id: None,
        remote_address: None,
        on_host: true,
        listener_trust: ListenerTrust::DirectLoopback,
    }
}

/// A machine, which is not a device: the refusal the five methods all share.
#[must_use]
pub fn machine_caller() -> Caller {
    Caller {
        principal: Principal::Worker {
            fingerprint: WORKER_FP.to_owned(),
            label: "test worker".to_owned(),
        },
        tab_id: None,
        remote_address: None,
        on_host: true,
        listener_trust: ListenerTrust::DirectLoopback,
    }
}

/// A browser key from before accounts existed: authenticated, but carrying no
/// device, which is the other half of the same refusal.
#[must_use]
pub fn legacy_caller() -> Caller {
    Caller {
        principal: Principal::LegacySelfHosted {
            fingerprint: DEVICE_FP.to_owned(),
            label: "legacy browser".to_owned(),
        },
        tab_id: None,
        remote_address: None,
        on_host: true,
        listener_trust: ListenerTrust::DirectLoopback,
    }
}
