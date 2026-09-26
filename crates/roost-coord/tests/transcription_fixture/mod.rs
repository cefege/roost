//! The transcription tests' shared fixture: a migrated coordinator database
//! with the one self-hosted tenant, reached the way `serve` reaches it.
//!
//! Owned by the transcription tests. The probe sender is the part that matters:
//! a reachability probe is the one place this domain waits on a third party, and
//! a test that reached Deepgram would prove nothing about the lifecycle.

#![allow(clippy::unwrap_used, clippy::expect_used, dead_code, unused_imports)]

use std::path::PathBuf;
use std::sync::Arc;

use roost_coord::auth::principal::Principal;
use roost_coord::auth::self_hosted_tenant::SelfHostedTenant;
use roost_coord::coord_core::boot_facts::BootFacts;
use roost_coord::coord_core::{Caller, CoordCore, ListenerTrust};
use roost_coord::db::CoordDb;
use roost_coord::diagnostics::transcription::{
    ProbeFuture, ProbeSender, ProviderProbe, ProviderRefusal,
};
use roost_coord::services::CoordServices;

/// A key long enough that a mask cannot show all of it.
pub const STORED_KEY: &str = "deepgram-live-9f2c-secret-abcd";

/// The last four characters of [`STORED_KEY`], which is all a mask may show.
pub const KEY_TAIL: &str = "abcd";

/// A coordinator booted with a tenant, and a browser to call it as.
pub struct TranscriptionFixture {
    /// The coordinator's shared state, as a handler receives it.
    pub core: CoordCore,
    /// The one dashboard every transcription row is stamped with.
    pub dashboard_id: String,
    root: PathBuf,
}

impl TranscriptionFixture {
    /// A fixture over a fresh database with nothing stored.
    pub async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-transcription-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = roost_coord::db::open(&root.join("coord.db"))
            .await
            .expect("a migrated coordinator database");
        let tenant: SelfHostedTenant =
            roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant(&database, 0)
                .await
                .expect("a self-hosted tenant");
        // The handlers read the tenancy scope from the boot facts rather than
        // taking it as an argument, so a fixture that left them unbooted would
        // be testing a wiring fault instead of the domain.
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

    /// The coordinator's database handle.
    pub fn database(&self) -> &CoordDb {
        &self.core.services.db
    }

    /// An authenticated browser, the only principal these four methods answer.
    pub fn browser(&self) -> Caller {
        Caller {
            principal: Principal::AccountDevice {
                fingerprint: "a".repeat(64),
                label: "transcription test".to_owned(),
                account_id: "account-under-test".to_owned(),
            },
            tab_id: None,
            remote_address: Some("127.0.0.1".to_owned()),
            on_host: true,
            listener_trust: ListenerTrust::DirectLoopback,
        }
    }

    /// A machine, which is the principal these four methods refuse.
    pub fn worker(&self) -> Caller {
        Caller {
            principal: Principal::Worker {
                fingerprint: "b".repeat(64),
                label: "transcription test worker".to_owned(),
            },
            tab_id: None,
            remote_address: Some("127.0.0.1".to_owned()),
            on_host: true,
            listener_trust: ListenerTrust::DirectLoopback,
        }
    }

    /// Answer probes through `sender` instead of over HTTPS.
    pub fn probe_with(&self, sender: ProbeSender) {
        self.core
            .services
            .telemetry
            .transcription
            .set_probe_sender(sender);
    }

    /// The probe's state, as an observer sees it.
    pub fn probe_state(&self) -> ProviderProbe {
        self.core
            .services
            .telemetry
            .transcription
            .provider_probe()
    }

    /// Store a key under a dashboard that is not this coordinator's.
    ///
    /// The read is tenant-scoped, and this row is what proves it: a prefix read
    /// would answer with `foreign-key-wxyz`, and a browser would then hold a
    /// credential this deployment never issued.
    pub async fn seed_foreign_key(&self) {
        let organization_id: String =
            sqlx::query_scalar("SELECT organization_id FROM dashboards WHERE id = ?1")
                .bind(&self.dashboard_id)
                .fetch_one(self.database().pool())
                .await
                .expect("the tenant's organization");
        sqlx::query(
            "INSERT INTO dashboards (id, organization_id, slug, name, status, created_at_ms) \
             VALUES ('other-dashboard', ?1, 'other', 'Other', 'active', 0)",
        )
        .bind(&organization_id)
        .execute(self.database().pool())
        .await
        .expect("the other dashboard");
        sqlx::query(
            "INSERT INTO app_settings (dashboard_id, key, value, updated_at_ms) \
             VALUES ('other-dashboard', 'transcription.deepgram_key', 'foreign-key-wxyz', 0)",
        )
        .execute(self.database().pool())
        .await
        .expect("the other dashboard's key");
    }
}

impl Drop for TranscriptionFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A provider that accepts the key.
pub fn accepting() -> ProbeSender {
    Arc::new(|_key: String| -> ProbeFuture { Box::pin(async { Ok(()) }) })
}

/// A provider that answers, and refuses with `reason`.
pub fn refusing(reason: &str) -> ProbeSender {
    let reason = reason.to_owned();
    Arc::new(move |_key: String| -> ProbeFuture {
        let reason = reason.clone();
        Box::pin(async move { Err(ProviderRefusal::Refused(reason)) })
    })
}

/// A provider that never answers.
///
/// The future parks rather than resolving, which is the case the deadline in
/// `transcription.rs` exists for: a third party holding the request open is
/// indistinguishable from a working one until something bounds the wait.
pub fn never_answering() -> ProbeSender {
    Arc::new(|_key: String| -> ProbeFuture {
        Box::pin(std::future::pending::<Result<(), ProviderRefusal>>())
    })
}
