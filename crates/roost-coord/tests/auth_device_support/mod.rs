// The fixture the auth-device tests drive: a migrated database with the
// self-hosted tenant, a booted `CoordCore` over it, and a SECOND core on the
// same file for the tests that need two connections rather than one.
//
// The second core is the point of this file. `CoordDb` is a pool of ONE (see
// `db.rs`), so a redemption race driven through one core is a race the pool
// serialised before it started, and a claim that read-then-wrote would pass it.
// Two handles on one file are two real connections, which is the only way a
// test can observe whether the claim decides single-use inside its statement or
// before it.

// `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
// test is its own crate rather than a module of one, so the exemption has to be
// stated here rather than inherited.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;

use base64::engine::general_purpose;
use base64::prelude::Engine as _;
use connectrpc::ConnectError;
use roost_coord::auth::authorized_keys::fingerprint_of_raw_public_key;
use roost_coord::auth::bootstrap_tokens::BootstrapTokenKind;
use roost_coord::auth::principal::Principal;
use roost_coord::auth::rpc_bootstrap::{
    handle_auth_mint_bootstrap, handle_auth_redeem_browser, handle_auth_redeem_worker,
};
use roost_coord::coord_core::boot_facts::BootFacts;
use roost_coord::coord_core::{Caller, CoordCore, ListenerTrust};
use roost_coord::db::CoordDb;
use roost_coord::services::CoordServices;
use roost_host::{CoordConfig, CoordConfigInput};
use roost_proto as proto;
use sqlx::AssertSqlSafe;

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
    /// two redemptions is a race SQLite has to resolve rather than one the pool
    /// queued.
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

    /// Enroll a paired browser: an authorized key and its account device. The
    /// key bytes are derived from the fingerprint, so a test that later revokes
    /// this device has a row holding the key it enrolled with.
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

fn caller(principal: Principal) -> Caller {
    Caller {
        principal,
        tab_id: None,
        remote_address: Some("127.0.0.1".to_owned()),
        on_host: true,
        listener_trust: ListenerTrust::DirectLoopback,
    }
}

/// The caller a public redemption arrives with: no credential, which is what
/// `authRedeem*` is reached by.
pub fn anonymous() -> Caller {
    Caller {
        principal: Principal::LegacySelfHosted {
            fingerprint: String::new(),
            label: String::new(),
        },
        tab_id: None,
        remote_address: Some("198.51.100.4".to_owned()),
        on_host: false,
        listener_trust: ListenerTrust::Forwarded,
    }
}

/// A 32-byte public key derived from a label, so two tests never collide and the
/// fingerprint computed from it is a real one.
pub fn public_key_for(label: &str) -> [u8; 32] {
    let digest: [u8; 32] = <sha2::Sha256 as sha2::Digest>::digest(label.as_bytes()).into();
    digest
}

/// The base64 a browser or worker would send for `label`'s key.
///
/// `STANDARD_NO_PAD` is an ENCODER here, and the decode leniency question does
/// not arise: encoding has no trailing bits to lose, and every base64 encoder
/// emits the same string for the same bytes. The lenient DECODER -- the one that
/// accepts trailing bits, which v2's `Buffer.from(x, "base64")` does and every
/// shipped `STANDARD_*` engine documents it does NOT -- is the other half of the
/// same value and it lives in `bootstrap_tokens::decode_ed25519_pubkey`, which
/// builds its own engine for exactly that reason. One owner per direction.
pub fn pubkey_b64(label: &str) -> String {
    general_purpose::STANDARD_NO_PAD.encode(public_key_for(label))
}

/// The fingerprint `label`'s key resolves to.
pub fn fingerprint_for(label: &str) -> String {
    fingerprint_of_raw_public_key(&public_key_for(label))
}

/// A browser redemption of `token` by `label`'s key.
pub fn redeem_browser(
    token: &str,
    label: &str,
    device_label: &str,
) -> proto::AuthRedeemBrowserRequest {
    proto::AuthRedeemBrowserRequest {
        token: token.to_owned(),
        ssh_pubkey_b64: pubkey_b64(label),
        label: device_label.to_owned(),
        ..Default::default()
    }
}

/// A worker redemption of `token` by `label`'s key.
pub fn redeem_worker(
    token: &str,
    label: &str,
    worker_label: &str,
) -> proto::AuthRedeemWorkerRequest {
    // `..Default::default()` covers `__buffa_unknown_fields`, the sink buffa
    // generates on every message. Naming the generated field instead would be
    // reading a compiler artefact.
    proto::AuthRedeemWorkerRequest {
        token: token.to_owned(),
        ssh_pubkey_b64: pubkey_b64(label),
        label: worker_label.to_owned(),
        os: "linux".to_owned(),
        git_sha: Some("test-sha".to_owned()),
        ..Default::default()
    }
}

/// Mint a grant through the handler, as a paired device would.
pub async fn mint_via_handler(
    core: &CoordCore,
    minter: &Caller,
    kind: &str,
    label: &str,
) -> String {
    handle_auth_mint_bootstrap(
        core,
        minter,
        proto::AuthMintBootstrapRequest {
            kind: kind.to_owned(),
            label: label.to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect("a minted grant")
    .body
    .token
}

/// Mint a grant straight against the state layer, for the tests that are about
/// the token rather than about the RPC.
pub async fn mint_grant(
    database: &CoordDb,
    tenancy: (&str, &str),
    kind: BootstrapTokenKind,
    label: &str,
    minter: Option<&str>,
) -> String {
    roost_coord::auth::bootstrap_tokens::mint_bootstrap_token(
        database, kind, label, tenancy.0, tenancy.1, minter, 2_000,
    )
    .await
    .expect("a grant")
    .token
}

/// Mint a grant nobody is accountable for, the way `roost quickstart` does.
pub async fn mint_host_grant(database: &CoordDb, kind: BootstrapTokenKind, label: &str) -> String {
    roost_coord::auth::bootstrap_tokens::mint_host_bootstrap_token(database, kind, label, 2_000)
        .await
        .expect("a host grant")
        .token
}

/// Redeem a browser grant through the handler.
pub async fn redeem_browser_via_handler(
    core: &CoordCore,
    request: proto::AuthRedeemBrowserRequest,
) -> Result<proto::AuthRedeemBrowserResponse, ConnectError> {
    // A handler answers `ServiceResult<T>`, which is connectrpc's
    // `Result<Response<T>, ConnectError>` -- the envelope, not the message. The
    // fixture promises the message, so the envelope is unwrapped here rather
    // than in every caller.
    handle_auth_redeem_browser(core, &anonymous(), request)
        .await
        .map(|response| response.body)
}

/// Redeem a worker grant through the handler.
pub async fn redeem_worker_via_handler(
    core: &CoordCore,
    request: proto::AuthRedeemWorkerRequest,
) -> Result<proto::AuthRedeemWorkerResponse, ConnectError> {
    handle_auth_redeem_worker(core, &anonymous(), request)
        .await
        .map(|response| response.body)
}
