// The fixture the auth-device tests drive: a migrated database with the
// self-hosted tenant, a booted `CoordCore` over it, and a SECOND core on the
// same file. The second core is the point: `CoordDb` is a pool of ONE, so a
// redemption race through one core is a race the pool serialised before it
// started, and a claim that read-then-wrote would pass it.
//
// `expect` and `unwrap` are denied outside `#[cfg(test)]` and an integration test
// is its own crate, so the exemption is stated here rather than inherited.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;

use base64::engine::general_purpose;
use base64::prelude::Engine as _;
use roost_coord::auth::authorized_keys::fingerprint_of_raw_public_key;
use roost_coord::auth::principal::Principal;
use roost_coord::coord_core::boot_facts::BootFacts;
use roost_coord::coord_core::{Caller, CoordCore, ListenerTrust};
use roost_coord::db::CoordDb;
use roost_coord::services::CoordServices;
use roost_host::{CoordConfig, CoordConfigInput};
use sqlx::AssertSqlSafe;

/// Minting and redeeming a grant is one half of this fixture and key derivation
/// is the other; they are split because together they overflow the file cap
/// while each is a subject of its own. Re-exported here so every consumer keeps
/// the one path it already imports.
mod mints;
pub use mints::*;

/// The account device an operator acts as.
pub const DEVICE_FP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// A second account device, for the tests that revoke a peer.
pub const PEER_FP: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// A worker, for the tests that must not be able to revoke a device.
pub const WORKER_FP: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

/// The number of `authorized_keys` rows in the fixture's database.
pub const AUTHORIZED_KEY_COUNT: &str = "SELECT count(*) FROM authorized_keys";

/// The number of `account_devices` rows in the fixture's database.
pub const ACCOUNT_DEVICE_COUNT: &str = "SELECT count(*) FROM account_devices";

/// The number of `workers` rows in the fixture's database.
pub const WORKER_COUNT: &str = "SELECT count(*) FROM workers";

/// The number of unspent grants in the fixture's database.
pub const UNSPENT_GRANT_COUNT: &str =
    "SELECT count(*) FROM bootstrap_tokens WHERE used_at_ms IS NULL";

/// A migrated database with the tenant established, and a core over it.
pub struct Scratch {
    /// The core every handler is driven through.
    pub core: CoordCore,
    /// The path, so a test can open a second connection to the same file.
    pub database_path: PathBuf,
    /// The account the tenancy invariant created.
    pub account_id: String,
    /// The dashboard a redeemed worker is scoped to.
    pub dashboard_id: String,
    root: PathBuf,
}

