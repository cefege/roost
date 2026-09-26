//! The boot-facts wiring-fault rule: a fact this process never established is
//! refused BY NAME, and a database full of rows does not make it present.
//!
//! The distinction this file guards is the one a default would erase. A
//! coordinator built by `CoordServices::new` sits on a real, migrated database
//! with a real dashboard in it; a handler that fell back to "no tenant" or
//! "the default config" would answer a browser with a belief about the
//! deployment that the deployment never stated. The refusal has to name the
//! fact, and it has to be `Internal`, because the request is well-formed and
//! the coordinator is misassembled.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;

use connectrpc::ErrorCode;
use roost_coord::coord_core::BootFacts;
use roost_coord::services::CoordServices;

/// A migrated coordinator database in a scratch directory, removed on drop.
struct Scratch {
    root: PathBuf,
    database: roost_coord::db::CoordDb,
}

impl Scratch {
    async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-boot-facts-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = roost_coord::db::open(&root.join("coord.db"))
            .await
            .expect("a migrated coordinator database");
        Self { root, database }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn config(root: &std::path::Path) -> roost_host::CoordConfig {
    roost_host::CoordConfig::parse(roost_host::CoordConfigInput {
        db_path: Some(root.join("coord.db")),
        authorized_keys_path: Some(root.join("authorized_keys")),
        log_dir: Some(root.join("logs")),
        ..Default::default()
    })
    .expect("a coordinator config")
}

/// Every fact this process never established is refused with its own name, and
/// with a status a client cannot act on.
#[test]
fn an_unbooted_coordinator_names_the_fact_it_lacks() {
    let facts = BootFacts::unbooted();

    let tenant = facts
        .require_tenant()
        .expect_err("an unbooted set has no tenant");
    assert_eq!(tenant.code, ErrorCode::Internal);
    assert_eq!(
        tenant.message.as_deref(),
        Some("coordinator booted without tenant"),
        "the refusal names the fact so the log line says which boot step is missing"
    );

    let missing_config = facts
        .require_config()
        .expect_err("an unbooted set has no config");
    assert_eq!(missing_config.code, ErrorCode::Internal);
    assert_eq!(
        missing_config.message.as_deref(),
        Some("coordinator booted without config")
    );

    // The two optional facts are the only ones with a value-shaped absence; the
    // other two are empty and zero, and reading them as facts is exactly what
    // an unbooted set is for.
    assert_eq!(facts.process_epoch(), "");
    assert_eq!(facts.boot_ms(), 0);
}

/// A real database and a real tenancy scope, and the facts still absent: the
/// refusal is about THIS process, not about the deployment.
#[tokio::test]
async fn a_populated_database_does_not_make_a_boot_fact_present() {
    let scratch = Scratch::new("unbooted-with-tenancy").await;
    let tenant =
        roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant(&scratch.database, 0)
            .await
            .expect("a self-hosted tenant");

    let services = CoordServices::new(scratch.database.clone());
    let refused = services
        .boot
        .require_tenant()
        .expect_err("an unbooted coordinator has no fact, tenancy row or not");

    assert_eq!(refused.code, ErrorCode::Internal);
    assert_eq!(
        refused.message.as_deref(),
        Some("coordinator booted without tenant")
    );
    assert!(
        !tenant.dashboard_id.is_empty(),
        "the refusal is not because the deployment has no dashboard"
    );
}

/// What boot established is what a handler gets back, through the same accessor
/// the unbooted case refuses with.
#[tokio::test]
async fn a_booted_coordinator_hands_back_what_boot_established() {
    let scratch = Scratch::new("booted").await;
    let tenant =
        roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant(&scratch.database, 0)
            .await
            .expect("a self-hosted tenant");
    let resolved = config(&scratch.root);
    let dashboard_id = tenant.dashboard_id.clone();

    let services = CoordServices::booted(
        scratch.database.clone(),
        BootFacts {
            tenant: Some(tenant),
            config: Some(Arc::new(resolved.clone())),
            process_epoch: "epoch-1".to_owned(),
            boot_ms: 4_242,
        },
    );

    assert_eq!(
        services
            .boot
            .require_tenant()
            .expect("a booted tenant")
            .dashboard_id,
        dashboard_id
    );
    assert_eq!(
        services
            .boot
            .require_config()
            .expect("a booted config")
            .db_path,
        resolved.db_path,
        "a handler reads the config boot resolved, not a copy it defaulted"
    );
    assert_eq!(services.boot.process_epoch(), "epoch-1");
    assert_eq!(services.boot.boot_ms(), 4_242);
}
