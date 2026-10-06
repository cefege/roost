//! `roost import-v2` against real SQLite files: that the device filter really
//! excludes the machine keys, that a re-run really applies a revocation, that
//! a target holding another install is refused, that a dry run really writes
//! nothing, and that the source really cannot be written to.
//!
//! The fixture is `tests/import_v2_fixture`, built from the coordinator's own
//! migration rather than a hand-written schema. A fixture with its own schema
//! would agree with itself and prove nothing about the file this command
//! actually has to read, and the whole risk in an import is a schema that is
//! not the one you assumed.
//!
//! Everything here is a temporary directory that removes itself, and the "v2
//! database" is a file the test built. Nothing reads the operator's real
//! coordinator database.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod import_v2_fixture;

use import_v2_fixture::{Fixture, report_for};
use roost_cli::import_v2::copy;
use roost_cli::import_v2::plan::ImportMode;
use roost_cli::import_v2::{CoordinatorState, coordinator_probe_argv, coordinator_state_with};
use roost_host::HostPlatform;

const MACHINE_KEYS: [&str; 2] = ["machine-one", "machine-two"];

/// The property the whole command exists for: a paired browser's key arrives,
/// and a machine's key does not.
#[tokio::test]
async fn a_first_run_carries_the_browser_and_leaves_the_machines_behind() {
    let fixture = Fixture::new("first-run").await;
    let (mode, reports) = fixture.import().await;

    assert_eq!(mode, ImportMode::FirstRun, "an empty target is a first run");
    let keys = report_for(&reports, "authorized_keys");
    assert_eq!(
        (keys.copied, keys.already_present),
        (1, 0),
        "one paired browser and NOT the two machine keys: this report is what the operator reads \
         to decide whether the import did what they meant"
    );

    assert_eq!(fixture.count_v3("SELECT count(*) FROM accounts").await, 1);
    assert_eq!(
        fixture
            .count_v3("SELECT count(*) FROM account_devices")
            .await,
        1
    );
    assert_eq!(
        fixture.count_v3("SELECT count(*) FROM app_settings").await,
        1
    );
    assert_eq!(
        fixture
            .count_v3("SELECT count(*) FROM authorized_key_revocations")
            .await,
        1,
        "a revocation is carried even though the key it covers is not"
    );
    for machine in MACHINE_KEYS {
        assert_eq!(
            fixture
                .count_v3(&format!(
                    "SELECT count(*) FROM authorized_keys WHERE label = '{machine}'"
                ))
                .await,
            0,
            "{machine} is a worker key, not a paired browser: importing it would enrol an \
             authenticator no human paired and no browser can present"
        );
    }
    assert_eq!(
        fixture
            .count_v3("SELECT count(*) FROM authorized_keys WHERE label = 'paired-browser'")
            .await,
        1,
    );
}

/// A re-run during the cutover window has to carry a browser paired on v2 after
/// the first import, and must not undo anything v3 decided in between.
#[tokio::test]
async fn a_re_run_carries_a_browser_paired_later_and_overwrites_nothing() {
    let fixture = Fixture::new("re-run").await;
    fixture.import().await;

    // v3 changes a setting, as an operator would while the cutover is running.
    fixture
        .on_v3("UPDATE app_settings SET value = 'codex', updated_at_ms = 99")
        .await;
    let later = fixture.pair_another_browser().await;

    let (mode, reports) = fixture.import().await;

    assert_eq!(mode, ImportMode::Refresh, "the same account is a refresh");
    let keys = report_for(&reports, "authorized_keys");
    assert_eq!(
        (keys.copied, keys.already_present),
        (1, 1),
        "the browser paired during the window arrives; the one already there is reported, not \
         rewritten"
    );
    assert_eq!(
        fixture
            .count_v3("SELECT count(*) FROM account_devices")
            .await,
        2
    );
    assert_eq!(
        fixture
            .count_v3(&format!(
                "SELECT count(*) FROM authorized_keys WHERE fingerprint = '{later}'"
            ))
            .await,
        1,
        "the browser paired after the first import is paired in v3 now"
    );
    assert_eq!(
        fixture
            .count_v3("SELECT count(*) FROM app_settings WHERE value = 'codex'")
            .await,
        1,
        "v3's own edit WINS: app_settings holds the Deepgram key and the VAPID pair, and an \
         import that reverted one would undo a decision the operator made after the first run"
    );
}

