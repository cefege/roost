// The applied migration history, through the real `db::open`.
//
// Two halves, and the second is the one that protects a database nothing has
// heard of. A removed migration's row is TRUE — that file did apply it — so
// reading it as corruption bricks a coordinator that was working minutes ago,
// and the operator's next move is to delete the row, which is the one thing
// `docs/FAILURE-INDEX.md` names as wrong. v2 declared the retirement and
// compared the survivors; `sqlx` keys its history by version, so the retirement
// is a version here and `db::RETIRED_MIGRATIONS` is where it is declared.
//
// Every comparison is by version, never by position in the raw history: v2's
// retired `0017_agent_ui_frames` sorts BEFORE the migration that reused its
// slot, so an ordinal comparison diverges at the position it reads from. A
// version comparison has no such failure mode, and these tests would not catch a
// regression into one — they catch a regression into the OTHER wrong answer,
// which is admitting a history this build did not write.
//
// A test that cannot say what it expected is not a test, and an integration test
// is its own crate rather than a module of one.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::path::PathBuf;

use roost_coord::db::{self, CoordDb, DbError, RETIRED_MIGRATIONS};
use roost_host::DatabaseLocation;
use sqlx::AssertSqlSafe;

mod db_support;

/// What a migrated database refuses, asserted on whichever backend
/// `ROOST_TEST_DATABASE_URL` selects: the same subject as this file's history
/// checks — the schema the migration leaves — so a submodule, not a second root.
#[path = "migration_history/backend_parity.rs"]
mod backend_parity;

/// The copy from a migrated SQLite file into a migrated Postgres database:
/// whether the two schemas this file guards carry the same rows.
#[path = "migration_history/sqlite_to_postgres.rs"]
mod sqlite_to_postgres;

/// The reverse copy, Postgres into a fresh SQLite file: the round trip lands
/// the file the forward copy started from.
#[path = "migration_history/postgres_to_sqlite.rs"]
mod postgres_to_sqlite;

/// The version the squashed schema ships as, read out of the embedded set
/// rather than restated: a test that hardcoded `1` would keep passing if the
/// squashed migration were renumbered, which is the moment the whole file's
/// reasoning about "one migration" stopped being true.
fn embedded_versions() -> HashSet<i64> {
    let migrator = sqlx::migrate!("./migrations/sqlite");
    let versions: HashSet<i64> = migrator.iter().map(|m| m.version).collect();
    assert!(
        !versions.is_empty(),
        "the coordinator embeds no migrations, so nothing here would be tested"
    );
    versions
}

/// A database file in a directory that removes itself, NOT opened: the tests
/// here decide for themselves whether `db::open` is allowed to succeed.
struct HistoryFixture {
    path: PathBuf,
    #[allow(dead_code)]
    root: PathBuf,
}

impl HistoryFixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-migration-history-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        Self {
            path: root.join("coord.db"),
            root,
        }
    }

    /// Apply the embedded set, then write one extra history row by hand — the
    /// state a file is in after a migration was removed from a build that had
    /// shipped it.
    async fn with_history_row(&self, version: i64, checksum: i64) -> CoordDb {
        let database = db::open(&DatabaseLocation::SqliteFile(self.path.clone()))
            .await
            .expect("a migrated database");
        sqlx::query(AssertSqlSafe(
            "INSERT INTO _sqlx_migrations (version, description, installed_on, \
             success, checksum, execution_time) VALUES ($1, 'gone', \
             '2026-01-01 00:00:00', 1, $2, 0)",
        ))
        .bind(version)
        .bind(checksum)
        .execute(database.pool())
        .await
        .expect("the history row applies");
        database
    }
}

impl Drop for HistoryFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The fail-closed half, end to end. A history row this build neither embeds nor
/// declares retired is a file this coordinator did not write, and it must not
/// bind a port over it — with an error that NAMES the version, because the
/// operator's next question is "which build made this" and a message reading
/// `migration embedded:` never answers it.
#[tokio::test]
async fn a_history_row_no_build_declared_is_refused_and_says_which_version() {
    let fixture = HistoryFixture::new("unknown");
    fixture.with_history_row(2_017, 0).await;

    let refusal = db::open(&DatabaseLocation::SqliteFile(fixture.path.clone()))
        .await
        .expect_err("an unrecognised history is not this build's history");

    let DbError::UnknownMigration { version } = refusal else {
        panic!("the refusal must name the version, got: {refusal:?}");
    };
    assert_eq!(version, 2_017);
    let reason = refusal.to_string();
    assert!(
        reason.contains("2017") && reason.contains("import-v2"),
        "the message has to give the operator both the version and the \
         supported way to carry an identity across, got: {reason}"
    );
}

