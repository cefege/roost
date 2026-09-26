//! The shared fixture for the audit hook tests: a coordinator with a migrated
//! database, a booted tenancy scope, a bus this test owns, and readers for the
//! rows and the published messages.
//!
//! Owned by the audit slice, split out for the 400-line cap. One fixture rather
//! than ten: a test file that re-states its own database is a second answer to
//! "which tenant scoped this row".

// `unwrap`/`expect` are denied outside `#[cfg(test)]`, and an integration test
// is its own crate rather than a module of one, so the exemption is stated here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use roost_coord::auth::self_hosted_tenant::SelfHostedTenant;
use roost_coord::coord_core::CoordCore;
use roost_coord::coord_core::boot_facts::BootFacts;
use roost_coord::db::CoordDb;
use roost_coord::events::bus::Subscription;
use roost_coord::events::bus_messages::AuditRow;
use roost_coord::services::CoordServices;

/// The service and procedure every Connect row names, spelled the way the proto
/// does and the way `audit_log` has always stored it.
pub const SERVICE: &str = "roost.v1.CoordinatorService";
pub const KILL: &str = "SessionsKill";
pub const CALLER: &str = "fp-device-1";
pub const DASHBOARD: &str = "dash_audit_test";

/// A coordinator with a migrated database, a tenant, and a bus this test owns.
pub struct AuditFixture {
    pub core: CoordCore,
    database: CoordDb,
    root: PathBuf,
    published: Arc<Mutex<Vec<AuditRow>>>,
    _subscription: Subscription<AuditRow>,
}

impl AuditFixture {
    /// A coordinator that booted with a tenancy scope.
    pub async fn new(label: &str) -> Self {
        Self::build(label, true).await
    }

    /// A coordinator whose boot facts are empty, which is the only way a row
    /// reaches the table unscoped.
    pub async fn unbooted(label: &str) -> Self {
        Self::build(label, false).await
    }

    async fn build(label: &str, booted: bool) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-audit-hook-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = roost_coord::db::open(&root.join("coord.db"))
            .await
            .expect("a migrated database");
        let boot = if booted {
            BootFacts {
                tenant: Some(SelfHostedTenant {
                    account_id: "acct_audit_test".to_string(),
                    organization_id: "org_audit_test".to_string(),
                    dashboard_id: DASHBOARD.to_string(),
                }),
                ..BootFacts::default()
            }
        } else {
            BootFacts::unbooted()
        };
        let services = Arc::new(CoordServices::booted(database.clone(), boot));
        let published: Arc<Mutex<Vec<AuditRow>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&published);
        let subscription = services.buses.audit_bus.subscribe(move |row: &AuditRow| {
            sink.lock().expect("the sink mutex").push(row.clone())
        });
        Self {
            core: CoordCore::new(services),
            database,
            root,
            published,
            _subscription: subscription,
        }
    }

    /// Every row's id, path, caller, status and dashboard scope, in id order.
    pub async fn rows(&self) -> Vec<(i64, String, Option<String>, i64, Option<String>)> {
        sqlx::query_as(
            "SELECT id, path, caller_fp, status, dashboard_id FROM audit_log ORDER BY id",
        )
        .fetch_all(self.database.pool())
        .await
        .expect("audit_log answers")
    }

    /// How many rows the table holds.
    pub async fn row_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(self.database.pool())
            .await
            .expect("audit_log counts")
    }

    /// What `audit_bus` delivered, in delivery order.
    pub async fn published(&self) -> Vec<AuditRow> {
        self.published.lock().expect("the sink mutex").clone()
    }

    /// Take the table away, so the next insert is a real write failure.
    pub async fn drop_audit_table(&self) {
        sqlx::query("DROP TABLE audit_log")
            .execute(self.database.pool())
            .await
            .expect("the table drops");
    }
}

impl Drop for AuditFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