/// A revocation that arrived on v2 has to be applied, not merely recorded.
#[tokio::test]
async fn a_re_run_revokes_a_key_the_target_still_holds() {
    let fixture = Fixture::new("revocation").await;
    fixture.import().await;
    let later = fixture.pair_another_browser().await;
    fixture.import().await;
    assert_eq!(
        fixture
            .count_v3(&format!(
                "SELECT count(*) FROM authorized_keys WHERE fingerprint = '{later}'"
            ))
            .await,
        1,
        "precondition: the key is live in v3 before it is revoked on v2"
    );

    fixture.revoke_on_v2(&later).await;
    let (_, reports) = fixture.import().await;

    assert_eq!(
        report_for(&reports, "authorized_key_revocations").copied,
        1,
        "the revocation the target did not have is carried"
    );
    assert_eq!(
        fixture
            .count_v3(&format!(
                "SELECT count(*) FROM authorized_keys WHERE fingerprint = '{later}'"
            ))
            .await,
        0,
        "a revoked key is removed, not merely accompanied by a tombstone: it could otherwise be \
         presented and the revocation would be decorative"
    );
    assert_eq!(
        fixture
            .count_v3(&format!(
                "SELECT count(*) FROM account_devices WHERE fingerprint = '{later}'"
            ))
            .await,
        0,
        "and the device row goes with it, or the fleet would show a paired browser that can never \
         be presented"
    );
}

/// A target holding a DIFFERENT single install is refused by name. Not merged,
/// not overwritten: the account is what every other imported row refers to.
#[tokio::test]
async fn a_target_belonging_to_another_install_is_refused() {
    let fixture = Fixture::new("other-install").await;
    // A v3 database that already holds somebody else's one account. This is
    // NOT the two-account case: a target with two accounts is refused as "not a
    // v3 install" and names no account, because there is no single install whose
    // identity the operator might have meant.
    let database = roost_coord::db::open(&roost_host::DatabaseLocation::SqliteFile(
        fixture.v3.clone(),
    ))
    .await
    .expect("the target opens and migrates");
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        "INSERT INTO accounts (id, email_normalized, status, created_at_ms) \
         VALUES ('acct-other', 'other@roost.test', 'active', 1)"
            .to_string(),
    ))
    .execute(database.pool())
    .await
    .expect("the other install's account");

    let failure = fixture.import_refused().await;
    assert_eq!(failure.code, roost_cli::command_error::REJECTED_INVOCATION);
    assert!(
        failure.message.contains("acct-other") && failure.message.contains("acct-import"),
        "the refusal names both accounts: {}",
        failure.message
    );
}

/// A target with more than one account is not a v3 install at all, and saying
/// so is different from naming an account to choose between.
#[tokio::test]
async fn a_target_that_is_not_a_v3_install_is_refused_without_naming_an_account() {
    let fixture = Fixture::new("two-accounts").await;
    fixture.import().await;
    fixture
        .on_v3(
            "INSERT INTO accounts (id, email_normalized, status, created_at_ms) \
             VALUES ('acct-other', 'other@roost.test', 'active', 1)",
        )
        .await;

    let failure = fixture.import_refused().await;
    assert!(
        failure.message.contains("holds 2 accounts") && failure.message.contains("single"),
        "two accounts is a state v3 does not have, and the refusal says which: {}",
        failure.message
    );
}

/// The dry run's whole claim is that it writes nothing, and the only thing that
/// can see a write is the file's bytes.
#[tokio::test]
async fn a_dry_run_writes_nothing_and_reports_what_a_real_run_would_do() {
    let fixture = Fixture::new("dry-run").await;
    let estimate = copy::estimate(&fixture.v2, &fixture.v3)
        .await
        .expect("a dry run against a machine with no v3 install");
    assert!(
        !fixture.v3.exists(),
        "a dry run must not create the database it is reporting on"
    );

    let (_, applied) = fixture.import().await;
    assert_eq!(
        estimate, applied,
        "the dry run's numbers are the real run's numbers, not an estimate of them: a run inserts \
         exactly the rows the target did not have"
    );

    // And against a target that exists, a dry run leaves it byte-identical.
    // The snapshot is taken AFTER every connection to it is closed, which is
    // the whole point: SQLite checkpoints the WAL and writes the main file
    // when the last connection closes, so a file can be byte-identical when a
    // function returns and different a moment later. An assertion that does
    // not say when it snapshots is a race with a timer on it.
    fixture.close_target().await;
    let before = std::fs::read(&fixture.v3).expect("the target is readable");
    let second = copy::estimate(&fixture.v2, &fixture.v3)
        .await
        .expect("a dry run against a live target");
    fixture.close_target().await;
    assert_eq!(
        std::fs::read(&fixture.v3).expect("the target is readable"),
        before,
        "a dry run that changed a byte of the database is not a dry run"
    );
    assert_eq!(
        second.iter().map(|line| line.copied).sum::<i64>(),
        0,
        "a dry run against an already-imported target has nothing left to copy, and says so \
         rather than reporting the rows it would have written"
    );
}