impl Scratch {
    /// A scratch coordinator with the default config.
    pub async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-authdev-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database_path = root.join("coord.db");
        let (core, tenant) = open_core(&database_path, &root, |_| {}).await;
        Self {
            core,
            database_path,
            account_id: tenant.account_id,
            dashboard_id: tenant.dashboard_id,
            root,
        }
    }

    /// A SECOND core on the same file: a second connection, so a race between
    /// two redemptions is one SQLite has to resolve rather than one the pool queued.
    pub async fn second_core(&self) -> CoordCore {
        open_core(&self.database_path, &self.root, |_| {}).await.0
    }

    /// A third core whose config is rebuilt with `adjust` applied first.
    pub async fn core_with(
        &self,
        label: &str,
        adjust: impl FnOnce(&mut CoordConfigInput),
    ) -> CoordCore {
        let root = self.root.join(label);
        std::fs::create_dir_all(&root).expect("a scratch subdirectory");
        open_core(&self.database_path, &root, adjust).await.0
    }

    /// The database handle, for the tests that drive the state layer directly.
    pub fn database(&self) -> &CoordDb {
        &self.core.services.db
    }

    /// The tenancy scope, for a grant minted outside the RPC surface.
    pub fn tenancy(&self) -> (&str, &str) {
        (&self.account_id, &self.dashboard_id)
    }

    /// The account device an operator acts as.
    pub fn device(&self) -> Caller {
        browser(DEVICE_FP, &self.account_id)
    }

    /// A second paired browser, for the tests that revoke a peer.
    pub fn peer(&self) -> Caller {
        browser(PEER_FP, &self.account_id)
    }

    /// Run one statement against the fixture's database.
    pub async fn exec(&self, sql: &str) {
        sqlx::query(AssertSqlSafe(sql))
            .execute(self.core.services.db.pool())
            .await
            .expect("the statement applies");
    }

    /// One integer out of a query.
    pub async fn scalar(&self, sql: &str) -> i64 {
        sqlx::query_scalar::<_, i64>(AssertSqlSafe(sql))
            .fetch_one(self.core.services.db.pool())
            .await
            .expect("a scalar")
    }

    /// Enroll a paired browser: an authorized key and its account device. The key
    /// bytes are derived from the fingerprint, so a revoked device still has a row.
    pub async fn enroll_device(&self, fingerprint: &str, label: &str) {
        let public_key = public_key_for(fingerprint);
        self.exec(&format!(
            "INSERT INTO authorized_keys (fingerprint, public_key, label, added_at, \
             paired_from_ip, paired_country) VALUES ('{fingerprint}', x'{}', '{label}', 1000, \
             '203.0.113.9', 'SE')",
            hex::encode(public_key),
        ))
        .await;
        self.exec(&format!(
            "INSERT INTO account_devices (fingerprint, account_id, added_at_ms, last_seen_at_ms) \
             VALUES ('{fingerprint}', '{}', 1000, 1000)",
            self.account_id
        ))
        .await;
    }

    /// Enroll a worker, as a redeemed grant would have.
    pub async fn enroll_worker(&self, fingerprint: &str, label: &str) {
        self.exec(&format!(
            "INSERT INTO workers (fp, label, os, registered_at_ms, last_seen_ms, dashboard_id) \
             VALUES ('{fingerprint}', '{label}', 'linux', 1000, 1000, \
             (SELECT id FROM dashboards LIMIT 1))"
        ))
        .await;
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Open one connection to `database_path` and build a booted core over it.
async fn open_core(
    database_path: &std::path::Path,
    root: &std::path::Path,
    adjust: impl FnOnce(&mut CoordConfigInput),
) -> (
    CoordCore,
    roost_coord::auth::self_hosted_tenant::SelfHostedTenant,
) {
    let database = roost_coord::db::open(database_path)
        .await
        .expect("a migrated database");
    let tenant = roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant(&database, 1_000)
        .await
        .expect("the self-hosted tenant");
    let mut input = CoordConfigInput {
        db_path: Some(database_path.to_path_buf()),
        authorized_keys_path: Some(root.join("authorized_keys")),
        log_dir: Some(root.join("logs")),
        ..CoordConfigInput::default()
    };
    adjust(&mut input);
    let config = CoordConfig::parse(input).expect("a coordinator config");
    let services = Arc::new(CoordServices::booted(
        database,
        BootFacts {
            tenant: Some(tenant.clone()),
            config: Some(Arc::new(config)),
            process_epoch: "epoch-1".to_owned(),
            boot_ms: 0,
        },
    ));
    (CoordCore::new(services), tenant)
}

/// A browser acting as `fingerprint` in `account_id`.
pub fn browser(fingerprint: &str, account_id: &str) -> Caller {
    caller(Principal::AccountDevice {
        fingerprint: fingerprint.to_owned(),
        label: "test device".to_owned(),
        account_id: account_id.to_owned(),
    })
}

/// A machine acting as `fingerprint`.
pub fn worker(fingerprint: &str) -> Caller {
    caller(Principal::Worker {
        fingerprint: fingerprint.to_owned(),
        label: "test worker".to_owned(),
    })
}

/// A caller with `on_host` false, for the tests that must not be local.
pub fn browser_off_host(fingerprint: &str, account_id: &str) -> Caller {
    Caller {
        on_host: false,
        ..browser(fingerprint, account_id)
    }
}

/// A machine that is not on the coordinator's host. `DevicesRevoke` admits an
/// ON-HOST non-browser caller as `"on-host-recovery"`, so "a machine may not
/// call the operator surface" is a statement about a REMOTE one.
pub fn worker_off_host(fingerprint: &str) -> Caller {
    Caller {
        on_host: false,
        ..worker(fingerprint)
    }
}

fn caller(principal: Principal) -> Caller {
    Caller {
        principal,
        tab_id: None,
        remote_address: Some("127.0.0.1".to_owned()),
        on_host: true,
        listener_trust: ListenerTrust::DirectLoopback,
    }
}

/// A 32-byte public key derived from a label, so no two tests collide.
pub fn public_key_for(label: &str) -> [u8; 32] {
    let digest: [u8; 32] = <sha2::Sha256 as sha2::Digest>::digest(label.as_bytes()).into();
    digest
}

/// The base64 a browser or worker would send for `label`'s key.
///
/// `STANDARD_NO_PAD` is an ENCODER here, and decode leniency does not arise:
/// encoding has no trailing bits to lose. The lenient DECODER is the other half
/// of the same value and it lives in `bootstrap_tokens::decode_ed25519_pubkey`.
/// One owner per direction.
pub fn pubkey_b64(label: &str) -> String {
    general_purpose::STANDARD_NO_PAD.encode(public_key_for(label))
}

/// The fingerprint `label`'s key resolves to.
pub fn fingerprint_for(label: &str) -> String {
    fingerprint_of_raw_public_key(&public_key_for(label))
}
