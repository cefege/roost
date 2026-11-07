//! Boot step 5: `serve::prepare_coordinator_database` imports the operator's
//! `authorized_keys` file after migrations and before the tenancy invariant.
//!
//! Ports the behaviour of `apps/coord/src/auth/authorized-keys.ts`
//! (`parseSshEd25519Line`, `importAuthorizedKeys`) and its boot call in
//! `apps/coord/src/main.ts:73-80`; v2 has no dedicated test file for either.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use base64::Engine as _;
use roost_coord::auth::authorized_keys::{fingerprint_of_raw_public_key, parse_ssh_ed25519_line};
use roost_coord::serve::prepare_coordinator_database;
use roost_host::{CoordConfig, CoordConfigInput, DatabaseLocation};

mod db_support;

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-boot-authorized-keys-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        Self { root }
    }

    fn keys_path(&self) -> PathBuf {
        self.root.join("authorized_keys")
    }

    async fn config(&self) -> CoordConfig {
        CoordConfig::parse(CoordConfigInput {
            database: Some(db_support::test_database_location(&self.root).await),
            authorized_keys_path: Some(self.keys_path()),
            log_dir: Some(self.root.join("logs")),
            ..CoordConfigInput::default()
        })
        .expect("a coordinator config")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The OpenSSH wire blob `<u32 11><"ssh-ed25519"><u32 32><key>`, base64.
fn ssh_wire_blob(public_key: &[u8; 32]) -> String {
    let mut wire = Vec::with_capacity(51);
    wire.extend_from_slice(&11_u32.to_be_bytes());
    wire.extend_from_slice(b"ssh-ed25519");
    wire.extend_from_slice(&32_u32.to_be_bytes());
    wire.extend_from_slice(public_key);
    base64::engine::general_purpose::STANDARD.encode(wire)
}

async fn key_labels(config: &CoordConfig) -> Vec<(String, String)> {
    let database = roost_coord::db::open(&config.database)
        .await
        .expect("reopen");
    let rows = sqlx::query_as("SELECT fingerprint, label FROM authorized_keys ORDER BY label")
        .fetch_all(database.pool())
        .await
        .expect("the imported keys");
    database.pool().close().await;
    rows
}

async fn device_fingerprints(location: &DatabaseLocation) -> Vec<String> {
    let database = roost_coord::db::open(location).await.expect("reopen");
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT fingerprint FROM account_devices ORDER BY fingerprint")
            .fetch_all(database.pool())
            .await
            .expect("the device rows");
    database.pool().close().await;
    rows.into_iter().map(|(fingerprint,)| fingerprint).collect()
}

/// `parseSshEd25519Line`: the label is every field after the blob joined by one
/// space, and a line with none is `(no label)`.
#[test]
fn a_well_formed_line_parses_its_key_and_label() {
    let key = [7_u8; 32];
    let parsed = parse_ssh_ed25519_line(&format!(
        "  ssh-ed25519 {}  laptop   work \r",
        ssh_wire_blob(&key)
    ))
    .expect("a well-formed line");
    assert_eq!(parsed.public_key, key);
    assert_eq!(parsed.label, "laptop work");

    let bare = parse_ssh_ed25519_line(&format!("ssh-ed25519 {}", ssh_wire_blob(&key)))
        .expect("a line with no label");
    assert_eq!(bare.label, "(no label)");
}

/// `parseSshEd25519Line` returns null for blank lines, comments, other key
/// types, a missing blob, and a blob too short to be the wire document (a bare
/// 32-byte key included).
#[test]
fn lines_that_are_not_ed25519_keys_are_skipped() {
    let short = base64::engine::general_purpose::STANDARD.encode([9_u8; 32]);
    for line in [
        String::new(),
        "   ".to_string(),
        format!("# ssh-ed25519 {}", ssh_wire_blob(&[1; 32])),
        format!("ssh-rsa {} rsa-key", ssh_wire_blob(&[1; 32])),
        "ssh-ed25519".to_string(),
        "ssh-ed25519 !!!!".to_string(),
        format!("ssh-ed25519 {short} bare-key"),
    ] {
        assert_eq!(parse_ssh_ed25519_line(&line), None, "{line:?} is not a key");
    }
}

/// `importAuthorizedKeys` at boot (`main.ts:73-80`): invalid lines are skipped,
/// a key listed twice keeps one row that takes the later label, a revoked key
/// is never resurrected, and with no account yet no device row is written.
#[tokio::test]
async fn boot_imports_the_file_deduping_and_skipping_invalid_lines() {
    let scratch = Scratch::new("import");
    let config = scratch.config().await;
    let revoked_key = [3_u8; 32];
    let database = roost_coord::db::open(&config.database)
        .await
        .expect("a database");
    sqlx::query(
        "INSERT INTO authorized_key_revocations (fingerprint, revoked_at_ms, revoked_by_fp, reason) \
         VALUES ($1, 1, 'operator', 'lost')",
    )
    .bind(fingerprint_of_raw_public_key(&revoked_key))
    .execute(database.pool())
    .await
    .expect("a revocation");
    database.pool().close().await;

    let (first, second) = ([1_u8; 32], [2_u8; 32]);
    let file = [
        "# operator keys".to_string(),
        format!("ssh-ed25519 {} first-old", ssh_wire_blob(&first)),
        "ssh-ed25519 not-a-key broken".to_string(),
        format!("ssh-ed25519 {} second", ssh_wire_blob(&second)),
        format!("ssh-ed25519 {} revoked", ssh_wire_blob(&revoked_key)),
        format!("ssh-ed25519 {} first-new", ssh_wire_blob(&first)),
    ]
    .join("\r\n");
    std::fs::write(scratch.keys_path(), file).expect("the keys file");

    let (database, _tenant) = prepare_coordinator_database(&config, 10)
        .await
        .expect("boot");
    database.pool().close().await;

    assert_eq!(
        key_labels(&config).await,
        vec![
            (
                fingerprint_of_raw_public_key(&first),
                "first-new".to_string()
            ),
            (fingerprint_of_raw_public_key(&second), "second".to_string()),
        ]
    );
    assert_eq!(
        device_fingerprints(&config.database).await,
        Vec::<String>::new()
    );
}

/// `importAuthorizedKeys` with exactly one active account: every imported key
/// that is not a worker becomes that account's device. The account exists from
/// the second boot on, because the import runs BEFORE the tenancy invariant.
#[tokio::test]
async fn a_later_boot_makes_imported_non_worker_keys_devices_of_the_one_account() {
    let scratch = Scratch::new("devices");
    let config = scratch.config().await;
    let (browser, worker) = ([4_u8; 32], [5_u8; 32]);
    let file = format!(
        "ssh-ed25519 {} browser\nssh-ed25519 {} worker\n",
        ssh_wire_blob(&browser),
        ssh_wire_blob(&worker)
    );
    std::fs::write(scratch.keys_path(), file).expect("the keys file");

    let (database, tenant) = prepare_coordinator_database(&config, 10)
        .await
        .expect("first boot");
    sqlx::query(
        "INSERT INTO workers (fp, label, os, registered_at_ms, last_seen_ms, dashboard_id) \
         VALUES ($1, 'worker', 'linux', 1, 1, $2)",
    )
    .bind(fingerprint_of_raw_public_key(&worker))
    .bind(&tenant.dashboard_id)
    .execute(database.pool())
    .await
    .expect("a worker row for the second key");
    database.pool().close().await;

    let (database, _tenant) = prepare_coordinator_database(&config, 20)
        .await
        .expect("second boot");
    database.pool().close().await;

    assert_eq!(
        device_fingerprints(&config.database).await,
        vec![fingerprint_of_raw_public_key(&browser)]
    );
}

/// `main.ts:73-80`: an absent file is not imported, and a file that cannot be
/// read is a warning (`authorized_keys_import_failed`), never a failed boot.
#[tokio::test]
async fn an_absent_or_unreadable_keys_file_does_not_stop_boot() {
    let scratch = Scratch::new("unreadable");
    let config = scratch.config().await;
    let (database, _tenant) = prepare_coordinator_database(&config, 10)
        .await
        .expect("boot without a keys file");
    database.pool().close().await;

    std::fs::create_dir_all(scratch.keys_path()).expect("a directory where the file should be");
    let (database, _tenant) = prepare_coordinator_database(&config, 20)
        .await
        .expect("boot over an unreadable keys file");
    database.pool().close().await;
    assert_eq!(key_labels(&config).await, Vec::new());
}
