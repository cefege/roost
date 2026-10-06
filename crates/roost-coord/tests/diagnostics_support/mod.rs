//! The diagnostics tests' shared fixture: a migrated coordinator database with
//! a self-hosted tenant, two paired browsers whose key rows the audit read
//! joins against, and the three callers these RPCs distinguish.
//!
//! Owned by `diagnostics_rpc.rs`. Compiled into that one binary, so an item
//! unused by it would be dead — everything here is used.
//!
//! `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to
//! be stated here rather than inherited.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;

use roost_coord::auth::principal::Principal;
use roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant;
use roost_coord::coord_core::{BootFacts, Caller, CoordCore, ListenerTrust};
use roost_coord::services::CoordServices;

/// The browser whose key row the fixture plants.
pub const CALLER_ONE: &str = "fp-one";

/// A second browser, so a caller filter has something to exclude.
pub const CALLER_TWO: &str = "fp-two";

/// A coordinator with the tenancy scope boot resolved, over a scratch database.
pub struct AuditFixture {
    /// The coordinator's shared state, as a handler receives it.
    pub core: CoordCore,
    /// The dashboard the audit rows belong to.
    pub dashboard_id: String,
    root: PathBuf,
}

impl AuditFixture {
    pub async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-diagnostics-{label}-{}-{:?}",
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
        for fingerprint in [CALLER_ONE, CALLER_TWO] {
            sqlx::query(
                "INSERT INTO authorized_keys (fingerprint, public_key, label, added_at) \
                 VALUES ($1, $2, $3, 0)",
            )
            .bind(fingerprint)
            .bind(vec![0_u8; 32])
            .bind(format!("{fingerprint} label"))
            .execute(database.pool())
            .await
            .expect("an authorized key row");
        }
        let dashboard_id = tenant.dashboard_id.clone();
        let services = CoordServices::booted(
            database,
            BootFacts {
                tenant: Some(tenant),
                ..BootFacts::unbooted()
            },
        );
        Self {
            core: CoordCore::new(Arc::new(services)),
            dashboard_id,
            root,
        }
    }

    /// Append one audit row, as the interceptor does for a real request.
    pub async fn record(&self, caller_fp: &str, method: &str, path: &str, status: u16) {
        sqlx::query(
            "INSERT INTO audit_log (ts, caller_fp, method, path, status, trace_id, dashboard_id) \
             VALUES ($1, $2, $3, $4, $5, NULL, $6)",
        )
        .bind(1_700_000_000_000_i64)
        .bind(caller_fp)
        .bind(method)
        .bind(path)
        .bind(i64::from(status))
        .bind(&self.dashboard_id)
        .execute(self.core.services.db.pool())
        .await
        .expect("an audit row");
    }
}

impl Drop for AuditFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A paired browser: the authority every diagnostics RPC admits.
pub fn device() -> Caller {
    caller(Principal::AccountDevice {
        fingerprint: CALLER_ONE.to_owned(),
        label: "audit pane".to_owned(),
        account_id: "account-under-test".to_owned(),
    })
}

/// A registered machine: a principal that is not the operator.
pub fn worker() -> Caller {
    caller(Principal::Worker {
        fingerprint: "diagnostics-worker".to_owned(),
        label: "a worker".to_owned(),
    })
}

/// A pre-account browser key: a browser the coordinator admits here, and the
/// reason these tests never claim to cover the "no credential" case. That
/// refusal belongs to the interceptor, before a `Caller` exists at all.
pub fn legacy_browser() -> Caller {
    caller(Principal::LegacySelfHosted {
        fingerprint: "unpaired-browser".to_owned(),
        label: "an unpaired browser".to_owned(),
    })
}

fn caller(principal: Principal) -> Caller {
    Caller {
        principal,
        tab_id: Some("tab-under-test".to_owned()),
        remote_address: Some("127.0.0.1".to_owned()),
        on_host: true,
        listener_trust: ListenerTrust::DirectLoopback,
    }
}