/// A dry run must not MIGRATE the target it is reporting on, and the only
/// thing that proves that is a target that has no schema yet.
///
/// `roost_coord::db::open` runs the coordinator's migrations. A dry run that
/// reached the target through it would create the very tables it promised not
/// to touch — on a machine where an operator ran `--dry-run` to look before
/// leaping, and the leap was the migration. The non-existent-target case is
/// already covered above; this is the case where the file IS there and is
/// still a stranger to this product.
#[tokio::test]
async fn a_dry_run_does_not_migrate_a_target_that_is_already_there() {
    let fixture = Fixture::new("no-migrate").await;
    fixture.empty_target().await;
    fixture.close_target().await;

    let reports = copy::estimate(&fixture.v2, &fixture.v3)
        .await
        .expect("a dry run against an unmigrated target");
    assert_eq!(
        reports.iter().map(|line| line.copied).sum::<i64>(),
        // One account, one organization, one owner membership, one dashboard,
        // one dashboard membership, the ONE paired-browser key, its device row,
        // one revocation and one setting: nine rows, which is every row this
        // fixture holds. The two machine keys are not among them, because a
        // target with nothing in it is a first run and a first run filters too.
        9,
        "it still REPORTS what a real run would copy, which is the half of --dry-run that is \
         about telling the operator rather than about touching"
    );

    fixture.close_target().await;
    let tables: i64 = fixture
        .count_v3("SELECT count(*) FROM sqlite_master WHERE type = 'table'")
        .await;
    assert_eq!(
        tables, 0,
        "the target still has no tables: the dry run reported without running a single \
         migration against it"
    );
}

/// The cutover imports BEFORE `roost quickstart` creates anything, so on a host
/// that has never had v3 the directory the database belongs in does not exist.
/// SQLite creates a missing file but not a missing directory, and the import
/// refused with "unable to open database file" on exactly that host.
#[tokio::test]
async fn a_first_import_on_a_host_without_v3_creates_the_data_directory() {
    let fixture = Fixture::new("fresh-host").await;
    let target = fixture
        .root
        .join("RoostCoordinatorV3")
        .join("coordinator_v3.db");

    let applied = roost_cli::import_v2::apply(&fixture.v2, &target, 1_700_000_000_000)
        .await
        .expect("an import into a data directory nothing has created yet");

    assert_eq!(applied.mode, ImportMode::FirstRun);
    assert_eq!(
        fixture
            .count(&target, "SELECT count(*) FROM accounts")
            .await,
        1,
        "the account landed in the database the coordinator will open"
    );
}

/// The source is the live v2 coordinator's file on the same host during the
/// cutover, so it is attached read-only rather than trusted.
#[tokio::test]
async fn the_source_cannot_be_written_through_the_import() {
    let fixture = Fixture::new("read-only").await;
    let pool = Fixture::open(&fixture.v2).await;
    copy::attach(&pool, &fixture.v2)
        .await
        .expect("the source attaches for the probe");
    let written = sqlx::raw_sql(roost_cli_tests_sql_safe("DELETE FROM src.accounts"))
        .execute(&pool)
        .await;
    assert!(
        written.is_err(),
        "the attached source must be read-only: a bug in the statement list would otherwise \
         write to the database of the product being replaced, which is live on this host"
    );
    pool.close().await;
    assert_eq!(
        fixture.count_v2("SELECT count(*) FROM accounts").await,
        1,
        "and the v2 account is still there, so the refusal was SQLite's and not the test's"
    );
}

/// The `AssertSqlSafe` audit, for one test-local statement.
fn roost_cli_tests_sql_safe(statement: &str) -> sqlx::AssertSqlSafe<String> {
    sqlx::AssertSqlSafe(statement.to_string())
}

/// The command refuses to rewrite a database a running coordinator holds, and
/// refuses ONLY in that state.
#[test]
fn a_running_coordinator_is_the_only_state_that_refuses_the_import() {
    let active = |_: &[String]| Some(String::from("active\n"));
    assert_eq!(
        coordinator_state_with(HostPlatform::Linux, "roost3-coord", active),
        CoordinatorState::Running,
    );
    for answer in ["inactive\n", "failed\n", ""] {
        let answered = move |_: &[String]| Some(String::from(answer));
        assert_eq!(
            coordinator_state_with(HostPlatform::Linux, "roost3-coord", answered),
            CoordinatorState::Stopped,
            "only an explicit `active` is a running coordinator: {answer:?}"
        );
    }
    assert_eq!(
        coordinator_state_with(HostPlatform::Linux, "roost3-coord", |_| None),
        CoordinatorState::Unavailable,
        "a service manager that could not be asked is not a coordinator holding the database"
    );
    assert_eq!(
        coordinator_probe_argv(HostPlatform::Linux, "roost3-coord"),
        vec!["systemctl", "--user", "is-active", "roost3-coord"],
        "argv, not a shell string: a unit name can come from the environment"
    );
}