/// The refusal must cost nothing: it happens before the migrator runs, so the
/// database a refused open left behind is byte-identical to the one it was
/// handed. A history check that ran after a migration would let a file this
/// build does not recognise be written to on the way to being refused.
#[tokio::test]
async fn a_refused_history_is_refused_without_adding_a_row_of_its_own() {
    let fixture = HistoryFixture::new("no-write");
    let database = fixture.with_history_row(2_017, 0).await;
    let before: i64 = sqlx::query_scalar(AssertSqlSafe("SELECT COUNT(*) FROM _sqlx_migrations"))
        .fetch_one(database.pool())
        .await
        .expect("a count");

    assert!(
        db::open(&DatabaseLocation::SqliteFile(fixture.path.clone()))
            .await
            .is_err()
    );

    let after: i64 = sqlx::query_scalar(AssertSqlSafe("SELECT COUNT(*) FROM _sqlx_migrations"))
        .fetch_one(database.pool())
        .await
        .expect("a count");
    assert_eq!(
        before, after,
        "the refusal has to leave the file it refused exactly as it found it"
    );
}

/// The control for the rule above, in the only form that matters operationally:
/// the history a healthy install actually has opens, every time. A guard that
/// failed here would be a coordinator that cannot restart.
#[tokio::test]
async fn the_history_this_build_wrote_is_admitted_and_keeps_booting() {
    let fixture = HistoryFixture::new("healthy");
    db::open(&DatabaseLocation::SqliteFile(fixture.path.clone()))
        .await
        .expect("a fresh file migrates and opens");
    for _ in 0..3 {
        db::open(&DatabaseLocation::SqliteFile(fixture.path.clone()))
            .await
            .expect(
                "a migrated file re-opens, because a restart is not a \
                     migration",
            );
    }
    let versions: Vec<i64> =
        sqlx::query_scalar(AssertSqlSafe("SELECT version FROM _sqlx_migrations"))
            .fetch_all(
                db::open(&DatabaseLocation::SqliteFile(fixture.path.clone()))
                    .await
                    .expect("open")
                    .pool(),
            )
            .await
            .expect("the history");
    assert_eq!(
        versions,
        embedded_versions().into_iter().collect::<Vec<i64>>(),
        "a file this build migrated records exactly the versions it embeds"
    );
}

/// THE 2026 RETIREMENT, expressed. A migration that shipped and was then removed
/// leaves a row in every database old enough to have applied it; that row is a
/// fact about the file, not damage, so the open succeeds and the rest of the
/// chain still applies. `db::RETIRED_MIGRATIONS` is empty today because the
/// embedded set is one checksum-frozen squashed migration, so the decision is
/// exercised here against an explicit list — and the list a build ships is
/// passed to the same function `db::open` passes its own to, rather than to a
/// second copy of the rule.
#[test]
fn a_declared_retirement_is_not_corruption_and_the_rest_of_the_chain_still_applies() {
    let embedded: HashSet<i64> = HashSet::from([1, 2, 3]);
    // v2's shape: the retired version sorts BEFORE the migration that reused its
    // slot, so anything comparing the raw history by position diverges here.
    let applied = [1_i64, 2, 3, 4];
    let retired = [4_i64];

    db::validate_migration_history(&applied, &embedded, &retired)
        .expect("a declared retirement is a fact about the file, not damage");
}

/// The control for that, and the safety property the allowlist exists to
/// protect: an undeclared extra row is still refused. Without it the retirement
/// list is a switch that turns the whole check off.
#[test]
fn an_undeclared_history_row_is_still_refused() {
    let embedded: HashSet<i64> = HashSet::from([1, 2, 3]);
    let applied = [1_i64, 2, 3, 9];

    let refusal = db::validate_migration_history(&applied, &embedded, &[])
        .expect_err("a row nothing declared is a row this build did not write");
    assert!(matches!(refusal, DbError::UnknownMigration { version: 9 }));
}

/// Declaring a retirement for a migration the build STILL embeds would make the
/// declaration a no-op that reads as deliberate, so it is refused. The cost of
/// this being wrong is a coordinator that will not start, which is the right
/// direction: the mistake is a stale list, and a stale list must not be able to
/// switch off the guard it is supposed to be standing in for.
#[test]
fn a_retirement_naming_a_still_embedded_migration_is_refused() {
    let embedded: HashSet<i64> = HashSet::from([1, 2, 3]);

    let refusal = db::validate_migration_history(&[1, 2, 3], &embedded, &[3])
        .expect_err("a migration that is still here has not been retired");
    assert!(matches!(refusal, DbError::UnknownMigration { version: 3 }));
}

/// The shipped list has to be honest about the embedded set, checked where the
/// embedded set is read rather than in a test that could drift from it. An empty
/// list is correct today and stays correct for as long as the schema is one
/// checksum-frozen squashed migration; the check is what makes adding a version
/// to it a deliberate act instead of a guess.
#[test]
fn the_shipped_retirement_list_and_the_embedded_set_do_not_overlap() {
    db::validate_migration_history(&[], &embedded_versions(), RETIRED_MIGRATIONS)
        .expect("nothing this build embeds may be declared retired");
}
