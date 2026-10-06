// Included by many test files, each using a different subset of it.
#![allow(dead_code)]

//! The coordinator database every behaviour test opens: a SQLite file in the
//! test's scratch directory, or — when `ROOST_TEST_DATABASE_URL` names a
//! Postgres server — a database of its own on that server.
//!
//! Included per test file with `mod db_support;`. A fixture that used to open
//! `root.join("coord.db")` asks [`open_test_database`] instead, so one suite
//! runs against both backends. Tests that assert file-level behaviour (backups,
//! export, the migration history of a file) keep opening a SQLite path.

use std::path::Path;

use roost_coord::db::{CoordDb, DbError};
use roost_host::DatabaseLocation;
use sha2::{Digest, Sha256};
use sqlx::Any;
use sqlx::migrate::MigrateDatabase as _;
use sqlx::pool::PoolConnection;

/// A `postgres://user:password@host:port/<any-db>` URL, no query string, for a
/// role allowed to create databases. Unset or blank, every test uses SQLite.
pub const TEST_DATABASE_URL_ENV: &str = "ROOST_TEST_DATABASE_URL";

/// Postgres truncates identifiers past 63 bytes; the readable tail of a name
/// stays well under that once the prefix and the digest are added.
const NAME_TAIL_MAX: usize = 32;

/// The file in a test's scratch directory that records its Postgres database
/// was created for this run. Fixtures recreate their directory per test, so
/// its absence means "first open": the database is dropped and created fresh,
/// and a name left over from an earlier run never leaks rows into this one.
const CREATED_MARKER: &str = ".roost-test-postgres";

/// Where the test that owns `root` keeps its database.
///
/// On Postgres the database is created on first use and reused after, so a
/// test that reopens its database sees its own rows. The name is a digest of
/// `root`, which every fixture already makes unique per test.
pub async fn test_database_location(root: &Path) -> DatabaseLocation {
    let Some(base) = std::env::var(TEST_DATABASE_URL_ENV)
        .ok()
        .filter(|url| !url.trim().is_empty())
    else {
        return DatabaseLocation::SqliteFile(root.join("coord.db"));
    };
    let url = scratch_url(&base, &database_name(root));
    let marker = root.join(CREATED_MARKER);
    if !marker.exists() {
        sqlx::any::install_default_drivers();
        let _ = sqlx::Any::force_drop_database(&url).await;
        sqlx::Any::create_database(&url)
            .await
            .unwrap_or_else(|error| panic!("the scratch database could not be created: {error}"));
        std::fs::create_dir_all(root).expect("the scratch directory");
        std::fs::write(&marker, url.as_bytes()).expect("the scratch marker");
    }
    DatabaseLocation::Postgres(url)
}

/// Open (creating and migrating) the database of the test that owns `root`.
pub async fn open_test_database(root: &Path) -> Result<CoordDb, DbError> {
    roost_coord::db::open(&test_database_location(root).await).await
}

/// Whether the suite is running against Postgres, for the few assertions that
/// are about one backend's own behaviour.
pub fn running_on_postgres() -> bool {
    std::env::var(TEST_DATABASE_URL_ENV).is_ok_and(|url| !url.trim().is_empty())
}

/// Take every connection the pool may open, so no statement can start until
/// the returned guards drop — a stalled store, on either backend (SQLite's
/// pool is one connection, Postgres' several).
pub async fn hold_every_connection(database: &CoordDb) -> Vec<PoolConnection<Any>> {
    let size = database.pool().options().get_max_connections();
    let mut held = Vec::new();
    for _ in 0..size {
        held.push(
            database
                .pool()
                .acquire()
                .await
                .expect("a pooled connection"),
        );
    }
    held
}

/// Insert one `authorized_keys` row with `public_key` bound as bytes.
///
/// A hex blob literal is not portable — SQLite reads `x'…'` as a blob,
/// Postgres as a bit string — so a fixture that writes key rows binds them.
pub async fn insert_authorized_key(
    database: &CoordDb,
    fingerprint: &str,
    public_key: &[u8],
    label: &str,
    paired_from: Option<(&str, &str)>,
) {
    let (ip, country) = paired_from.unzip();
    sqlx::query(
        "INSERT INTO authorized_keys \
         (fingerprint, public_key, label, added_at, paired_from_ip, paired_country) \
         VALUES ($1, $2, $3, 1000, $4, $5)",
    )
    .bind(fingerprint)
    .bind(public_key)
    .bind(label)
    .bind(ip)
    .bind(country)
    .execute(database.pool())
    .await
    .expect("an authorized key row");
}

/// Make `table` refuse every row `condition` matches on `event` (`INSERT`,
/// `UPDATE` or `DELETE`), with `message`, in the backend's trigger dialect.
///
/// `condition` spells the row as `NEW.`/`OLD.`, which both dialects share; the
/// trigger is a fault injected by a test, and dropping it is `DROP TRIGGER
/// <name>` on SQLite and `DROP TRIGGER <name> ON <table>` on Postgres.
pub async fn install_refusing_trigger(
    database: &CoordDb,
    name: &str,
    event: &str,
    table: &str,
    condition: &str,
    message: &str,
) {
    let statements = if running_on_postgres() {
        let returned = if event == "DELETE" { "OLD" } else { "NEW" };
        vec![
            format!(
                "CREATE FUNCTION {name}() RETURNS trigger LANGUAGE plpgsql AS $body$ \
                 BEGIN IF {condition} THEN RAISE EXCEPTION '{message}'; END IF; \
                 RETURN {returned}; END $body$"
            ),
            format!(
                "CREATE TRIGGER {name} BEFORE {event} ON {table} FOR EACH ROW \
                 EXECUTE FUNCTION {name}()"
            ),
        ]
    } else {
        vec![format!(
            "CREATE TRIGGER {name} BEFORE {event} ON {table} WHEN {condition} \
             BEGIN SELECT RAISE(ABORT, '{message}'); END"
        )]
    };
    for statement in statements {
        sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
            .execute(database.pool())
            .await
            .expect("the refusing trigger installs");
    }
}

/// Remove a trigger [`install_refusing_trigger`] made.
pub async fn drop_refusing_trigger(database: &CoordDb, name: &str, table: &str) {
    let statement = if running_on_postgres() {
        format!("DROP TRIGGER {name} ON {table}")
    } else {
        format!("DROP TRIGGER {name}")
    };
    sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
        .execute(database.pool())
        .await
        .expect("the refusing trigger drops");
}

fn database_name(root: &Path) -> String {
    let rendered = root.to_string_lossy();
    let digest = hex::encode(Sha256::digest(rendered.as_bytes()));
    let tail: String = root
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .take(NAME_TAIL_MAX)
        .collect();
    format!("roost_t_{}_{tail}", &digest[..16])
}

fn scratch_url(base: &str, database: &str) -> String {
    let Some((server, _)) = base
        .rsplit_once('/')
        .filter(|(server, _)| server.contains("://"))
    else {
        panic!("{TEST_DATABASE_URL_ENV} must be postgres://user:password@host:port/database");
    };
    format!("{server}/{database}")
}
